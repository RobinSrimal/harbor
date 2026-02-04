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

use crate::data::get_topics_for_member;
use crate::network::send::protocol::{Receipt, SendRpcMessage, SendRpcProtocol, verify_membership_proof};
use crate::network::send::SendService;
use crate::protocol::ProtocolError;
use crate::security::harbor_id_from_topic;

impl ProtocolHandler for SendService {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let sender_id = *conn.remote_id().as_bytes();
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
                    trace!(
                        harbor_id = %hex::encode(&deliver_msg.harbor_id[..8]),
                        sender = %hex::encode(&sender_id[..8]),
                        payload_len = deliver_msg.payload.len(),
                        "received DeliverTopic (direct delivery)"
                    );

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
                        if expected_harbor_id == deliver_msg.harbor_id {
                            // Verify membership proof: sender must know the topic_id
                            if verify_membership_proof(
                                topic_id,
                                &deliver_msg.harbor_id,
                                &sender_id,
                                &deliver_msg.membership_proof,
                            ) {
                                matched_topic = Some(*topic_id);
                                break;
                            } else {
                                warn!(
                                    harbor_id = %hex::encode(&deliver_msg.harbor_id[..8]),
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
                                harbor_id = %hex::encode(&deliver_msg.harbor_id[..8]),
                                "DeliverTopic for unknown topic or invalid proof"
                            );
                            let dummy_packet_id = [0u8; 32];
                            deliver_msg.tx.send(Receipt::new(dummy_packet_id, our_id)).await.ok();
                            continue;
                        }
                    };

                    // Process raw TopicMessage payload (already plaintext from QUIC TLS)
                    let result = self.process_raw_topic_payload(&topic_id, sender_id, &deliver_msg.payload).await;
                    let response = match result {
                        Ok(r) => r.receipt,
                        Err(e) => {
                            debug!(error = %e, "DeliverTopic processing error");
                            let dummy_packet_id = [0u8; 32];
                            Receipt::new(dummy_packet_id, our_id)
                        }
                    };
                    deliver_msg.tx.send(response).await.ok();
                }

                SendRpcMessage::DeliverDm(deliver_msg) => {
                    trace!(
                        sender = %hex::encode(&sender_id[..8]),
                        payload_len = deliver_msg.payload.len(),
                        "received DeliverDm (direct delivery)"
                    );

                    // Process raw DmMessage payload (already plaintext from QUIC TLS)
                    let result = self.process_raw_dm_payload(sender_id, &deliver_msg.payload).await;
                    let response = match result {
                        Ok(r) => r.receipt,
                        Err(e) => {
                            debug!(error = %e, "DeliverDm processing error");
                            let dummy_packet_id = [0u8; 32];
                            Receipt::new(dummy_packet_id, our_id)
                        }
                    };
                    deliver_msg.tx.send(response).await.ok();
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
