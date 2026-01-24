//! Send Service - Incoming packet processing
//!
//! This module handles receiving and processing incoming packets:
//! - Decryption and verification
//! - TopicMessage parsing
//! - Control message handling (Join, Leave, FileAnnouncement, CanSeed)
//! - Sync message handling (SyncUpdate, SyncRequest)
//! - Event emission
//! - Deduplication marking
//! - Receipt generation

use std::sync::Arc;

use rusqlite::Connection as SqliteConnection;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, trace, warn};

use crate::data::{
    current_timestamp, add_topic_member_with_relay,
    remove_topic_member, mark_pulled, get_blob, insert_blob, init_blob_sections,
    record_peer_can_seed, CHUNK_SIZE,
};
use crate::data::send::acknowledge_receipt as db_acknowledge_receipt;
use crate::network::send::topic_messages::TopicMessage;
use crate::protocol::{
    ProtocolEvent, IncomingMessage, FileAnnouncedEvent, SyncUpdateEvent, SyncRequestEvent,
};
use crate::resilience::{PoWConfig, PoWVerifyResult, verify_pow};
use crate::security::{
    SendPacket, PacketError, verify_and_decrypt_packet, verify_and_decrypt_packet_with_mode,
    send_target_id, VerificationMode,
};
use super::protocol::Receipt;
use super::topic_messages::get_verification_mode_from_payload;

/// Error during incoming packet processing
#[derive(Debug)]
pub enum ProcessError {
    /// Decryption/verification failed (try next topic)
    VerificationFailed(String),
    /// Database error
    Database(String),
    /// Invalid message format
    InvalidMessage(String),
    /// Sender validation failed (e.g., joiner != sender)
    SenderMismatch(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessError::VerificationFailed(e) => write!(f, "verification failed: {}", e),
            ProcessError::Database(e) => write!(f, "database error: {}", e),
            ProcessError::InvalidMessage(e) => write!(f, "invalid message: {}", e),
            ProcessError::SenderMismatch(e) => write!(f, "sender mismatch: {}", e),
        }
    }
}

impl std::error::Error for ProcessError {}

/// Result of processing an incoming packet
#[derive(Debug)]
pub struct ProcessResult {
    /// The receipt to send back
    pub receipt: Receipt,
    /// Whether the packet was a Content message (should be forwarded to app)
    pub content_payload: Option<Vec<u8>>,
}

/// Error during PoW-protected receive operations
#[derive(Debug)]
pub enum ReceiveError {
    /// Failed to verify/decrypt packet
    PacketVerification(PacketError),
    /// Proof of Work required but not provided
    PoWRequired,
    /// Invalid Proof of Work
    InvalidPoW(PoWVerifyResult),
    /// PoW target mismatch (wrong topic or recipient)
    PoWTargetMismatch,
}

impl std::fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReceiveError::PacketVerification(e) => write!(f, "packet verification failed: {}", e),
            ReceiveError::PoWRequired => write!(f, "proof of work required"),
            ReceiveError::InvalidPoW(result) => write!(f, "invalid proof of work: {}", result),
            ReceiveError::PoWTargetMismatch => write!(f, "proof of work target mismatch"),
        }
    }
}

impl std::error::Error for ReceiveError {}

impl From<PacketError> for ReceiveError {
    fn from(e: PacketError) -> Self {
        ReceiveError::PacketVerification(e)
    }
}

/// Process an incoming packet - handles all business logic
///
/// This function:
/// 1. Decrypts the packet using the topic key
/// 2. Parses the TopicMessage
/// 3. Handles control messages (Join, Leave, FileAnnouncement, CanSeed)
/// 4. Handles sync messages (SyncUpdate, SyncRequest)
/// 5. Emits appropriate events
/// 6. Marks packet as seen (dedup)
/// 7. Returns a receipt and optional content payload
///
/// # Arguments
/// * `packet` - The incoming SendPacket
/// * `topic_id` - The topic this packet belongs to (already matched by handler)
/// * `sender_id` - The sender's EndpointID (from connection)
/// * `our_id` - Our EndpointID (for receipt)
/// * `db` - Database connection
/// * `event_tx` - Channel for emitting protocol events
///
/// # Returns
/// * `Ok(ProcessResult)` - Processing succeeded, includes receipt and optional content
/// * `Err(ProcessError::VerificationFailed)` - Decryption failed, handler should try next topic
/// * `Err(ProcessError::*)` - Other errors
pub async fn process_incoming_packet(
    packet: &SendPacket,
    topic_id: &[u8; 32],
    sender_id: [u8; 32],
    our_id: [u8; 32],
    db: &Arc<Mutex<SqliteConnection>>,
    event_tx: &mpsc::Sender<ProtocolEvent>,
) -> Result<ProcessResult, ProcessError> {
    // Determine verification mode from payload prefix
    let mode = get_verification_mode_from_payload(&packet.ciphertext)
        .unwrap_or(VerificationMode::Full);

    // Verify and decrypt
    let plaintext = verify_and_decrypt_packet_with_mode(packet, topic_id, mode)
        .map_err(|e| ProcessError::VerificationFailed(e.to_string()))?;

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

    // Handle SyncUpdate - emit event for app to handle
    if let Some(TopicMessage::SyncUpdate(ref sync_msg)) = topic_msg {
        info!(
            topic = %hex::encode(&topic_id[..8]),
            sender = %hex::encode(&sender_id[..8]),
            size = sync_msg.data.len(),
            "SYNC: received update from peer"
        );

        // Emit event for app to handle
        let event = ProtocolEvent::SyncUpdate(SyncUpdateEvent {
            topic_id: *topic_id,
            sender_id,
            data: sync_msg.data.clone(),
        });
        let _ = event_tx.send(event).await;

        // Mark packet as seen (dedup)
        {
            let db_lock = db.lock().await;
            let _ = mark_pulled(&db_lock, topic_id, &packet.packet_id);
        }

        return Ok(ProcessResult {
            receipt: Receipt::new(packet.packet_id, our_id),
            content_payload: None,
        });
    }

    // Handle SyncRequest - emit event for app to respond
    if let Some(TopicMessage::SyncRequest) = topic_msg {
        info!(
            topic = %hex::encode(&topic_id[..8]),
            sender = %hex::encode(&sender_id[..8]),
            "SYNC: received sync request from peer"
        );

        // Emit event for app to handle
        let event = ProtocolEvent::SyncRequest(SyncRequestEvent {
            topic_id: *topic_id,
            sender_id,
        });
        let _ = event_tx.send(event).await;

        // Mark packet as seen (dedup)
        {
            let db_lock = db.lock().await;
            let _ = mark_pulled(&db_lock, topic_id, &packet.packet_id);
        }

        return Ok(ProcessResult {
            receipt: Receipt::new(packet.packet_id, our_id),
            content_payload: None,
        });
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
                        topic_id,
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
                    if let Err(e) = remove_topic_member(&db_lock, topic_id, &leave.leaver) {
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
                            topic_id,
                            &ann.source_id,
                            &ann.display_name,
                            ann.total_size,
                            ann.num_sections,
                        ) {
                            warn!(error = %e, "failed to store blob metadata");
                        } else {
                            // Initialize sections for tracking
                            let total_chunks = ((ann.total_size + CHUNK_SIZE - 1)
                                / CHUNK_SIZE) as u32;
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
                            // Note: We need to drop db_lock before sending to avoid deadlock
                            drop(db_lock);
                            let file_event = ProtocolEvent::FileAnnounced(FileAnnouncedEvent {
                                topic_id: *topic_id,
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

                            // Mark packet as seen and return early (db_lock was dropped)
                            {
                                let db_lock = db.lock().await;
                                let _ = mark_pulled(&db_lock, topic_id, &packet.packet_id);
                            }
                            return Ok(ProcessResult {
                                receipt: Receipt::new(packet.packet_id, our_id),
                                content_payload: None,
                            });
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
            TopicMessage::SyncRequest => {}   // Handled above, before db lock
        }
    }

    // Extract content payload if this is a Content message
    let content_payload = if let Some(TopicMessage::Content(data)) = topic_msg {
        // Emit Message event
        let event = ProtocolEvent::Message(IncomingMessage {
            topic_id: *topic_id,
            sender_id,
            payload: data.clone(),
            timestamp: current_timestamp(),
        });

        if event_tx.send(event).await.is_err() {
            debug!("event receiver dropped");
        }
        Some(data)
    } else {
        None
    };

    // Mark packet as seen (dedup) so Harbor pull won't deliver it again
    {
        let db_lock = db.lock().await;
        let _ = mark_pulled(&db_lock, topic_id, &packet.packet_id);
    }

    Ok(ProcessResult {
        receipt: Receipt::new(packet.packet_id, our_id),
        content_payload,
    })
}

/// Verify and decrypt a packet (no PoW verification)
///
/// # Returns
/// - The decrypted plaintext if verification succeeds
/// - The receipt to send back
pub fn receive_packet(
    packet: &SendPacket,
    topic_id: &[u8; 32],
    our_id: [u8; 32],
) -> Result<(Vec<u8>, Receipt), PacketError> {
    // Verify and decrypt
    let plaintext = verify_and_decrypt_packet(packet, topic_id)?;

    // Create receipt
    let receipt = Receipt::new(packet.packet_id, our_id);

    Ok((plaintext, receipt))
}

/// Verify and decrypt a packet with PoW verification
///
/// # Arguments
/// * `packet` - The received packet
/// * `proof_of_work` - The PoW attached to the packet
/// * `topic_id` - The topic this packet is for
/// * `our_id` - Our EndpointID
/// * `pow_config` - PoW configuration for verification
///
/// # Returns
/// - The decrypted plaintext if verification succeeds
/// - The receipt to send back
///
/// # PoW Verification
/// Verifies that:
/// 1. PoW target matches `send_target_id(topic_id, our_endpoint_id)`
/// 2. PoW packet_id matches the packet's packet_id
/// 3. PoW meets difficulty and freshness requirements
pub fn receive_packet_with_pow(
    packet: &SendPacket,
    proof_of_work: &crate::resilience::ProofOfWork,
    topic_id: &[u8; 32],
    our_id: [u8; 32],
    pow_config: &PoWConfig,
) -> Result<(Vec<u8>, Receipt), ReceiveError> {
    // Skip PoW verification if disabled
    if !pow_config.enabled {
        let (plaintext, receipt) = receive_packet(packet, topic_id, our_id)?;
        return Ok((plaintext, receipt));
    }

    // Verify PoW is bound to correct target (topic + us as recipient)
    let expected_target = send_target_id(topic_id, &our_id);
    if proof_of_work.harbor_id != expected_target {
        return Err(ReceiveError::PoWTargetMismatch);
    }

    // Verify PoW is bound to this packet
    if proof_of_work.packet_id != packet.packet_id {
        return Err(ReceiveError::PoWTargetMismatch);
    }

    // Verify PoW meets requirements (difficulty, freshness)
    let result = verify_pow(proof_of_work, pow_config);
    if !result.is_valid() {
        return Err(ReceiveError::InvalidPoW(result));
    }

    // PoW valid - now verify and decrypt the packet
    let plaintext = verify_and_decrypt_packet(packet, topic_id)?;

    // Create receipt
    let receipt = Receipt::new(packet.packet_id, our_id);

    Ok((plaintext, receipt))
}

/// Process an incoming receipt - update tracking in database
///
/// Note: Takes `&Connection` (not `&mut`) because this is a single UPDATE
/// operation that doesn't require transactional guarantees.
pub fn process_receipt(
    receipt: &Receipt,
    conn: &rusqlite::Connection,
) -> Result<bool, rusqlite::Error> {
    db_acknowledge_receipt(conn, &receipt.packet_id, &receipt.sender)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::create_key_pair::generate_key_pair;
    use crate::security::send::packet::create_packet;
    use crate::data::schema::create_all_tables;
    use crate::data::send::store_outgoing_packet;
    use crate::security::harbor_id_from_topic;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn setup_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_all_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_process_error_display() {
        let err = ProcessError::VerificationFailed("bad MAC".to_string());
        assert!(err.to_string().contains("verification failed"));

        let err = ProcessError::Database("constraint".to_string());
        assert!(err.to_string().contains("database error"));

        let err = ProcessError::InvalidMessage("bad format".to_string());
        assert!(err.to_string().contains("invalid message"));

        let err = ProcessError::SenderMismatch("wrong sender".to_string());
        assert!(err.to_string().contains("sender mismatch"));
    }

    #[test]
    fn test_receive_error_display() {
        let err = ReceiveError::PoWRequired;
        assert_eq!(err.to_string(), "proof of work required");

        let err = ReceiveError::PoWTargetMismatch;
        assert_eq!(err.to_string(), "proof of work target mismatch");

        let err = ReceiveError::InvalidPoW(PoWVerifyResult::Expired);
        assert!(err.to_string().contains("invalid proof of work"));
    }

    #[test]
    fn test_receive_packet_success() {
        // Create sender's keys
        let sender_keys = generate_key_pair();
        let topic_id = test_id(100);

        // Create a packet
        let plaintext = b"Hello, world!";
        let packet = create_packet(
            &topic_id,
            &sender_keys.private_key,
            &sender_keys.public_key,
            plaintext,
        ).unwrap();

        // Create receiver
        let receiver_keys = generate_key_pair();

        // Receive and verify
        let (decrypted, receipt) = receive_packet(&packet, &topic_id, receiver_keys.public_key).unwrap();
        assert_eq!(decrypted, plaintext);
        assert_eq!(receipt.packet_id, packet.packet_id);
        assert_eq!(receipt.sender, receiver_keys.public_key);
    }

    #[test]
    fn test_receive_packet_wrong_topic() {
        let sender_keys = generate_key_pair();
        let topic_id = test_id(100);
        let wrong_topic_id = test_id(200);

        let packet = create_packet(
            &topic_id,
            &sender_keys.private_key,
            &sender_keys.public_key,
            b"Secret",
        ).unwrap();

        // Try to receive with wrong topic should fail
        let result = receive_packet(&packet, &wrong_topic_id, [0u8; 32]);
        assert!(result.is_err());
    }

    #[test]
    fn test_process_receipt_success() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let packet_id = [42u8; 32];
        let recipient_id = test_id(2);

        // Store outgoing packet
        store_outgoing_packet(
            &mut db_conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"packet data",
            &[recipient_id],
            0, // Content
        ).unwrap();

        // Process receipt
        let receipt = Receipt::new(packet_id, recipient_id);
        let acked = process_receipt(&receipt, &db_conn).unwrap();
        assert!(acked);
    }

    #[test]
    fn test_process_receipt_already_acked() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let packet_id = [42u8; 32];
        let recipient_id = test_id(2);

        store_outgoing_packet(
            &mut db_conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"packet data",
            &[recipient_id],
            0, // Content
        ).unwrap();

        // First ack
        let receipt = Receipt::new(packet_id, recipient_id);
        let acked1 = process_receipt(&receipt, &db_conn).unwrap();
        assert!(acked1);

        // Second ack should return false (already acked)
        let acked2 = process_receipt(&receipt, &db_conn).unwrap();
        assert!(!acked2);
    }

    #[test]
    fn test_receipt_creation() {
        let packet_id = [99u8; 32];
        let sender = test_id(5);

        let receipt = Receipt::new(packet_id, sender);
        assert_eq!(receipt.packet_id, packet_id);
        assert_eq!(receipt.sender, sender);
    }
}
