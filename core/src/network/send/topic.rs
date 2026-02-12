//! Send Service - Topic operations
//!
//! All topic-related send and receive logic:
//! - Sending: `send_topic`, `send_to_topic`, `deliver`
//! - Processing: `process_raw_topic_payload_with_id`, `handle_topic_message`
//! - Handler dispatch: `handle_deliver_topic`

use std::sync::Arc;

use futures::future::join_all;
use iroh::EndpointId;
use rusqlite::Connection as DbConnection;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, trace, warn};

use super::protocol::{
    DeliverTopic, Receipt, SendRpcProtocol, create_membership_proof, verify_membership_proof,
};
use super::service::{
    PacketSource, ProcessError, ProcessResult, SendError, SendOptions, SendResult, SendService,
    generate_packet_id,
};
use crate::data::send::store_outgoing_packet;
use crate::data::{
    CHUNK_SIZE, DedupResult, WILDCARD_RECIPIENT, dedup_check_and_mark, get_blob, get_topic_members,
    get_topics_for_member, init_blob_sections, insert_blob, mark_pulled, record_peer_can_seed,
};
use crate::network::packet::TopicMessage;
use crate::network::rpc::ExistingConnection;
use crate::protocol::{MemberInfo, ProtocolEvent};
use crate::resilience::{PoWConfig, ProofOfWork, build_context};
use crate::security::{PacketId, harbor_id_from_topic};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlobLookupStatus {
    Known,
    Missing,
    LookupFailed,
}

fn dedup_result_or_process<E: std::fmt::Display>(
    dedup_result: Result<DedupResult, E>,
    packet_id: &PacketId,
    sender_id: &[u8; 32],
    topic_id: &[u8; 32],
) -> DedupResult {
    match dedup_result {
        Ok(result) => result,
        Err(e) => {
            warn!(
                error = %e,
                packet_id = %hex::encode(&packet_id[..8]),
                sender = %hex::encode(&sender_id[..8]),
                topic = %hex::encode(&topic_id[..8]),
                "DeliverTopic dedup check failed; processing packet defensively"
            );
            DedupResult::Process
        }
    }
}

fn validate_topic_receipt(
    receipt: &Receipt,
    expected_packet_id: &PacketId,
    expected_sender: &[u8; 32],
) -> Result<(), SendError> {
    if receipt.packet_id != *expected_packet_id {
        return Err(SendError::Send(format!(
            "invalid receipt packet_id: expected {}, got {}",
            hex::encode(&expected_packet_id[..8]),
            hex::encode(&receipt.packet_id[..8]),
        )));
    }

    if receipt.sender != *expected_sender {
        return Err(SendError::Send(format!(
            "invalid receipt sender: expected {}, got {}",
            hex::encode(&expected_sender[..8]),
            hex::encode(&receipt.sender[..8]),
        )));
    }

    Ok(())
}

fn persist_topic_receipt(
    conn: &rusqlite::Connection,
    receipt: &Receipt,
) -> Result<bool, SendError> {
    SendService::process_receipt(receipt, conn)
        .map_err(|e| SendError::Database(format!("receipt persistence failed: {}", e)))
}

fn classify_blob_lookup(
    lookup_result: rusqlite::Result<Option<crate::data::BlobMetadata>>,
    hash: &[u8; 32],
) -> BlobLookupStatus {
    match lookup_result {
        Ok(Some(_)) => BlobLookupStatus::Known,
        Ok(None) => BlobLookupStatus::Missing,
        Err(e) => {
            warn!(
                error = %e,
                hash = %hex::encode(&hash[..8]),
                "failed to read blob metadata; skipping persistence for this announcement"
            );
            BlobLookupStatus::LookupFailed
        }
    }
}

impl SendService {
    // ========== Handler Dispatch ==========

    /// Handle an incoming DeliverTopic message
    ///
    /// Performs PoW verification, topic resolution, membership proof check,
    /// deduplication, then delegates to `process_raw_topic_payload_with_id`.
    /// Always returns a Receipt.
    pub async fn handle_deliver_topic(&self, msg: &DeliverTopic, sender_id: [u8; 32]) -> Receipt {
        let our_id = self.endpoint_id();
        let db = self.db().clone();

        trace!(
            harbor_id = %hex::encode(&msg.harbor_id[..8]),
            sender = %hex::encode(&sender_id[..8]),
            payload_len = msg.payload.len(),
            "received DeliverTopic (direct delivery)"
        );

        // Verify PoW first (context: sender_id || harbor_id)
        if !self.verify_topic_pow(&msg.pow, &sender_id, &msg.harbor_id, msg.payload.len()) {
            debug!(
                sender = %hex::encode(&sender_id[..8]),
                harbor_id = %hex::encode(&msg.harbor_id[..8]),
                "DeliverTopic rejected: insufficient PoW"
            );
            return Receipt::new(msg.packet_id, our_id);
        }

        // Find which topic this message belongs to by matching harbor_id
        let topics = {
            let db_lock = db.lock().await;
            match get_topics_for_member(&db_lock, &our_id) {
                Ok(t) => t,
                Err(e) => {
                    debug!(error = %e, "DeliverTopic: failed to get topics");
                    return Receipt::new(msg.packet_id, our_id);
                }
            }
        };

        // Find matching topic and verify membership proof
        let mut matched_topic: Option<[u8; 32]> = None;
        for topic_id in &topics {
            let expected_harbor_id = harbor_id_from_topic(topic_id);
            if expected_harbor_id == msg.harbor_id {
                // Verify membership proof: sender must know the topic_id
                if verify_membership_proof(
                    topic_id,
                    &msg.harbor_id,
                    &sender_id,
                    &msg.membership_proof,
                ) {
                    matched_topic = Some(*topic_id);
                    break;
                } else {
                    warn!(
                        harbor_id = %hex::encode(&msg.harbor_id[..8]),
                        sender = %hex::encode(&sender_id[..8]),
                        "DeliverTopic membership proof verification failed"
                    );
                }
            }
        }

        let topic_id = match matched_topic {
            Some(id) => id,
            None => {
                debug!(
                    harbor_id = %hex::encode(&msg.harbor_id[..8]),
                    "DeliverTopic for unknown topic or invalid proof"
                );
                return Receipt::new(msg.packet_id, our_id);
            }
        };

        // Deduplication check - skip if already seen
        let dedup_result = {
            let db_lock = db.lock().await;
            dedup_result_or_process(
                dedup_check_and_mark(&db_lock, &topic_id, &msg.packet_id, &sender_id, &our_id),
                &msg.packet_id,
                &sender_id,
                &topic_id,
            )
        };

        if !dedup_result.should_process() {
            trace!(
                packet_id = %hex::encode(&msg.packet_id[..8]),
                result = %dedup_result,
                "DeliverTopic skipped (dedup)"
            );
            // Still send receipt so sender knows we got it
            return Receipt::new(msg.packet_id, our_id);
        }

        // Process raw TopicMessage payload (already plaintext from QUIC TLS)
        match self
            .process_raw_topic_payload_with_id(&topic_id, sender_id, &msg.payload, msg.packet_id)
            .await
        {
            Ok(r) => r.receipt,
            Err(e) => {
                debug!(error = %e, "DeliverTopic processing error");
                Receipt::new(msg.packet_id, our_id)
            }
        }
    }

    // ========== Outgoing ==========

    /// Send content to a topic, resolving recipients from the database
    ///
    /// Looks up topic members, validates membership, filters out self,
    /// encodes as Content message, and delivers to all recipients.
    pub async fn send_topic(&self, topic_id: &[u8; 32], payload: &[u8]) -> Result<(), SendError> {
        let our_id = self.endpoint_id();

        // Get topic members
        let members = {
            let db = self.db().lock().await;
            get_topic_members(&db, topic_id).map_err(|e| SendError::Database(e.to_string()))?
        };

        trace!(
            topic = %hex::encode(topic_id),
            member_count = members.len(),
            "sending message"
        );

        if members.is_empty() {
            return Err(SendError::TopicNotFound);
        }

        if !members.iter().any(|m| *m == our_id) {
            return Err(SendError::NotMember);
        }

        // Get recipients (all members except us)
        let recipients: Vec<MemberInfo> = members
            .into_iter()
            .filter(|m| *m != our_id)
            .map(|endpoint_id| MemberInfo::new(endpoint_id))
            .collect();

        if recipients.is_empty() {
            trace!("no recipients - only member is self");
            return Ok(());
        }

        // Encode as Content message and send
        let content_msg = TopicMessage::Content(payload.to_vec());
        let encoded_payload = content_msg.encode();

        self.send_to_topic(
            topic_id,
            &encoded_payload,
            &recipients,
            SendOptions::content(),
        )
        .await?;

        Ok(())
    }

    /// Send a message to topic members
    ///
    /// Lower-level entry point for all send operations.
    /// Takes pre-encoded TopicMessage bytes and delivers to all recipients.
    ///
    /// # Flow:
    /// 1. Store RAW payload in outgoing_packets (no encryption)
    /// 2. Deliver raw payload directly via QUIC TLS (DeliverTopic)
    /// 3. Harbor replication task will seal() undelivered packets later
    pub async fn send_to_topic(
        &self,
        topic_id: &[u8; 32],
        encoded_payload: &[u8],
        recipients: &[MemberInfo],
        options: SendOptions,
    ) -> Result<SendResult, SendError> {
        if recipients.is_empty() {
            return Err(SendError::NoRecipients);
        }

        // Extract endpoint IDs for storage
        let recipient_ids: Vec<[u8; 32]> = recipients.iter().map(|m| m.endpoint_id).collect();

        // Generate packet_id for tracking (seal() will use this when creating the sealed packet)
        let packet_id = generate_packet_id();
        let harbor_id = harbor_id_from_topic(topic_id);

        // Store RAW payload in outgoing table (no encryption - seal() called during harbor replication)
        if !options.skip_harbor {
            let mut db = self.db().lock().await;
            store_outgoing_packet(
                &mut db,
                &packet_id,
                topic_id,
                &harbor_id,
                encoded_payload, // Raw payload, not encrypted
                &recipient_ids,
                0, // packet_type is embedded in payload now
            )
            .map_err(|e| SendError::Database(e.to_string()))?;
        }

        // Filter out self and wildcard for actual delivery
        let our_id = self.endpoint_id();
        let actual_recipients: Vec<&MemberInfo> = recipients
            .iter()
            .filter(|m| m.endpoint_id != our_id && m.endpoint_id != WILDCARD_RECIPIENT)
            .collect();

        let mut delivered_to = Vec::new();
        let mut failed = Vec::new();

        // Mark self as delivered if in recipients
        if recipients.iter().any(|m| m.endpoint_id == our_id) {
            delivered_to.push(our_id);
        }

        if actual_recipients.is_empty() {
            debug!(
                packet_id = hex::encode(&packet_id[..8]),
                "no recipients (only self/wildcard)"
            );
            return Ok(SendResult {
                packet_id,
                delivered_to,
                failed,
            });
        }

        info!(
            packet_id = hex::encode(&packet_id[..8]),
            recipient_count = actual_recipients.len(),
            "sending to recipients in parallel (direct delivery)"
        );

        // Deliver RAW payload to all recipients in parallel via DeliverTopic
        let send_futures = actual_recipients.iter().map(|member| {
            let member = (*member).clone();
            let topic_id = *topic_id;
            let payload = encoded_payload.to_vec();
            let pkt_id = packet_id;
            async move {
                let result = self.deliver(&member, &topic_id, &payload, pkt_id).await;
                (member.endpoint_id, result)
            }
        });

        let results = join_all(send_futures).await;

        for (endpoint_id, result) in results {
            match result {
                Ok(receipt) => {
                    if let Err(e) = validate_topic_receipt(&receipt, &packet_id, &endpoint_id) {
                        let err_msg = e.to_string();
                        failed.push((endpoint_id, err_msg.clone()));
                        debug!(
                            recipient = hex::encode(endpoint_id),
                            error = %err_msg,
                            "direct delivery receipt validation failed; will rely on harbor"
                        );
                        continue;
                    }

                    if !options.skip_harbor {
                        let persist_result = {
                            let db = self.db().lock().await;
                            persist_topic_receipt(&db, &receipt)
                        };
                        match persist_result {
                            Ok(was_new_ack) => {
                                if !was_new_ack {
                                    trace!(
                                        recipient = hex::encode(endpoint_id),
                                        packet_id = hex::encode(&packet_id[..8]),
                                        "direct delivery receipt already recorded"
                                    );
                                }
                            }
                            Err(e) => {
                                let err_msg = e.to_string();
                                failed.push((endpoint_id, err_msg.clone()));
                                debug!(
                                    recipient = hex::encode(endpoint_id),
                                    error = %err_msg,
                                    "direct delivery receipt persistence failed; will rely on harbor"
                                );
                                continue;
                            }
                        }
                    }

                    delivered_to.push(endpoint_id);
                    trace!(
                        recipient = hex::encode(endpoint_id),
                        receipt_from = hex::encode(receipt.sender),
                        "packet delivered with receipt"
                    );
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    failed.push((endpoint_id, err_msg.clone()));
                    debug!(
                        recipient = hex::encode(endpoint_id),
                        error = %err_msg,
                        "direct delivery failed, will rely on harbor"
                    );
                }
            }
        }

        info!(
            packet_id = hex::encode(&packet_id[..8]),
            delivered = delivered_to.len(),
            failed = failed.len(),
            "parallel send complete"
        );

        Ok(SendResult {
            packet_id,
            delivered_to,
            failed,
        })
    }

    // ========== Transport: irpc RPC delivery ==========

    /// Deliver raw topic payload directly to a member via DeliverTopic
    ///
    /// Uses QUIC TLS for encryption - no app-level crypto needed.
    /// This is the fast path for direct delivery to online peers.
    ///
    /// The message includes a membership proof (BLAKE3 hash) that proves
    /// we know the topic_id without revealing it on the wire.
    async fn deliver(
        &self,
        member: &MemberInfo,
        topic_id: &[u8; 32],
        payload: &[u8],
        packet_id: PacketId,
    ) -> Result<Receipt, SendError> {
        let node_id = EndpointId::from_bytes(&member.endpoint_id)
            .map_err(|e| SendError::Connection(e.to_string()))?;

        // Get or create connection (relay URL looked up from peers table)
        let conn = self.get_connection(node_id).await?;

        // Compute harbor_id and membership proof
        let harbor_id = harbor_id_from_topic(topic_id);
        let sender_id = self.endpoint_id();
        let membership_proof = create_membership_proof(topic_id, &harbor_id, &sender_id);

        // Compute PoW (context: sender_id || harbor_id)
        let pow_context = build_context(&[&sender_id, &harbor_id]);
        let pow = ProofOfWork::compute(&pow_context, PoWConfig::send().base_difficulty)
            .ok_or_else(|| SendError::Send("failed to compute PoW".to_string()))?;

        // Send via irpc RPC - DeliverTopic request, Receipt response
        let client = irpc::Client::<SendRpcProtocol>::boxed(ExistingConnection::new(&conn));
        let receipt = client
            .rpc(DeliverTopic {
                packet_id,
                harbor_id,
                membership_proof,
                payload: payload.to_vec(),
                pow,
            })
            .await
            .map_err(|e| SendError::Send(e.to_string()))?;

        Ok(receipt)
    }

    // ========== Incoming (Direct Delivery - Raw Payloads) ==========

    /// Process a raw topic message payload received via direct delivery (DeliverTopic)
    ///
    /// The payload is already plaintext (QUIC TLS provides encryption).
    /// No decryption or MAC/signature verification needed.
    ///
    /// The packet_id is provided by the sender for deduplication.
    pub async fn process_raw_topic_payload_with_id(
        &self,
        topic_id: &[u8; 32],
        sender_id: [u8; 32],
        payload: &[u8],
        packet_id: PacketId,
    ) -> Result<ProcessResult, ProcessError> {
        let db = self.db();
        let event_tx = self.event_tx();
        let stream = self.stream_service().await;

        info!(
            payload_len = payload.len(),
            "processing raw topic payload (direct delivery)"
        );

        // Parse topic message (payload is already plaintext)
        let topic_msg = match TopicMessage::decode(payload) {
            Ok(msg) => {
                info!(msg_type = ?msg.packet_type(), "decoded TopicMessage from raw payload");
                Some(msg)
            }
            Err(e) => {
                warn!(
                    topic = %hex::encode(&topic_id[..8]),
                    error = %e,
                    payload_len = payload.len(),
                    first_byte = ?payload.first(),
                    "failed to decode TopicMessage from raw payload"
                );
                None
            }
        };

        // Handle the decoded message
        self.handle_topic_message(
            topic_id,
            sender_id,
            &topic_msg,
            &packet_id,
            db,
            event_tx,
            stream.as_ref(),
        )
        .await
    }

    /// Helper: Handle a decoded TopicMessage
    ///
    /// Dispatches topic messages to appropriate handlers (content, sync, file, stream).
    async fn handle_topic_message(
        &self,
        topic_id: &[u8; 32],
        sender_id: [u8; 32],
        topic_msg: &Option<TopicMessage>,
        packet_id: &PacketId,
        db: &Arc<Mutex<DbConnection>>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        stream: Option<&Arc<crate::network::stream::StreamService>>,
    ) -> Result<ProcessResult, ProcessError> {
        let our_id = self.endpoint_id();

        // Handle SyncUpdate - emit event for app to handle
        if let Some(TopicMessage::SyncUpdate(sync_msg)) = topic_msg {
            info!(
                topic = %hex::encode(&topic_id[..8]),
                sender = %hex::encode(&sender_id[..8]),
                size = sync_msg.data.len(),
                "SYNC: received update from peer"
            );

            let event =
                crate::protocol::ProtocolEvent::SyncUpdate(crate::protocol::SyncUpdateEvent {
                    topic_id: *topic_id,
                    sender_id,
                    data: sync_msg.data.clone(),
                });
            let _ = event_tx.send(event).await;

            {
                let db_lock = db.lock().await;
                let _ = mark_pulled(&db_lock, topic_id, packet_id);
            }

            return Ok(ProcessResult {
                receipt: Receipt::new(*packet_id, our_id),
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

            let event =
                crate::protocol::ProtocolEvent::SyncRequest(crate::protocol::SyncRequestEvent {
                    topic_id: *topic_id,
                    sender_id,
                });
            let _ = event_tx.send(event).await;

            {
                let db_lock = db.lock().await;
                let _ = mark_pulled(&db_lock, topic_id, packet_id);
            }

            return Ok(ProcessResult {
                receipt: Receipt::new(*packet_id, our_id),
                content_payload: None,
            });
        }

        // Handle control messages
        if let Some(msg) = topic_msg {
            let db_lock = db.lock().await;
            match msg {
                TopicMessage::FileAnnouncement(ann) => {
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

                        match classify_blob_lookup(get_blob(&db_lock, &ann.hash), &ann.hash) {
                            BlobLookupStatus::Known => {
                                debug!(
                                    hash = hex::encode(&ann.hash[..8]),
                                    "blob already known, skipping insert"
                                );
                            }
                            BlobLookupStatus::Missing => {
                                if let Err(e) = insert_blob(
                                    &db_lock,
                                    &ann.hash,
                                    topic_id, // scope_id: topic scopes the blob
                                    &ann.source_id,
                                    &ann.display_name,
                                    ann.total_size,
                                    ann.num_sections,
                                ) {
                                    warn!(error = %e, "failed to store blob metadata");
                                } else {
                                    let total_chunks =
                                        ((ann.total_size + CHUNK_SIZE - 1) / CHUNK_SIZE) as u32;
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

                                    // Drop db_lock before sending event to avoid deadlock
                                    drop(db_lock);
                                    let file_event = crate::protocol::ProtocolEvent::FileAnnounced(
                                        crate::protocol::FileAnnouncedEvent {
                                            topic_id: *topic_id,
                                            source_id: ann.source_id,
                                            hash: ann.hash,
                                            display_name: ann.display_name.clone(),
                                            total_size: ann.total_size,
                                            total_chunks,
                                            timestamp: crate::data::current_timestamp(),
                                        },
                                    );
                                    if event_tx.send(file_event).await.is_err() {
                                        debug!("event receiver dropped");
                                    }

                                    {
                                        let db_lock = db.lock().await;
                                        let _ = mark_pulled(&db_lock, topic_id, packet_id);
                                    }
                                    return Ok(ProcessResult {
                                        receipt: Receipt::new(*packet_id, our_id),
                                        content_payload: None,
                                    });
                                }
                            }
                            BlobLookupStatus::LookupFailed => {}
                        }
                    }
                }
                TopicMessage::CanSeed(can_seed) => {
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

                        if let Err(e) =
                            record_peer_can_seed(&db_lock, &can_seed.hash, &can_seed.seeder_id)
                        {
                            warn!(error = %e, "failed to record peer can seed");
                        }
                    }
                }
                TopicMessage::Content(_) => {}
                TopicMessage::SyncUpdate(_) => {}
                TopicMessage::SyncRequest => {}
                // Stream signaling — route to StreamService
                TopicMessage::StreamRequest(_) => {
                    drop(db_lock);
                    if let Some(ref stream_svc) = stream {
                        stream_svc
                            .handle_signaling(msg, topic_id, sender_id, PacketSource::Direct)
                            .await;
                    }

                    {
                        let db_lock = db.lock().await;
                        let _ = mark_pulled(&db_lock, topic_id, packet_id);
                    }
                    return Ok(ProcessResult {
                        receipt: Receipt::new(*packet_id, our_id),
                        content_payload: None,
                    });
                }
            }
        }

        // Extract content payload if this is a Content message
        let content_payload = if let Some(TopicMessage::Content(data)) = topic_msg.clone() {
            let event = crate::protocol::ProtocolEvent::Message(crate::protocol::IncomingMessage {
                topic_id: *topic_id,
                sender_id,
                payload: data.clone(),
                timestamp: crate::data::current_timestamp(),
            });

            if event_tx.send(event).await.is_err() {
                debug!("event receiver dropped");
            }
            Some(data)
        } else {
            None
        };

        // Mark packet as seen (dedup)
        {
            let db_lock = db.lock().await;
            let _ = mark_pulled(&db_lock, topic_id, packet_id);
        }

        Ok(ProcessResult {
            receipt: Receipt::new(*packet_id, our_id),
            content_payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::data::schema::create_all_tables;
    use crate::data::send::store_outgoing_packet;
    use crate::security::harbor_id_from_topic;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn test_packet_id(seed: u8) -> PacketId {
        [seed; 16]
    }

    fn setup_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_all_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_validate_topic_receipt_accepts_expected_fields() {
        let packet_id = test_packet_id(1);
        let sender = test_id(2);
        let receipt = Receipt::new(packet_id, sender);
        assert!(validate_topic_receipt(&receipt, &packet_id, &sender).is_ok());
    }

    #[test]
    fn test_validate_topic_receipt_rejects_packet_id_mismatch() {
        let expected_packet_id = test_packet_id(1);
        let sender = test_id(2);
        let receipt = Receipt::new(test_packet_id(3), sender);
        let err = validate_topic_receipt(&receipt, &expected_packet_id, &sender).unwrap_err();
        assert!(err.to_string().contains("invalid receipt packet_id"));
    }

    #[test]
    fn test_validate_topic_receipt_rejects_sender_mismatch() {
        let packet_id = test_packet_id(1);
        let expected_sender = test_id(2);
        let receipt = Receipt::new(packet_id, test_id(3));
        let err = validate_topic_receipt(&receipt, &packet_id, &expected_sender).unwrap_err();
        assert!(err.to_string().contains("invalid receipt sender"));
    }

    #[test]
    fn test_persist_topic_receipt_tracks_ack() {
        let mut conn = setup_db();
        let packet_id = test_packet_id(7);
        let topic_id = test_id(8);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let recipient = test_id(9);

        store_outgoing_packet(
            &mut conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"payload",
            &[recipient],
            0,
        )
        .unwrap();

        let receipt = Receipt::new(packet_id, recipient);
        let was_new = persist_topic_receipt(&conn, &receipt).unwrap();
        assert!(was_new);

        let was_new_again = persist_topic_receipt(&conn, &receipt).unwrap();
        assert!(!was_new_again);
    }

    #[test]
    fn test_dedup_result_or_process_falls_back_to_process_on_error() {
        let packet_id = test_packet_id(1);
        let sender = test_id(2);
        let topic = test_id(3);
        let result = dedup_result_or_process(
            Err(rusqlite::Error::InvalidQuery),
            &packet_id,
            &sender,
            &topic,
        );
        assert_eq!(result, DedupResult::Process);
    }

    #[test]
    fn test_classify_blob_lookup_status_variants() {
        let hash = test_id(4);
        let metadata = crate::data::BlobMetadata {
            hash,
            scope_id: test_id(5),
            source_id: test_id(6),
            display_name: "blob.bin".to_string(),
            total_size: 1024,
            total_chunks: 1,
            num_sections: 1,
            state: crate::data::BlobState::Partial,
            created_at: 0,
        };

        assert_eq!(
            classify_blob_lookup(Ok(Some(metadata)), &hash),
            BlobLookupStatus::Known
        );
        assert_eq!(
            classify_blob_lookup(Ok(None), &hash),
            BlobLookupStatus::Missing
        );
        assert_eq!(
            classify_blob_lookup(Err(rusqlite::Error::InvalidQuery), &hash),
            BlobLookupStatus::LookupFailed
        );
    }
}
