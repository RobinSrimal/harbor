//! Send Service - DM operations
//!
//! All DM-related send and receive logic:
//! - Sending: `send_dm`, `send_to_dm`, `deliver_dm_to_member`
//! - Processing: `process_raw_dm_payload_with_id`
//! - Handler dispatch: `handle_deliver_dm`

use iroh::EndpointId;
use tracing::{debug, info, trace};

use crate::data::send::store_outgoing_packet;
use crate::data::{dedup_check_and_mark, DedupResult};
use crate::network::rpc::ExistingConnection;
use crate::protocol::MemberInfo;
use crate::resilience::{ProofOfWork, PoWConfig, build_context};
use super::protocol::{SendRpcProtocol, DeliverDm, Receipt};
use super::service::{SendService, SendError, ProcessError, ProcessResult, generate_packet_id};

impl SendService {
    // ========== Handler Dispatch ==========

    /// Handle an incoming DeliverDm message
    ///
    /// Performs PoW verification, deduplication, then delegates to
    /// `process_raw_dm_payload_with_id`. Always returns a Receipt.
    pub async fn handle_deliver_dm(
        &self,
        msg: &DeliverDm,
        sender_id: [u8; 32],
    ) -> Receipt {
        let our_id = self.endpoint_id();
        let db = self.db().clone();

        trace!(
            sender = %hex::encode(&sender_id[..8]),
            payload_len = msg.payload.len(),
            "received DeliverDm (direct delivery)"
        );

        // Verify PoW first (context: sender_id || recipient_id)
        if !self.verify_dm_pow(&msg.pow, &sender_id, &our_id, msg.payload.len()) {
            debug!(
                sender = %hex::encode(&sender_id[..8]),
                "DeliverDm rejected: insufficient PoW"
            );
            return Receipt::new(msg.packet_id, our_id);
        }

        // Deduplication check - for DMs, use our_id as "topic" (DMs are addressed to us)
        let dedup_result = {
            let db_lock = db.lock().await;
            dedup_check_and_mark(&db_lock, &our_id, &msg.packet_id, &sender_id, &our_id)
                .unwrap_or(DedupResult::Process)
        };

        if !dedup_result.should_process() {
            trace!(
                packet_id = %hex::encode(&msg.packet_id[..8]),
                result = %dedup_result,
                "DeliverDm skipped (dedup)"
            );
            // Still send receipt so sender knows we got it
            return Receipt::new(msg.packet_id, our_id);
        }

        // Process raw DmMessage payload (already plaintext from QUIC TLS)
        match self.process_raw_dm_payload_with_id(sender_id, &msg.payload, msg.packet_id).await {
            Ok(r) => r.receipt,
            Err(e) => {
                debug!(error = %e, "DeliverDm processing error");
                Receipt::new(msg.packet_id, our_id)
            }
        }
    }

    // ========== Outgoing ==========

    /// Send a direct message to a single peer (API layer)
    ///
    /// Wraps the payload as DmMessage::Content and delegates to send_to_dm.
    pub async fn send_dm(
        &self,
        recipient_id: &[u8; 32],
        payload: &[u8],
    ) -> Result<(), SendError> {
        use crate::network::packet::DmMessage;

        let encoded = DmMessage::Content(payload.to_vec()).encode();
        self.send_to_dm(recipient_id, &encoded).await
    }

    /// Send pre-encoded DM bytes to a single peer (service layer)
    ///
    /// Used by other services to send arbitrary DM message types
    /// (e.g., FileAnnouncement, CanSeed, SyncUpdate).
    /// Callers encode the DmMessage themselves.
    ///
    /// # Flow:
    /// 1. Store RAW payload in outgoing_packets (no encryption)
    /// 2. Deliver raw payload directly via QUIC TLS (DeliverDm)
    /// 3. Harbor replication task will seal() undelivered packets later
    ///
    /// The packet is stored using the recipient's endpoint_id as harbor_id.
    pub async fn send_to_dm(
        &self,
        recipient_id: &[u8; 32],
        encoded_payload: &[u8],
    ) -> Result<(), SendError> {
        // Generate packet_id for tracking (seal() will use this when creating the sealed packet)
        let packet_id = generate_packet_id();
        let harbor_id = *recipient_id;

        // Store RAW payload in outgoing table (no encryption - seal() called during harbor replication)
        {
            let mut db = self.db().lock().await;
            store_outgoing_packet(
                &mut db,
                &packet_id,
                recipient_id, // use recipient_id as "topic_id" for DM tracking
                &harbor_id,
                encoded_payload,  // Raw payload, not encrypted
                &[*recipient_id],
                0, // packet_type is embedded in payload now
            ).map_err(|e| SendError::Database(e.to_string()))?;
        }

        // Try direct delivery via DeliverDm (raw payload over QUIC TLS)
        let member = MemberInfo::new(*recipient_id);
        match self.deliver_dm_to_member(&member, encoded_payload, packet_id).await {
            Ok(receipt) => {
                trace!(
                    recipient = hex::encode(recipient_id),
                    receipt_from = hex::encode(receipt.sender),
                    "DM delivered with receipt"
                );
            }
            Err(e) => {
                debug!(
                    recipient = hex::encode(recipient_id),
                    error = %e,
                    "DM direct delivery failed, will rely on harbor"
                );
            }
        }

        Ok(())
    }

    // ========== Transport: irpc RPC delivery ==========

    /// Deliver raw DM payload directly to a member via DeliverDm
    ///
    /// Uses QUIC TLS for encryption - no app-level crypto needed.
    /// This is the fast path for direct delivery to online peers.
    async fn deliver_dm_to_member(
        &self,
        member: &MemberInfo,
        payload: &[u8],
        packet_id: [u8; 32],
    ) -> Result<Receipt, SendError> {
        let node_id = EndpointId::from_bytes(&member.endpoint_id)
            .map_err(|e| SendError::Connection(e.to_string()))?;

        // Get or create connection (relay URL looked up from peers table)
        let conn = self.get_connection(node_id).await?;

        // Compute PoW (context: sender_id || recipient_id)
        let sender_id = self.endpoint_id();
        let pow_context = build_context(&[&sender_id, &member.endpoint_id]);
        let pow = ProofOfWork::compute(&pow_context, PoWConfig::send().base_difficulty)
            .ok_or_else(|| SendError::Send("failed to compute PoW".to_string()))?;

        // Send via irpc RPC - DeliverDm request, Receipt response
        let client = irpc::Client::<SendRpcProtocol>::boxed(
            ExistingConnection::new(&conn)
        );
        let receipt = client
            .rpc(DeliverDm {
                packet_id,
                payload: payload.to_vec(),
                pow,
            })
            .await
            .map_err(|e| SendError::Send(e.to_string()))?;

        Ok(receipt)
    }

    // ========== Incoming (Direct Delivery - Raw Payloads) ==========

    /// Process a raw DM payload received via direct delivery (DeliverDm)
    ///
    /// The payload is already plaintext (QUIC TLS provides encryption).
    /// No decryption or signature verification needed.
    ///
    /// The packet_id is provided by the sender for deduplication.
    pub async fn process_raw_dm_payload_with_id(
        &self,
        sender_id: [u8; 32],
        payload: &[u8],
        packet_id: [u8; 32],
    ) -> Result<ProcessResult, ProcessError> {
        use crate::network::packet::{
            DmMessage, StreamSignalingMessage, is_dm_message_type, is_stream_signaling_type
        };

        let our_id = self.endpoint_id();
        info!(payload_len = payload.len(), "processing raw DM payload (direct delivery)");

        if payload.is_empty() {
            return Err(ProcessError::VerificationFailed("empty payload".to_string()));
        }

        let type_byte = payload[0];

        // Check if this is a stream signaling message (0x50-0x5F)
        if is_stream_signaling_type(type_byte) {
            let stream_msg = StreamSignalingMessage::decode(payload)
                .map_err(|e| ProcessError::VerificationFailed(format!("Stream signaling decode: {}", e)))?;

            if let Some(stream_svc) = self.stream_service().await {
                stream_svc.handle_stream_signaling_msg(&stream_msg, sender_id).await;
            }

            return Ok(ProcessResult {
                receipt: Receipt::new(packet_id, our_id),
                content_payload: None,
            });
        }

        // Check if this is a DM message (0x40-0x4F)
        if !is_dm_message_type(type_byte) {
            return Err(ProcessError::VerificationFailed(format!("unknown type byte: 0x{:02x}", type_byte)));
        }

        // Decode DmMessage (payload is already plaintext)
        let dm_msg = DmMessage::decode(payload)
            .map_err(|e| ProcessError::VerificationFailed(format!("DM decode: {}", e)))?;

        // Handle the decoded DM message
        match dm_msg {
            DmMessage::Content(data) => {
                let event = crate::protocol::ProtocolEvent::DmReceived(
                    crate::protocol::DmReceivedEvent {
                        sender_id,
                        payload: data,
                        timestamp: crate::data::current_timestamp(),
                    },
                );
                let _ = self.event_tx().send(event).await;
            }
            DmMessage::SyncUpdate(msg) => {
                let event = crate::protocol::ProtocolEvent::DmSyncUpdate(
                    crate::protocol::DmSyncUpdateEvent {
                        sender_id,
                        data: msg.data,
                    },
                );
                let _ = self.event_tx().send(event).await;
            }
            DmMessage::SyncRequest => {
                let event = crate::protocol::ProtocolEvent::DmSyncRequest(
                    crate::protocol::DmSyncRequestEvent {
                        sender_id,
                    },
                );
                let _ = self.event_tx().send(event).await;
            }
            DmMessage::FileAnnouncement(msg) => {
                let event = crate::protocol::ProtocolEvent::DmFileAnnounced(
                    crate::protocol::DmFileAnnouncedEvent {
                        sender_id,
                        hash: msg.hash,
                        display_name: msg.display_name,
                        total_size: msg.total_size,
                        total_chunks: msg.total_chunks,
                        num_sections: msg.num_sections,
                        timestamp: crate::data::current_timestamp(),
                    },
                );
                let _ = self.event_tx().send(event).await;
            }
            // DM stream request — route to StreamService
            DmMessage::StreamRequest(_) => {
                if let Some(stream_svc) = self.stream_service().await {
                    stream_svc.handle_dm_signaling(&dm_msg, sender_id).await;
                }
            }
        }

        Ok(ProcessResult {
            receipt: Receipt::new(packet_id, our_id),
            content_payload: None,
        })
    }
}
