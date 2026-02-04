//! Harbor pull loop
//!
//! Periodically pulls missed packets from Harbor Nodes for offline delivery.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::data::{
    get_topics_for_member, add_topic_member, update_peer_relay_url, current_timestamp,
    remove_topic_member, mark_pulled, was_pulled, get_joined_at,
    get_blob, insert_blob, init_blob_sections, record_peer_can_seed,
    CHUNK_SIZE,
};
use crate::network::harbor::HarborService;
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::send::topic_messages::TopicMessage;
use crate::security::{
    verify_and_decrypt_with_epoch,
    harbor_id_from_topic,
    verify_and_decrypt_dm_packet,
    VerificationMode, SendPacket, EpochKeys,
};
use crate::network::send::dm_messages::DmMessage;

use crate::network::stream::StreamService;
use crate::network::send::PacketSource;
use crate::network::control::protocol::{
    ControlPacketType, TopicInvite, Suggest, RemoveMember,
    ConnectRequest, ConnectAccept, ConnectDecline,
};
use crate::protocol::{Protocol, IncomingMessage, ProtocolEvent};

impl Protocol {
    /// Run the Harbor pull loop
    pub(crate) async fn run_harbor_pull_loop(
        harbor_service: Arc<HarborService>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        our_id: [u8; 32],
        running: Arc<RwLock<bool>>,
        pull_interval: Duration,
        pull_max_nodes: usize,
        pull_early_stop: usize,
        stream_service: Arc<StreamService>,
    ) {
        let db = harbor_service.db().clone();
        let dht_service = harbor_service.dht_service().clone();
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

            // === Pull DM packets (harbor_id = our endpoint_id) ===
            {
                let dm_harbor_id = our_id;
                let harbor_nodes = HarborService::find_harbor_nodes(&dht_service, &dm_harbor_id).await;

                if !harbor_nodes.is_empty() {
                    let already_have: Vec<[u8; 32]> = {
                        let db_lock = db.lock().await;
                        // Use our_id as "topic_id" for DM dedup tracking
                        crate::data::harbor::get_pulled_packet_ids(&db_lock, &our_id)
                            .unwrap_or_default()
                    };

                    let pull_req = crate::network::harbor::protocol::PullRequest {
                        harbor_id: dm_harbor_id,
                        recipient_id: our_id,
                        since_timestamp: 0,
                        already_have,
                        relay_url: None,
                    };

                    for harbor_node in harbor_nodes.iter().take(pull_max_nodes) {
                        match harbor_service.send_harbor_pull(harbor_node, &pull_req).await {
                            Ok(packets) => {
                                for packet_info in packets {
                                    {
                                        let db_lock = db.lock().await;
                                        if was_pulled(&db_lock, &our_id, &packet_info.packet_id).unwrap_or(true) {
                                            continue;
                                        }
                                    }

                                    let send_packet = match SendPacket::from_bytes(&packet_info.packet_data) {
                                        Ok(p) => p,
                                        Err(e) => {
                                            debug!(error = %e, "failed to parse pulled DM packet");
                                            continue;
                                        }
                                    };

                                    if !send_packet.is_dm() {
                                        debug!("pulled packet from DM harbor_id is not a DM packet, skipping");
                                        continue;
                                    }

                                    // Decrypt DM
                                    // We need the identity private key - get it from harbor_service context
                                    let private_key = {
                                        let db_lock = db.lock().await;
                                        match crate::data::identity::get_identity(&db_lock) {
                                            Ok(Some(id)) => id.private_key,
                                            _ => {
                                                debug!("no identity for DM decryption");
                                                continue;
                                            }
                                        }
                                    };

                                    match verify_and_decrypt_dm_packet(&send_packet, &private_key) {
                                        Ok(plaintext) => {
                                            {
                                                let db_lock = db.lock().await;
                                                let _ = mark_pulled(&db_lock, &our_id, &packet_info.packet_id);
                                            }

                                            // Check if this is a Control packet (type byte < 0x80)
                                            // or a DM packet (type byte >= 0x80)
                                            if !plaintext.is_empty() && plaintext[0] < 0x80 {
                                                // Control packet - parse type and data
                                                if let Some(ctrl_type) = ControlPacketType::from_byte(plaintext[0]) {
                                                    let ctrl_data = &plaintext[1..];
                                                    match ctrl_type {
                                                        ControlPacketType::TopicInvite => {
                                                            if let Ok(invite) = postcard::from_bytes::<TopicInvite>(ctrl_data) {
                                                                // Store as pending invite
                                                                {
                                                                    let db_lock = db.lock().await;
                                                                    let _ = crate::data::store_pending_invite(
                                                                        &db_lock,
                                                                        &invite.message_id,
                                                                        &invite.topic_id,
                                                                        &invite.sender_id,
                                                                        invite.topic_name.as_deref(),
                                                                        invite.epoch,
                                                                        &invite.epoch_key,
                                                                        &invite.admin_id,
                                                                        &invite.members,
                                                                    );
                                                                }
                                                                // Emit event
                                                                let member_count = invite.members.len();
                                                                let event = ProtocolEvent::TopicInviteReceived(
                                                                    crate::protocol::TopicInviteReceivedEvent {
                                                                        message_id: invite.message_id,
                                                                        topic_id: invite.topic_id,
                                                                        sender_id: invite.sender_id,
                                                                        topic_name: invite.topic_name.clone(),
                                                                        admin_id: invite.admin_id,
                                                                        member_count,
                                                                    }
                                                                );
                                                                let _ = event_tx.send(event).await;
                                                                info!(
                                                                    topic = %hex::encode(&invite.topic_id[..8]),
                                                                    sender = %hex::encode(&invite.sender_id[..8]),
                                                                    admin = %hex::encode(&invite.admin_id[..8]),
                                                                    "TOPIC_INVITE_RECEIVED via Harbor pull"
                                                                );
                                                            }
                                                        }
                                                        ControlPacketType::Suggest => {
                                                            if let Ok(suggest) = postcard::from_bytes::<Suggest>(ctrl_data) {
                                                                // Emit event
                                                                let event = ProtocolEvent::PeerSuggested(
                                                                    crate::protocol::PeerSuggestedEvent {
                                                                        introducer_id: suggest.sender_id,
                                                                        suggested_peer_id: suggest.suggested_peer,
                                                                        relay_url: suggest.relay_url,
                                                                        note: suggest.note,
                                                                    }
                                                                );
                                                                let _ = event_tx.send(event).await;
                                                                info!(
                                                                    suggested = %hex::encode(&suggest.suggested_peer[..8]),
                                                                    "PEER_SUGGESTED via Harbor pull"
                                                                );
                                                            }
                                                        }
                                                        ControlPacketType::ConnectRequest => {
                                                            if let Ok(req) = postcard::from_bytes::<ConnectRequest>(ctrl_data) {
                                                                // Store connection and emit event
                                                                {
                                                                    let db_lock = db.lock().await;
                                                                    let _ = crate::data::upsert_connection(
                                                                        &db_lock,
                                                                        &req.sender_id,
                                                                        crate::data::ConnectionState::PendingIncoming,
                                                                        req.display_name.as_deref(),
                                                                        req.relay_url.as_deref(),
                                                                        Some(&req.request_id),
                                                                    );
                                                                }
                                                                let event = ProtocolEvent::ConnectionRequest(
                                                                    crate::protocol::ConnectionRequestEvent {
                                                                        peer_id: req.sender_id,
                                                                        request_id: req.request_id,
                                                                        display_name: req.display_name,
                                                                        relay_url: req.relay_url,
                                                                    }
                                                                );
                                                                let _ = event_tx.send(event).await;
                                                            }
                                                        }
                                                        ControlPacketType::ConnectAccept => {
                                                            if let Ok(accept) = postcard::from_bytes::<ConnectAccept>(ctrl_data) {
                                                                {
                                                                    let db_lock = db.lock().await;
                                                                    let _ = crate::data::update_connection_state(
                                                                        &db_lock,
                                                                        &accept.sender_id,
                                                                        crate::data::ConnectionState::Connected,
                                                                    );
                                                                }
                                                                let event = ProtocolEvent::ConnectionAccepted(
                                                                    crate::protocol::ConnectionAcceptedEvent {
                                                                        peer_id: accept.sender_id,
                                                                        request_id: accept.request_id,
                                                                    }
                                                                );
                                                                let _ = event_tx.send(event).await;
                                                            }
                                                        }
                                                        ControlPacketType::ConnectDecline => {
                                                            if let Ok(decline) = postcard::from_bytes::<ConnectDecline>(ctrl_data) {
                                                                {
                                                                    let db_lock = db.lock().await;
                                                                    let _ = crate::data::update_connection_state(
                                                                        &db_lock,
                                                                        &decline.sender_id,
                                                                        crate::data::ConnectionState::Declined,
                                                                    );
                                                                }
                                                                let event = ProtocolEvent::ConnectionDeclined(
                                                                    crate::protocol::ConnectionDeclinedEvent {
                                                                        request_id: decline.request_id,
                                                                        peer_id: decline.sender_id,
                                                                        reason: decline.reason,
                                                                    }
                                                                );
                                                                let _ = event_tx.send(event).await;
                                                            }
                                                        }
                                                        ControlPacketType::RemoveMember => {
                                                            if let Ok(remove) = postcard::from_bytes::<RemoveMember>(ctrl_data) {
                                                                {
                                                                    let db_lock = db.lock().await;
                                                                    // Store new epoch key
                                                                    let _ = crate::data::store_epoch_key(
                                                                        &db_lock,
                                                                        &remove.topic_id,
                                                                        remove.new_epoch,
                                                                        &remove.new_epoch_key,
                                                                    );
                                                                    // Remove the member from our local list
                                                                    let _ = crate::data::remove_topic_member(
                                                                        &db_lock,
                                                                        &remove.topic_id,
                                                                        &remove.removed_member,
                                                                    );
                                                                }
                                                                let event = ProtocolEvent::TopicEpochRotated(
                                                                    crate::protocol::TopicEpochRotatedEvent {
                                                                        topic_id: remove.topic_id,
                                                                        new_epoch: remove.new_epoch,
                                                                        removed_member: remove.removed_member,
                                                                    }
                                                                );
                                                                let _ = event_tx.send(event).await;
                                                            }
                                                        }
                                                        // TopicJoin/TopicLeave are topic-scoped, not DM
                                                        _ => {
                                                            debug!(packet_type = ?ctrl_type, "ignoring unexpected control packet type in DM pull");
                                                        }
                                                    }
                                                } else {
                                                    debug!(type_byte = plaintext[0], "unknown control packet type byte");
                                                }

                                                // Send ack to Harbor Node
                                                let ack = crate::network::harbor::protocol::DeliveryAck {
                                                    packet_id: packet_info.packet_id,
                                                    recipient_id: our_id,
                                                };
                                                let _ = harbor_service.send_harbor_ack(harbor_node, &ack).await;
                                                continue;
                                            }

                                            // Regular DM message (0x80+ type byte)
                                            match DmMessage::decode(&plaintext) {
                                                Ok(DmMessage::Content(data)) => {
                                                    let event = ProtocolEvent::DmReceived(crate::protocol::DmReceivedEvent {
                                                        sender_id: packet_info.sender_id,
                                                        payload: data,
                                                        timestamp: packet_info.created_at,
                                                    });
                                                    let _ = event_tx.send(event).await;
                                                }
                                                Ok(DmMessage::SyncUpdate(data)) => {
                                                    let event = ProtocolEvent::DmSyncUpdate(crate::protocol::DmSyncUpdateEvent {
                                                        sender_id: packet_info.sender_id,
                                                        data,
                                                    });
                                                    let _ = event_tx.send(event).await;
                                                }
                                                Ok(DmMessage::SyncRequest) => {
                                                    let event = ProtocolEvent::DmSyncRequest(crate::protocol::DmSyncRequestEvent {
                                                        sender_id: packet_info.sender_id,
                                                    });
                                                    let _ = event_tx.send(event).await;
                                                }
                                                Ok(DmMessage::FileAnnouncement(msg)) => {
                                                    let event = ProtocolEvent::DmFileAnnounced(crate::protocol::DmFileAnnouncedEvent {
                                                        sender_id: packet_info.sender_id,
                                                        hash: msg.hash,
                                                        display_name: msg.display_name,
                                                        total_size: msg.total_size,
                                                        total_chunks: msg.total_chunks,
                                                        num_sections: msg.num_sections,
                                                        timestamp: packet_info.created_at,
                                                    });
                                                    let _ = event_tx.send(event).await;
                                                }
                                                // DM stream signaling — route to StreamService
                                                Ok(ref dm_msg @ DmMessage::StreamAccept(_))
                                                | Ok(ref dm_msg @ DmMessage::StreamReject(_))
                                                | Ok(ref dm_msg @ DmMessage::StreamQuery(_))
                                                | Ok(ref dm_msg @ DmMessage::StreamActive(_))
                                                | Ok(ref dm_msg @ DmMessage::StreamEnded(_))
                                                | Ok(ref dm_msg @ DmMessage::StreamRequest(_)) => {
                                                    stream_service.handle_dm_signaling(dm_msg, packet_info.sender_id).await;
                                                }
                                                Err(e) => {
                                                    debug!(error = %e, "failed to decode pulled DM message");
                                                }
                                            }

                                            let ack = crate::network::harbor::protocol::DeliveryAck {
                                                packet_id: packet_info.packet_id,
                                                recipient_id: our_id,
                                            };
                                            let _ = harbor_service.send_harbor_ack(harbor_node, &ack).await;
                                        }
                                        Err(e) => {
                                            debug!(error = %e, "failed to decrypt pulled DM packet");
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                debug!(error = %e, "failed to pull DMs from Harbor Node");
                            }
                        }
                    }
                }
            }

            // === Pull topic packets ===
            for topic_id in topics {
                let harbor_id = harbor_id_from_topic(&topic_id);

                // Find Harbor Nodes for this topic
                let harbor_nodes = HarborService::find_harbor_nodes(&dht_service, &harbor_id).await;

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
                    match harbor_service.send_harbor_pull(harbor_node, &pull_req).await {
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

                                // All topics have stored epoch keys (at least epoch 0)
                                // Look up the stored epoch key for the packet's epoch
                                let epoch_key_opt = {
                                    let db_lock = db.lock().await;
                                    crate::data::get_epoch_key(&db_lock, &topic_id, send_packet.epoch)
                                        .ok()
                                        .flatten()
                                };

                                let decrypt_result = match epoch_key_opt {
                                    Some(stored_key) => {
                                        // Derive encryption keys from stored epoch secret
                                        let epoch_keys = EpochKeys::derive_from_secret(
                                            send_packet.epoch,
                                            &stored_key.key_data,
                                        );
                                        verify_and_decrypt_with_epoch(&send_packet, &topic_id, &epoch_keys, mode)
                                    }
                                    None => {
                                        // No epoch key stored - member was likely removed
                                        warn!(
                                            topic = %hex::encode(&topic_id[..8]),
                                            epoch = send_packet.epoch,
                                            packet_id = %hex::encode(&packet_info.packet_id[..8]),
                                            "NO_EPOCH_KEY_FOR_EPOCH - cannot decrypt (member likely removed)"
                                        );
                                        continue;
                                    }
                                };

                                // Verify and decrypt
                                match decrypt_result {
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
                                                        let _ = add_topic_member(
                                                            &db_lock,
                                                            &topic_id,
                                                            &join.joiner,
                                                        );
                                                        // Store relay URL in peers table if provided
                                                        if let Some(ref relay_url) = join.relay_url {
                                                            let _ = update_peer_relay_url(
                                                                &db_lock,
                                                                &join.joiner,
                                                                relay_url,
                                                                current_timestamp(),
                                                            );
                                                        }
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
                                                                &topic_id, // scope_id: topic scopes the blob
                                                                &ann.source_id,
                                                                &ann.display_name,
                                                                ann.total_size,
                                                                ann.num_sections,
                                                            ) {
                                                                warn!(error = %e, "failed to store blob metadata");
                                                            } else {
                                                                let total_chunks = ann.total_size.div_ceil(CHUNK_SIZE) as u32;
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
                                                // Stream signaling — route to StreamService
                                                TopicMessage::StreamRequest(_) => {
                                                    drop(db_lock);
                                                    stream_service.handle_signaling(
                                                        msg, &topic_id, packet_info.sender_id,
                                                        PacketSource::HarborPull,
                                                    ).await;
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
                                        let _ = harbor_service.send_harbor_ack(harbor_node, &ack).await;
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

