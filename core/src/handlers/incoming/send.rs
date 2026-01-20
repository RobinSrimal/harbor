//! Send protocol incoming handler
//!
//! Handles incoming Send protocol connections:
//! - Packet delivery (encrypted messages)
//! - Receipt acknowledgements
//! - PacketWithPoW (spam-protected delivery)

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, trace, warn};

use crate::protocol::sync::SyncManager;

use crate::data::{
    current_timestamp, get_topics_for_member, add_topic_member_with_relay,
    remove_topic_member, mark_pulled, get_blob, insert_blob, init_blob_sections,
    record_peer_can_seed,
};
use crate::data::send::acknowledge_and_cleanup_if_complete;
use crate::network::send::protocol::{SendMessage, Receipt};
use crate::network::membership::messages::TopicMessage;
use crate::security::{
    verify_and_decrypt_packet_with_mode, harbor_id_from_topic,
    VerificationMode,
};

use crate::protocol::{Protocol, IncomingMessage, ProtocolEvent, FileAnnouncedEvent, ProtocolError};

impl Protocol {
    /// Handle a single incoming Send protocol connection
    pub(crate) async fn handle_send_connection(
        conn: iroh::endpoint::Connection,
        db: Arc<Mutex<Connection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        sender_id: [u8; 32],
        our_id: [u8; 32],
        sync_managers: Arc<RwLock<HashMap<[u8; 32], SyncManager>>>,
    ) -> Result<(), ProtocolError> {
        // Maximum message size (512KB + overhead)
        const MAX_READ_SIZE: usize = 600 * 1024;

        trace!(sender = %hex::encode(sender_id), "waiting for streams");

        loop {
            // Accept unidirectional stream
            let mut recv = match conn.accept_uni().await {
                Ok(r) => r,
                Err(e) => {
                    // Connection closed
                    trace!(error = %e, sender = %hex::encode(sender_id), "stream accept ended");
                    break;
                }
            };

            trace!(sender = %hex::encode(sender_id), "stream received");

            // Read message with size limit
            let buf = match recv.read_to_end(MAX_READ_SIZE).await {
                Ok(data) => data,
                Err(e) => {
                    debug!(error = %e, "failed to read stream");
                    continue;
                }
            };

            trace!(bytes = buf.len(), "read from stream");

            // Decode message
            let message = match SendMessage::decode(&buf) {
                Ok(m) => m,
                Err(e) => {
                    debug!(error = %e, "failed to decode message");
                    continue;
                }
            };

            trace!("decoded SendMessage successfully");

            match message {
                SendMessage::Packet(packet) => {
                    Self::handle_packet(&conn, &db, &event_tx, sender_id, our_id, packet, &sync_managers).await?;
                }
                SendMessage::Receipt(receipt) => {
                    Self::handle_receipt(&db, receipt).await;
                }
                SendMessage::PacketWithPoW { packet, proof_of_work } => {
                    Self::handle_packet_with_pow(&conn, &db, &event_tx, sender_id, our_id, packet, proof_of_work, &sync_managers).await?;
                }
            }
        }

        Ok(())
    }

    /// Handle an incoming packet
    async fn handle_packet(
        conn: &iroh::endpoint::Connection,
        db: &Arc<Mutex<Connection>>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        sender_id: [u8; 32],
        our_id: [u8; 32],
        packet: crate::security::SendPacket,
        sync_managers: &Arc<RwLock<HashMap<[u8; 32], SyncManager>>>,
    ) -> Result<(), ProtocolError> {
        info!(harbor_id = %hex::encode(packet.harbor_id), sender = %hex::encode(&sender_id[..8]), "processing incoming packet");
        
        // Find which topic this packet belongs to
        let topics = {
            let db_lock = db.lock().await;
            get_topics_for_member(&db_lock, &our_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        info!(topic_count = topics.len(), "member of topics");

        // Try each topic we're subscribed to
        let mut processed = false;
        for topic_id in topics {
            let harbor_id = harbor_id_from_topic(&topic_id);
            if packet.harbor_id != harbor_id {
                continue;
            }
            info!(topic = %hex::encode(&topic_id[..8]), "found matching topic for packet");

            // Determine verification mode from payload prefix
            let mode = crate::network::membership::messages::get_verification_mode_from_payload(
                &packet.ciphertext
            ).unwrap_or(VerificationMode::Full);

            // Verify and decrypt
            match verify_and_decrypt_packet_with_mode(&packet, &topic_id, mode) {
                Ok(plaintext) => {
                    info!(plaintext_len = plaintext.len(), "packet decrypted successfully");
                    
                    // Parse topic message
                    let topic_msg = match TopicMessage::decode(&plaintext) {
                        Ok(msg) => {
                            info!(msg_type = ?msg.message_type(), "decoded TopicMessage");
                            Some(msg)
                        }
                        Err(e) => {
                            warn!(
                                topic = %hex::encode(&topic_id[..8]),
                                error = %e,
                                plaintext_len = plaintext.len(),
                                first_byte = ?plaintext.first(),
                                "failed to decode TopicMessage"
                            );
                            None
                        }
                    };

                    // Handle SyncUpdate separately (doesn't need db lock, uses sync_managers)
                    // Note: We check for manager existence rather than sync_enabled flag,
                    // since sync can be enabled per-topic at runtime via API
                    if let Some(TopicMessage::SyncUpdate(ref sync_msg)) = topic_msg {
                        info!(
                            topic = %hex::encode(&topic_id[..8]),
                            sender = %hex::encode(&sender_id[..8]),
                            size = sync_msg.data.len(),
                            "SYNC: received update from peer"
                        );
                        
                        // Apply to sync manager if one exists for this topic
                        let mut managers = sync_managers.write().await;
                        if let Some(manager) = managers.get_mut(&topic_id) {
                            let update = crate::network::sync::protocol::SyncUpdate {
                                data: sync_msg.data.clone(),
                            };
                            if let Err(e) = manager.apply_update(&update) {
                                warn!(error = %e, "SYNC: failed to apply update");
                            } else {
                                info!(
                                    topic = %hex::encode(&topic_id[..8]),
                                    sender = %hex::encode(&sender_id[..8]),
                                    size = sync_msg.data.len(),
                                    "SYNC: applied update from peer"
                                );
                                
                                // Emit event
                                let event = ProtocolEvent::SyncUpdated(crate::protocol::SyncUpdatedEvent {
                                    topic_id,
                                    sender_id,
                                    update_size: sync_msg.data.len(),
                                });
                                let _ = event_tx.send(event).await;
                            }
                        } else {
                            debug!(
                                topic = %hex::encode(&topic_id[..8]),
                                "SYNC: no manager for topic (sync not enabled for this topic)"
                            );
                        }
                        
                        // Mark packet as seen (dedup)
                        {
                            let db_lock = db.lock().await;
                            let _ = mark_pulled(&db_lock, &topic_id, &packet.packet_id);
                        }
                        
                        // Send receipt back
                        let receipt = Receipt::new(packet.packet_id, our_id);
                        let reply = SendMessage::Receipt(receipt);
                        if let Ok(mut send) = conn.open_uni().await {
                            let _ = tokio::io::AsyncWriteExt::write_all(&mut send, &reply.encode()).await;
                            let _ = send.finish();
                        }
                        
                        processed = true;
                        break;
                    }

                    // Handle control messages
                    if let Some(ref msg) = topic_msg {
                        let db_lock = db.lock().await;
                        match msg {
                            TopicMessage::Join(join) => {
                                // Validate joiner matches packet sender
                                if join.joiner != sender_id {
                                    warn!(
                                        joiner = %hex::encode(join.joiner),
                                        sender = %hex::encode(sender_id),
                                        "join message joiner doesn't match packet sender - ignoring"
                                    );
                                } else {
                                    // Add new member with relay URL for connectivity
                                    trace!(
                                        joiner = %hex::encode(join.joiner),
                                        relay = ?join.relay_url,
                                        topic = %hex::encode(topic_id),
                                        "received join message"
                                    );
                                    
                                    if let Err(e) = add_topic_member_with_relay(
                                        &db_lock,
                                        &topic_id,
                                        &join.joiner,
                                        join.relay_url.as_deref(),
                                    ) {
                                        warn!(error = %e, "failed to add member");
                                    }
                                    debug!(
                                        joiner = hex::encode(join.joiner),
                                        relay = ?join.relay_url,
                                        "member joined"
                                    );
                                }
                            }
                            TopicMessage::Leave(leave) => {
                                // Validate leaver matches packet sender (Tier 1: self-leave only)
                                if leave.leaver != sender_id {
                                    warn!(
                                        leaver = %hex::encode(leave.leaver),
                                        sender = %hex::encode(sender_id),
                                        "leave message leaver doesn't match packet sender - ignoring"
                                    );
                                } else {
                                    // Remove member
                                    if let Err(e) = remove_topic_member(&db_lock, &topic_id, &leave.leaver) {
                                        warn!(error = %e, "failed to remove member");
                                    }
                                    debug!(leaver = hex::encode(leave.leaver), "member left");
                                }
                            }
                            TopicMessage::FileAnnouncement(ann) => {
                                // Validate source matches packet sender
                                if ann.source_id != sender_id {
                                    warn!(
                                        source = %hex::encode(ann.source_id),
                                        sender = %hex::encode(sender_id),
                                        "file announcement source doesn't match packet sender - ignoring"
                                    );
                                } else {
                                    info!(
                                        hash = hex::encode(&ann.hash[..8]),
                                        source = hex::encode(&ann.source_id[..8]),
                                        name = ann.display_name,
                                        size = ann.total_size,
                                        chunks = ann.total_chunks,
                                        sections = ann.num_sections,
                                        "SHARE: Received file announcement"
                                    );
                                    
                                    // Check if we already have this blob
                                    let existing = get_blob(&db_lock, &ann.hash);
                                    if existing.is_ok() && existing.as_ref().unwrap().is_some() {
                                        debug!(
                                            hash = hex::encode(&ann.hash[..8]),
                                            "blob already known, skipping insert"
                                        );
                                    } else {
                                        // Store blob metadata - it will be in Partial state
                                        // The background share_pull task will pick it up
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
                                            // Initialize sections for tracking
                                            let total_chunks = ((ann.total_size + crate::data::CHUNK_SIZE - 1) 
                                                / crate::data::CHUNK_SIZE) as u32;
                                            if let Err(e) = init_blob_sections(
                                                &db_lock,
                                                &ann.hash,
                                                ann.num_sections,
                                                total_chunks,
                                            ) {
                                                warn!(error = %e, "failed to init blob sections");
                                            }
                                            debug!(
                                                hash = hex::encode(&ann.hash[..8]),
                                                "blob stored, will pull via background task"
                                            );
                                            
                                            // Emit FileAnnounced event to app
                                            let file_event = ProtocolEvent::FileAnnounced(FileAnnouncedEvent {
                                                topic_id,
                                                source_id: ann.source_id,
                                                hash: ann.hash,
                                                display_name: ann.display_name.clone(),
                                                total_size: ann.total_size,
                                                total_chunks,
                                                timestamp: current_timestamp(),
                                            });
                                            if event_tx.send(file_event).await.is_err() {
                                                debug!("event receiver dropped");
                                            }
                                        }
                                    }
                                }
                            }
                            TopicMessage::CanSeed(can_seed) => {
                                // Validate seeder matches packet sender
                                if can_seed.seeder_id != sender_id {
                                    warn!(
                                        seeder = %hex::encode(can_seed.seeder_id),
                                        sender = %hex::encode(sender_id),
                                        "can seed message seeder doesn't match packet sender - ignoring"
                                    );
                                } else {
                                    info!(
                                        hash = hex::encode(&can_seed.hash[..8]),
                                        peer = hex::encode(&can_seed.seeder_id[..8]),
                                        "SHARE: Peer can seed file"
                                    );
                                    
                                    // Record that this peer can seed all sections
                                    if let Err(e) = record_peer_can_seed(
                                        &db_lock,
                                        &can_seed.hash,
                                        &can_seed.seeder_id,
                                    ) {
                                        warn!(error = %e, "failed to record peer can seed");
                                    }
                                }
                            }
                            TopicMessage::Content(_) => {}
                            TopicMessage::SyncUpdate(_) => {} // Handled above, before db lock
                        }
                    }

                    // Only forward Content messages to the app
                    // Control messages (Join/Leave/FileAnnouncement/CanSeed/SyncUpdate) are internal
                    if let Some(TopicMessage::Content(data)) = topic_msg {
                        let event = ProtocolEvent::Message(IncomingMessage {
                            topic_id,
                            sender_id,
                            payload: data,
                            timestamp: current_timestamp(),
                        });

                        if event_tx.send(event).await.is_err() {
                            debug!("event receiver dropped");
                        }
                    }

                    // Mark packet as seen (dedup) so Harbor pull won't deliver it again
                    {
                        let db_lock = db.lock().await;
                        let _ = mark_pulled(&db_lock, &topic_id, &packet.packet_id);
                    }

                    // Send receipt back
                    let receipt = Receipt::new(packet.packet_id, our_id);
                    let reply = SendMessage::Receipt(receipt);
                    if let Ok(mut send) = conn.open_uni().await {
                        let _ = tokio::io::AsyncWriteExt::write_all(&mut send, &reply.encode()).await;
                        let _ = send.finish();
                    }

                    processed = true;
                    break;
                }
                Err(e) => {
                    trace!(error = %e, topic = %hex::encode(topic_id), "verification failed for topic");
                }
            }
        }

        if !processed {
            debug!(
                harbor_id = hex::encode(packet.harbor_id),
                "packet for unknown topic"
            );
        }

        Ok(())
    }

    /// Handle a receipt
    async fn handle_receipt(db: &Arc<Mutex<Connection>>, receipt: Receipt) {
        let db_lock = db.lock().await;
        match acknowledge_and_cleanup_if_complete(&db_lock, &receipt.packet_id, &receipt.sender) {
            Ok((true, true)) => {
                info!(
                    packet_id = hex::encode(receipt.packet_id),
                    from = hex::encode(receipt.sender),
                    "packet fully acknowledged - deleted from outgoing queue"
                );
            }
            Ok((true, false)) => {
                trace!(
                    packet_id = hex::encode(receipt.packet_id),
                    from = hex::encode(receipt.sender),
                    "receipt acknowledged (more recipients pending)"
                );
            }
            Ok((false, _)) => {
                trace!(
                    packet_id = hex::encode(receipt.packet_id),
                    "receipt already acknowledged"
                );
            }
            Err(e) => {
                debug!(error = %e, "failed to acknowledge receipt");
            }
        }
    }

    /// Handle a packet with proof of work
    async fn handle_packet_with_pow(
        conn: &iroh::endpoint::Connection,
        db: &Arc<Mutex<Connection>>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        sender_id: [u8; 32],
        our_id: [u8; 32],
        packet: crate::security::SendPacket,
        proof_of_work: crate::resilience::ProofOfWork,
        sync_managers: &Arc<RwLock<HashMap<[u8; 32], SyncManager>>>,
    ) -> Result<(), ProtocolError> {
        trace!(harbor_id = %hex::encode(packet.harbor_id), "processing packet with PoW");
        
        // Find which topic this packet belongs to
        let topics = {
            let db_lock = db.lock().await;
            get_topics_for_member(&db_lock, &our_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        // Try each topic we're subscribed to
        let mut processed = false;
        for topic_id in topics {
            let harbor_id = harbor_id_from_topic(&topic_id);
            if packet.harbor_id != harbor_id {
                continue;
            }

            // Verify PoW is bound to correct target (topic + us as recipient)
            let expected_target = crate::security::send_target_id(&topic_id, &our_id);
            if proof_of_work.harbor_id != expected_target {
                debug!(
                    expected = hex::encode(expected_target),
                    got = hex::encode(proof_of_work.harbor_id),
                    "PoW target mismatch"
                );
                continue;
            }

            // Verify PoW is bound to this packet
            if proof_of_work.packet_id != packet.packet_id {
                debug!("PoW packet_id mismatch");
                continue;
            }

            // Verify PoW meets requirements
            // TODO: Make PoW config configurable per Protocol instance
            let pow_config = crate::resilience::PoWConfig::default();
            let pow_result = crate::resilience::verify_pow(&proof_of_work, &pow_config);
            if !pow_result.is_valid() {
                debug!(result = %pow_result, "PoW verification failed");
                continue;
            }

            trace!(topic = %hex::encode(topic_id), "PoW verified");

            // Determine verification mode from payload prefix
            let mode = crate::network::membership::messages::get_verification_mode_from_payload(
                &packet.ciphertext
            ).unwrap_or(VerificationMode::Full);

            // Verify and decrypt
            match verify_and_decrypt_packet_with_mode(&packet, &topic_id, mode) {
                Ok(plaintext) => {
                    // Parse topic message
                    let topic_msg = TopicMessage::decode(&plaintext).ok();

                    // Handle SyncUpdate separately (doesn't need db lock, uses sync_managers)
                    // Note: We check for manager existence rather than sync_enabled flag,
                    // since sync can be enabled per-topic at runtime via API
                    if let Some(TopicMessage::SyncUpdate(ref sync_msg)) = topic_msg {
                        info!(
                            topic = %hex::encode(&topic_id[..8]),
                            sender = %hex::encode(&sender_id[..8]),
                            size = sync_msg.data.len(),
                            "SYNC: received update from peer (with PoW)"
                        );
                        
                        // Apply to sync manager if one exists for this topic
                        let mut managers = sync_managers.write().await;
                        if let Some(manager) = managers.get_mut(&topic_id) {
                            let update = crate::network::sync::protocol::SyncUpdate {
                                data: sync_msg.data.clone(),
                            };
                            if let Err(e) = manager.apply_update(&update) {
                                warn!(error = %e, "SYNC: failed to apply update (with PoW)");
                            } else {
                                info!(
                                    topic = %hex::encode(&topic_id[..8]),
                                    sender = %hex::encode(&sender_id[..8]),
                                    size = sync_msg.data.len(),
                                    "SYNC: applied update from peer (with PoW)"
                                );
                                
                                // Emit event
                                let event = ProtocolEvent::SyncUpdated(crate::protocol::SyncUpdatedEvent {
                                    topic_id,
                                    sender_id,
                                    update_size: sync_msg.data.len(),
                                });
                                let _ = event_tx.send(event).await;
                            }
                        } else {
                            debug!(
                                topic = %hex::encode(&topic_id[..8]),
                                "SYNC: no manager for topic (sync not enabled for this topic, with PoW)"
                            );
                        }
                        
                        // Mark packet as seen (dedup)
                        {
                            let db_lock = db.lock().await;
                            let _ = mark_pulled(&db_lock, &topic_id, &packet.packet_id);
                        }
                        
                        // Send receipt back
                        let receipt = Receipt::new(packet.packet_id, our_id);
                        let reply = SendMessage::Receipt(receipt);
                        if let Ok(mut send) = conn.open_uni().await {
                            let _ = tokio::io::AsyncWriteExt::write_all(&mut send, &reply.encode()).await;
                            let _ = send.finish();
                        }
                        
                        processed = true;
                        break;
                    }

                    // Handle control messages
                    if let Some(ref msg) = topic_msg {
                        let db_lock = db.lock().await;
                        match msg {
                            TopicMessage::Join(join) => {
                                // Validate joiner matches packet sender
                                if join.joiner != sender_id {
                                    warn!(
                                        joiner = %hex::encode(join.joiner),
                                        sender = %hex::encode(sender_id),
                                        "join message joiner doesn't match packet sender - ignoring"
                                    );
                                } else {
                                    if let Err(e) = add_topic_member_with_relay(
                                        &db_lock,
                                        &topic_id,
                                        &join.joiner,
                                        join.relay_url.as_deref(),
                                    ) {
                                        warn!(error = %e, "failed to add member");
                                    }
                                    debug!(
                                        joiner = hex::encode(join.joiner),
                                        relay = ?join.relay_url,
                                        "member joined"
                                    );
                                }
                            }
                            TopicMessage::Leave(leave) => {
                                // Validate leaver matches packet sender (Tier 1: self-leave only)
                                if leave.leaver != sender_id {
                                    warn!(
                                        leaver = %hex::encode(leave.leaver),
                                        sender = %hex::encode(sender_id),
                                        "leave message leaver doesn't match packet sender - ignoring"
                                    );
                                } else {
                                    if let Err(e) = remove_topic_member(&db_lock, &topic_id, &leave.leaver) {
                                        warn!(error = %e, "failed to remove member");
                                    }
                                    debug!(leaver = hex::encode(leave.leaver), "member left");
                                }
                            }
                            TopicMessage::FileAnnouncement(ann) => {
                                // Validate source matches packet sender
                                if ann.source_id != sender_id {
                                    warn!(
                                        source = %hex::encode(ann.source_id),
                                        sender = %hex::encode(sender_id),
                                        "file announcement source doesn't match packet sender - ignoring"
                                    );
                                } else {
                                    info!(
                                        hash = hex::encode(&ann.hash[..8]),
                                        source = hex::encode(&ann.source_id[..8]),
                                        name = ann.display_name,
                                        size = ann.total_size,
                                        chunks = ann.total_chunks,
                                        sections = ann.num_sections,
                                        "SHARE: Received file announcement"
                                    );
                                    
                                    // Check if we already have this blob
                                    let existing = get_blob(&db_lock, &ann.hash);
                                    if existing.is_ok() && existing.as_ref().unwrap().is_some() {
                                        debug!(
                                            hash = hex::encode(&ann.hash[..8]),
                                            "blob already known, skipping insert"
                                        );
                                    } else {
                                        // Store blob metadata - it will be in Partial state
                                        // The background share_pull task will pick it up
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
                                            // Initialize sections for tracking
                                            let total_chunks = ((ann.total_size + crate::data::CHUNK_SIZE - 1) 
                                                / crate::data::CHUNK_SIZE) as u32;
                                            if let Err(e) = init_blob_sections(
                                                &db_lock,
                                                &ann.hash,
                                                ann.num_sections,
                                                total_chunks,
                                            ) {
                                                warn!(error = %e, "failed to init blob sections");
                                            }
                                            debug!(
                                                hash = hex::encode(&ann.hash[..8]),
                                                "blob stored, will pull via background task"
                                            );
                                            
                                            // Emit FileAnnounced event to app
                                            let file_event = ProtocolEvent::FileAnnounced(FileAnnouncedEvent {
                                                topic_id,
                                                source_id: ann.source_id,
                                                hash: ann.hash,
                                                display_name: ann.display_name.clone(),
                                                total_size: ann.total_size,
                                                total_chunks,
                                                timestamp: current_timestamp(),
                                            });
                                            if event_tx.send(file_event).await.is_err() {
                                                debug!("event receiver dropped");
                                            }
                                        }
                                    }
                                }
                            }
                            TopicMessage::CanSeed(can_seed) => {
                                // Validate seeder matches packet sender
                                if can_seed.seeder_id != sender_id {
                                    warn!(
                                        seeder = %hex::encode(can_seed.seeder_id),
                                        sender = %hex::encode(sender_id),
                                        "can seed message seeder doesn't match packet sender - ignoring"
                                    );
                                } else {
                                    info!(
                                        hash = hex::encode(&can_seed.hash[..8]),
                                        peer = hex::encode(&can_seed.seeder_id[..8]),
                                        "SHARE: Peer can seed file"
                                    );
                                    
                                    // Record that this peer can seed all sections
                                    if let Err(e) = record_peer_can_seed(
                                        &db_lock,
                                        &can_seed.hash,
                                        &can_seed.seeder_id,
                                    ) {
                                        warn!(error = %e, "failed to record peer can seed");
                                    }
                                }
                            }
                            TopicMessage::Content(_) => {}
                            TopicMessage::SyncUpdate(_) => {} // Handled above, before db lock
                        }
                    }

                    // Only forward Content messages to the app
                    // Control messages (Join/Leave/FileAnnouncement/CanSeed/SyncUpdate) are internal
                    if let Some(TopicMessage::Content(data)) = topic_msg {
                        let event = ProtocolEvent::Message(IncomingMessage {
                            topic_id,
                            sender_id,
                            payload: data,
                            timestamp: current_timestamp(),
                        });

                        if event_tx.send(event).await.is_err() {
                            debug!("event receiver dropped");
                        }
                    }

                    // Mark packet as seen (dedup) so Harbor pull won't deliver it again
                    {
                        let db_lock = db.lock().await;
                        let _ = mark_pulled(&db_lock, &topic_id, &packet.packet_id);
                    }

                    // Send receipt back
                    let receipt = Receipt::new(packet.packet_id, our_id);
                    let reply = SendMessage::Receipt(receipt);
                    if let Ok(mut send) = conn.open_uni().await {
                        let _ = tokio::io::AsyncWriteExt::write_all(&mut send, &reply.encode()).await;
                        let _ = send.finish();
                    }

                    processed = true;
                    break;
                }
                Err(e) => {
                    trace!(error = %e, topic = hex::encode(topic_id), "verification failed for topic");
                }
            }
        }

        if !processed {
            debug!(
                harbor_id = hex::encode(packet.harbor_id),
                "PacketWithPoW for unknown topic or invalid PoW"
            );
        }

        Ok(())
    }
}

