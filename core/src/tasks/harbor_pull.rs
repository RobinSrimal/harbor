//! Harbor pull loop
//!
//! Periodically pulls missed packets from Harbor Nodes for offline delivery.

use std::sync::Arc;
use std::time::Duration;

use iroh::Endpoint;
use rusqlite::Connection;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::data::{
    get_topics_for_member, add_topic_member_with_relay,
    remove_topic_member, mark_pulled, was_pulled, get_joined_at,
    get_blob, insert_blob, init_blob_sections, record_peer_can_seed,
    CHUNK_SIZE,
};
use crate::network::dht::DhtService;
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::send::topic_messages::TopicMessage;
use crate::security::{
    verify_and_decrypt_packet_with_mode, harbor_id_from_topic,
    VerificationMode, SendPacket,
};

use crate::protocol::{Protocol, IncomingMessage, ProtocolEvent};

impl Protocol {
    /// Run the Harbor pull loop
    pub(crate) async fn run_harbor_pull_loop(
        db: Arc<Mutex<Connection>>,
        endpoint: Endpoint,
        event_tx: mpsc::Sender<ProtocolEvent>,
        our_id: [u8; 32],
        dht_service: Option<Arc<DhtService>>,
        running: Arc<RwLock<bool>>,
        pull_interval: Duration,
        pull_max_nodes: usize,
        pull_early_stop: usize,
        connect_timeout: Duration,
        response_timeout: Duration,
    ) {
        loop {
            // Check if we should stop
            if !*running.read().await {
                break;
            }

            tokio::time::sleep(pull_interval).await;

            // Get all topics we're subscribed to
            let topics = {
                let db_lock = db.lock().await;
                match get_topics_for_member(&db_lock, &our_id) {
                    Ok(t) => t,
                    Err(e) => {
                        error!(error = %e, "failed to get topics");
                        continue;
                    }
                }
            };

            for topic_id in topics {
                let harbor_id = harbor_id_from_topic(&topic_id);

                // Find Harbor Nodes for this topic
                let harbor_nodes = Self::find_harbor_nodes(&dht_service, &harbor_id).await;

                if harbor_nodes.is_empty() {
                    continue;
                }

                // Get packet IDs we already have and our join timestamp
                let (already_have, joined_at): (Vec<[u8; 32]>, i64) = {
                    let db_lock = db.lock().await;
                    let already_have = crate::data::harbor::get_pulled_packet_ids(&db_lock, &topic_id)
                        .unwrap_or_default();
                    let joined_at = get_joined_at(&db_lock, &topic_id).unwrap_or(0);
                    (already_have, joined_at)
                };

                // Create PullRequest - only get packets sent after we joined
                let pull_req = crate::network::harbor::protocol::PullRequest {
                    harbor_id,
                    recipient_id: our_id,
                    since_timestamp: joined_at, // Only get packets sent after we joined
                    already_have,
                    relay_url: None, // Will be tracked by Harbor Node from connection
                };

                // Try multiple Harbor Nodes - different nodes may have different packets
                // Stop after consecutive empty responses (configurable)
                let mut consecutive_empty = 0;
                for harbor_node in harbor_nodes.iter().take(pull_max_nodes) {
                    match Self::send_harbor_pull(&endpoint, harbor_node, &pull_req, connect_timeout, response_timeout).await {
                        Ok(packets) => {
                            if packets.is_empty() {
                                consecutive_empty += 1;
                                if consecutive_empty >= pull_early_stop {
                                    // Multiple nodes in a row had nothing, likely no more packets available
                                    break;
                                }
                                continue;
                            }

                            // Got packets, reset counter
                            consecutive_empty = 0;

                            debug!(
                                topic = hex::encode(topic_id),
                                count = packets.len(),
                                harbor_node = hex::encode(harbor_node),
                                "pulled packets from Harbor"
                            );

                            // Process each pulled packet
                            for packet_info in packets {
                                // Check if we already processed this
                                {
                                    let db_lock = db.lock().await;
                                    if was_pulled(&db_lock, &topic_id, &packet_info.packet_id).unwrap_or(true) {
                                        continue;
                                    }
                                }

                                // Parse and verify the packet
                                let send_packet = match SendPacket::from_bytes(&packet_info.packet_data) {
                                    Ok(p) => p,
                                    Err(e) => {
                                        debug!(error = %e, "failed to parse pulled packet");
                                        continue;
                                    }
                                };

                                // Determine verification mode from stored packet type
                                let mode = match packet_info.packet_type {
                                    HarborPacketType::Join => VerificationMode::MacOnly,
                                    HarborPacketType::Content | HarborPacketType::Leave => VerificationMode::Full,
                                };

                                // Verify and decrypt
                                match verify_and_decrypt_packet_with_mode(&send_packet, &topic_id, mode) {
                                    Ok(plaintext) => {
                                        // Mark as pulled
                                        {
                                            let db_lock = db.lock().await;
                                            let _ = mark_pulled(&db_lock, &topic_id, &packet_info.packet_id);
                                        }

                                        // Handle control messages
                                        let topic_msg = TopicMessage::decode(&plaintext).ok();
                                        if let Some(ref msg) = topic_msg {
                                            let db_lock = db.lock().await;
                                            match msg {
                                                TopicMessage::Join(join) => {
                                                    // Validate joiner matches packet sender
                                                    if join.joiner != packet_info.sender_id {
                                                        warn!(
                                                            joiner = %hex::encode(join.joiner),
                                                            sender = %hex::encode(packet_info.sender_id),
                                                            "join message joiner doesn't match packet sender - ignoring"
                                                        );
                                                    } else {
                                                        let _ = add_topic_member_with_relay(
                                                            &db_lock,
                                                            &topic_id,
                                                            &join.joiner,
                                                            join.relay_url.as_deref(),
                                                        );
                                                    }
                                                }
                                                TopicMessage::Leave(leave) => {
                                                    // Validate leaver matches packet sender (Tier 1: self-leave only)
                                                    if leave.leaver != packet_info.sender_id {
                                                        warn!(
                                                            leaver = %hex::encode(leave.leaver),
                                                            sender = %hex::encode(packet_info.sender_id),
                                                            "leave message leaver doesn't match packet sender - ignoring"
                                                        );
                                                    } else {
                                                        let _ = remove_topic_member(&db_lock, &topic_id, &leave.leaver);
                                                    }
                                                }
                                                TopicMessage::FileAnnouncement(ann) => {
                                                    // Validate source matches packet sender
                                                    if ann.source_id != packet_info.sender_id {
                                                        warn!(
                                                            source = %hex::encode(ann.source_id),
                                                            sender = %hex::encode(packet_info.sender_id),
                                                            "file announcement source doesn't match packet sender - ignoring"
                                                        );
                                                    } else {
                                                        // Check if we already have this blob
                                                        let existing = get_blob(&db_lock, &ann.hash);
                                                        if existing.is_ok() && existing.as_ref().unwrap().is_none() {
                                                            // Store blob metadata
                                                            if let Err(e) = insert_blob(
                                                                &db_lock,
                                                                &ann.hash,
                                                                &topic_id,
                                                                &ann.source_id,
                                                                &ann.display_name,
                                                                ann.total_size,
                                                                ann.num_sections,
                                                            ) {
                                                                warn!(error = %e, "failed to store blob metadata");
                                                            } else {
                                                                let total_chunks = ((ann.total_size + CHUNK_SIZE - 1) 
                                                                    / CHUNK_SIZE) as u32;
                                                                let _ = init_blob_sections(
                                                                    &db_lock,
                                                                    &ann.hash,
                                                                    ann.num_sections,
                                                                    total_chunks,
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                                TopicMessage::CanSeed(can_seed) => {
                                                    // Validate seeder matches packet sender
                                                    if can_seed.seeder_id != packet_info.sender_id {
                                                        warn!(
                                                            seeder = %hex::encode(can_seed.seeder_id),
                                                            sender = %hex::encode(packet_info.sender_id),
                                                            "can seed message seeder doesn't match packet sender - ignoring"
                                                        );
                                                    } else {
                                                        let _ = record_peer_can_seed(
                                                            &db_lock,
                                                            &can_seed.hash,
                                                            &can_seed.seeder_id,
                                                        );
                                                    }
                                                }
                                                TopicMessage::Content(_) => {}
                                                TopicMessage::SyncUpdate(sync_update) => {
                                                    // Emit SyncUpdate event for app to handle
                                                    let event = ProtocolEvent::SyncUpdate(crate::protocol::SyncUpdateEvent {
                                                        topic_id,
                                                        sender_id: packet_info.sender_id,
                                                        data: sync_update.data.clone(),
                                                    });
                                                    let _ = event_tx.send(event).await;
                                                }
                                                TopicMessage::SyncRequest => {
                                                    // Emit SyncRequest event for app to handle
                                                    let event = ProtocolEvent::SyncRequest(crate::protocol::SyncRequestEvent {
                                                        topic_id,
                                                        sender_id: packet_info.sender_id,
                                                    });
                                                    let _ = event_tx.send(event).await;
                                                }
                                            }
                                        }

                                        // Only forward Content messages to the app
                                        // Join/Leave/SyncUpdate are internal control messages, not user content
                                        if let Some(TopicMessage::Content(data)) = topic_msg {
                                            let event = ProtocolEvent::Message(IncomingMessage {
                                                topic_id,
                                                sender_id: packet_info.sender_id,
                                                payload: data,
                                                timestamp: packet_info.created_at,
                                            });

                                            if event_tx.send(event).await.is_err() {
                                                debug!("event receiver dropped");
                                            }
                                        }

                                        // Send ack to Harbor Node
                                        let ack = crate::network::harbor::protocol::DeliveryAck {
                                            packet_id: packet_info.packet_id,
                                            recipient_id: our_id,
                                        };
                                        let _ = Self::send_harbor_ack(&endpoint, harbor_node, &ack, connect_timeout).await;
                                    }
                                    Err(e) => {
                                        debug!(error = %e, "failed to verify pulled packet");
                                    }
                                }
                            }
                            // Continue to next node - it might have additional packets
                        }
                        Err(e) => {
                            debug!(
                                harbor_node = hex::encode(harbor_node),
                                error = %e,
                                "failed to pull from Harbor Node"
                            );
                            // Connection error doesn't count as "empty" - try next node
                        }
                    }
                }
            }
        }

        info!("Harbor pull loop stopped");
    }
}

