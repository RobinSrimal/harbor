//! Send protocol incoming handler
//!
//! Handles incoming Send protocol connections via irpc:
//! - DeliverTopic: receives raw topic payload, processes it, responds with Receipt
//! - DeliverDm: receives raw DM payload, processes it, responds with Receipt
//!
//! Direct delivery uses QUIC TLS for encryption - no app-level crypto needed.
//! The handler verifies topic membership and delegates processing to the service layer.

use iroh::protocol::{AcceptError, ProtocolHandler};
use tracing::{debug, trace, warn};

use crate::data::{get_topics_for_member, dedup_check_and_mark, DedupResult};
use crate::network::send::protocol::{Receipt, SendRpcMessage, SendRpcProtocol, verify_membership_proof};
use crate::network::send::SendService;
use crate::protocol::ProtocolError;
use crate::security::harbor_id_from_topic;

impl ProtocolHandler for SendService {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let sender_id = *conn.remote_id().as_bytes();

        // Connection gate check - must be a connected peer or share a topic
        if let Some(gate) = self.connection_gate() {
            if !gate.is_allowed(&sender_id).await {
                trace!(sender = %hex::encode(sender_id), "SEND: rejected connection from non-connected peer");
                return Ok(()); // Silent drop
            }
        }

        if let Err(e) = self.handle_send_connection(conn, sender_id).await {
            debug!(error = %e, sender = %hex::encode(sender_id), "Send connection handler error");
        }
        Ok(())
    }
}

impl SendService {
    /// Handle a single incoming Send protocol connection
    ///
    /// Uses irpc for wire framing (varint length-prefix + postcard).
    /// Processes DeliverTopic and DeliverDm requests, responds with Receipts.
    pub(crate) async fn handle_send_connection(
        &self,
        conn: iroh::endpoint::Connection,
        sender_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
        let our_id = self.endpoint_id();
        let db = self.db().clone();
        trace!(sender = %hex::encode(sender_id), "handling Send connection");

        loop {
            // Read next request using irpc framing
            let msg = match irpc_iroh::read_request::<SendRpcProtocol>(&conn).await {
                Ok(Some(msg)) => msg,
                Ok(None) => {
                    trace!("Send connection closed normally");
                    break;
                }
                Err(e) => {
                    debug!(error = %e, "Send read_request error");
                    break;
                }
            };

            match msg {
                // =====================================================================
                // Direct delivery: raw payload over QUIC TLS (no app-level crypto)
                // =====================================================================
                SendRpcMessage::DeliverTopic(deliver_msg) => {
                    // Extract fields before consuming tx
                    let packet_id = deliver_msg.packet_id;
                    let harbor_id = deliver_msg.harbor_id;
                    let membership_proof = deliver_msg.membership_proof;
                    let payload = deliver_msg.payload.clone();
                    let pow = deliver_msg.pow.clone();
                    let tx = deliver_msg.tx;

                    trace!(
                        harbor_id = %hex::encode(&harbor_id[..8]),
                        sender = %hex::encode(&sender_id[..8]),
                        payload_len = payload.len(),
                        "received DeliverTopic (direct delivery)"
                    );

                    // Verify PoW first (context: sender_id || harbor_id)
                    if !self.verify_topic_pow(&pow, &sender_id, &harbor_id, payload.len()) {
                        debug!(
                            sender = %hex::encode(&sender_id[..8]),
                            harbor_id = %hex::encode(&harbor_id[..8]),
                            "DeliverTopic rejected: insufficient PoW"
                        );
                        tx.send(Receipt::new(packet_id, our_id)).await.ok();
                        continue;
                    }

                    // Find which topic this message belongs to by matching harbor_id
                    let topics = {
                        let db_lock = db.lock().await;
                        get_topics_for_member(&db_lock, &our_id)
                            .map_err(|e| ProtocolError::Database(e.to_string()))?
                    };

                    // Find matching topic and verify membership proof
                    let mut matched_topic: Option<[u8; 32]> = None;
                    for topic_id in &topics {
                        let expected_harbor_id = harbor_id_from_topic(topic_id);
                        if expected_harbor_id == harbor_id {
                            // Verify membership proof: sender must know the topic_id
                            if verify_membership_proof(topic_id, &harbor_id, &sender_id, &membership_proof) {
                                matched_topic = Some(*topic_id);
                                break;
                            } else {
                                warn!(
                                    harbor_id = %hex::encode(&harbor_id[..8]),
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
                                harbor_id = %hex::encode(&harbor_id[..8]),
                                "DeliverTopic for unknown topic or invalid proof"
                            );
                            tx.send(Receipt::new(packet_id, our_id)).await.ok();
                            continue;
                        }
                    };

                    // Deduplication check - skip if already seen
                    let dedup_result = {
                        let db_lock = db.lock().await;
                        dedup_check_and_mark(&db_lock, &topic_id, &packet_id, &sender_id, &our_id)
                            .unwrap_or(DedupResult::Process)
                    };

                    if !dedup_result.should_process() {
                        trace!(
                            packet_id = %hex::encode(&packet_id[..8]),
                            result = %dedup_result,
                            "DeliverTopic skipped (dedup)"
                        );
                        // Still send receipt so sender knows we got it
                        tx.send(Receipt::new(packet_id, our_id)).await.ok();
                        continue;
                    }

                    // Process raw TopicMessage payload (already plaintext from QUIC TLS)
                    let result = self.process_raw_topic_payload_with_id(&topic_id, sender_id, &payload, packet_id).await;
                    let response = match result {
                        Ok(r) => r.receipt,
                        Err(e) => {
                            debug!(error = %e, "DeliverTopic processing error");
                            Receipt::new(packet_id, our_id)
                        }
                    };
                    tx.send(response).await.ok();
                }

                SendRpcMessage::DeliverDm(deliver_msg) => {
                    // Extract fields before consuming tx
                    let packet_id = deliver_msg.packet_id;
                    let payload = deliver_msg.payload.clone();
                    let pow = deliver_msg.pow.clone();
                    let tx = deliver_msg.tx;

                    trace!(
                        sender = %hex::encode(&sender_id[..8]),
                        payload_len = payload.len(),
                        "received DeliverDm (direct delivery)"
                    );

                    // Verify PoW first (context: sender_id || recipient_id)
                    if !self.verify_dm_pow(&pow, &sender_id, &our_id, payload.len()) {
                        debug!(
                            sender = %hex::encode(&sender_id[..8]),
                            "DeliverDm rejected: insufficient PoW"
                        );
                        tx.send(Receipt::new(packet_id, our_id)).await.ok();
                        continue;
                    }

                    // Deduplication check - for DMs, use our_id as "topic" (DMs are addressed to us)
                    let dedup_result = {
                        let db_lock = db.lock().await;
                        dedup_check_and_mark(&db_lock, &our_id, &packet_id, &sender_id, &our_id)
                            .unwrap_or(DedupResult::Process)
                    };

                    if !dedup_result.should_process() {
                        trace!(
                            packet_id = %hex::encode(&packet_id[..8]),
                            result = %dedup_result,
                            "DeliverDm skipped (dedup)"
                        );
                        // Still send receipt so sender knows we got it
                        tx.send(Receipt::new(packet_id, our_id)).await.ok();
                        continue;
                    }

                    // Process raw DmMessage payload (already plaintext from QUIC TLS)
                    let result = self.process_raw_dm_payload_with_id(sender_id, &payload, packet_id).await;
                    let response = match result {
                        Ok(r) => r.receipt,
                        Err(e) => {
                            debug!(error = %e, "DeliverDm processing error");
                            Receipt::new(packet_id, our_id)
                        }
                    };
                    tx.send(response).await.ok();
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::network::send::protocol::Receipt;

    fn make_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_receipt_creation() {
        let receipt = Receipt::new(make_id(1), make_id(2));
        assert_eq!(receipt.packet_id, make_id(1));
        assert_eq!(receipt.sender, make_id(2));
    }

    #[test]
    fn test_receipt_serialization() {
        let receipt = Receipt::new(make_id(1), make_id(2));
        let bytes = postcard::to_allocvec(&receipt).unwrap();
        let decoded: Receipt = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, receipt);
    }
}
