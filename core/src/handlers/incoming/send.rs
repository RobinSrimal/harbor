//! Send protocol incoming handler
//!
//! Handles incoming Send protocol connections:
//! - Packet delivery (encrypted messages)
//! - Receipt acknowledgements
//! - PacketWithPoW (spam-protected delivery)
//!
//! The handler is responsible for:
//! - Accepting streams and decoding messages
//! - Finding matching topic for packets
//! - PoW verification (gatekeeper - reject invalid PoW early)
//! - Delegating business logic to the service layer
//! - Sending receipts

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, trace};

use crate::data::get_topics_for_member;
use crate::data::send::acknowledge_and_cleanup_if_complete;
use crate::network::send::protocol::{SendMessage, Receipt};
use crate::network::send::{process_incoming_packet, ProcessError};
use crate::resilience::{ProofOfWork, PoWConfig, verify_pow};
use crate::security::{harbor_id_from_topic, send_target_id, SendPacket};

use crate::protocol::{Protocol, ProtocolEvent, ProtocolError};

impl Protocol {
    /// Handle a single incoming Send protocol connection
    pub(crate) async fn handle_send_connection(
        conn: iroh::endpoint::Connection,
        db: Arc<Mutex<Connection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        sender_id: [u8; 32],
        our_id: [u8; 32],
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
                    Self::handle_packet_internal(&conn, &db, &event_tx, sender_id, our_id, packet, None).await?;
                }
                SendMessage::Receipt(receipt) => {
                    Self::handle_receipt(&db, receipt).await;
                }
                SendMessage::PacketWithPoW { packet, proof_of_work } => {
                    Self::handle_packet_internal(&conn, &db, &event_tx, sender_id, our_id, packet, Some(proof_of_work)).await?;
                }
            }
        }

        Ok(())
    }

    /// Handle an incoming packet (with or without PoW)
    ///
    /// This is the consolidated handler that:
    /// 1. Finds the matching topic for the packet
    /// 2. Verifies PoW if provided (gatekeeper - reject early if invalid)
    /// 3. Delegates business logic to the service layer
    /// 4. Sends receipt back to sender
    async fn handle_packet_internal(
        conn: &iroh::endpoint::Connection,
        db: &Arc<Mutex<Connection>>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        sender_id: [u8; 32],
        our_id: [u8; 32],
        packet: SendPacket,
        proof_of_work: Option<ProofOfWork>,
    ) -> Result<(), ProtocolError> {
        let has_pow = proof_of_work.is_some();
        if has_pow {
            trace!(harbor_id = %hex::encode(packet.harbor_id), "processing packet with PoW");
        } else {
            info!(harbor_id = %hex::encode(packet.harbor_id), sender = %hex::encode(&sender_id[..8]), "processing incoming packet");
        }

        // Find which topic this packet belongs to
        let topics = {
            let db_lock = db.lock().await;
            get_topics_for_member(&db_lock, &our_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        if !has_pow {
            info!(topic_count = topics.len(), "member of topics");
        }

        // Try each topic we're subscribed to
        let mut processed = false;
        for topic_id in topics {
            let harbor_id = harbor_id_from_topic(&topic_id);
            if packet.harbor_id != harbor_id {
                continue;
            }

            if !has_pow {
                info!(topic = %hex::encode(&topic_id[..8]), "found matching topic for packet");
            }

            // PoW verification (gatekeeper) - if PoW provided, verify it before processing
            if let Some(ref pow) = proof_of_work {
                if !Self::verify_pow_for_topic(pow, &topic_id, &our_id, &packet.packet_id) {
                    // PoW invalid for this topic, try next
                    continue;
                }
                trace!(topic = %hex::encode(topic_id), "PoW verified");
            }

            // Delegate business logic to the service layer
            match process_incoming_packet(&packet, &topic_id, sender_id, our_id, db, event_tx).await {
                Ok(result) => {
                    // Send receipt back
                    Self::send_receipt(conn, result.receipt).await;
                    processed = true;
                    break;
                }
                Err(ProcessError::VerificationFailed(e)) => {
                    // Decryption failed for this topic, try next
                    trace!(error = %e, topic = %hex::encode(topic_id), "verification failed for topic");
                    continue;
                }
                Err(e) => {
                    // Other errors - log but continue (non-fatal)
                    debug!(error = %e, "packet processing error");
                    continue;
                }
            }
        }

        if !processed {
            if has_pow {
                debug!(
                    harbor_id = hex::encode(packet.harbor_id),
                    "PacketWithPoW for unknown topic or invalid PoW"
                );
            } else {
                debug!(
                    harbor_id = hex::encode(packet.harbor_id),
                    "packet for unknown topic"
                );
            }
        }

        Ok(())
    }

    /// Verify PoW for a specific topic (gatekeeper function)
    ///
    /// Returns true if PoW is valid for this topic+recipient combination.
    /// This is called before any business logic to reject invalid packets early.
    fn verify_pow_for_topic(
        proof_of_work: &ProofOfWork,
        topic_id: &[u8; 32],
        our_id: &[u8; 32],
        packet_id: &[u8; 32],
    ) -> bool {
        // Verify PoW is bound to correct target (topic + us as recipient)
        let expected_target = send_target_id(topic_id, our_id);
        if proof_of_work.harbor_id != expected_target {
            debug!(
                expected = hex::encode(expected_target),
                got = hex::encode(proof_of_work.harbor_id),
                "PoW target mismatch"
            );
            return false;
        }

        // Verify PoW is bound to this packet
        if proof_of_work.packet_id != *packet_id {
            debug!("PoW packet_id mismatch");
            return false;
        }

        // Verify PoW meets requirements
        // TODO: Make PoW config configurable per Protocol instance
        let pow_config = PoWConfig::default();
        let pow_result = verify_pow(proof_of_work, &pow_config);
        if !pow_result.is_valid() {
            debug!(result = %pow_result, "PoW verification failed");
            return false;
        }

        true
    }

    /// Send a receipt back to the sender
    async fn send_receipt(conn: &iroh::endpoint::Connection, receipt: Receipt) {
        let reply = SendMessage::Receipt(receipt);
        if let Ok(mut send) = conn.open_uni().await {
            let _ = tokio::io::AsyncWriteExt::write_all(&mut send, &reply.encode()).await;
            let _ = send.finish();
        }
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
}
