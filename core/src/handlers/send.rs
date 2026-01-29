//! Send protocol incoming handler
//!
//! Handles incoming Send protocol connections via irpc:
//! - DeliverPacket RPC: receives packet, processes it, responds with Receipt
//!
//! The handler is responsible for:
//! - Reading irpc requests from the connection
//! - Finding matching topic for packets
//! - Delegating business logic to the service layer
//! - Responding with receipts via irpc

use iroh::protocol::{AcceptError, ProtocolHandler};
use tracing::{debug, trace};

use crate::data::get_topics_for_member;
use crate::network::send::protocol::{Receipt, SendRpcMessage, SendRpcProtocol};
use crate::network::send::{ProcessError, PacketSource};
use crate::security::harbor_id_from_topic;

use crate::network::send::SendService;
use crate::protocol::ProtocolError;

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
    /// Processes DeliverPacket requests and responds with Receipts.
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
                SendRpcMessage::DeliverPacket(deliver_msg) => {
                    let packet = deliver_msg.packet.clone();

                    trace!(
                        harbor_id = %hex::encode(packet.harbor_id),
                        sender = %hex::encode(&sender_id[..8]),
                        "received DeliverPacket request"
                    );

                    // Check if this is a DM packet
                    if packet.is_dm() {
                        let result = self.process_incoming_dm_packet(&packet).await;
                        let response = match result {
                            Ok(r) => r.receipt,
                            Err(e) => {
                                debug!(error = %e, "DM packet processing error");
                                Receipt::new(packet.packet_id, our_id)
                            }
                        };
                        deliver_msg.tx.send(response).await.ok();
                        continue;
                    }

                    // Find which topic this packet belongs to
                    let topics = {
                        let db_lock = db.lock().await;
                        get_topics_for_member(&db_lock, &our_id)
                            .map_err(|e| ProtocolError::Database(e.to_string()))?
                    };

                    // Try each topic we're subscribed to
                    let mut receipt = None;
                    for topic_id in topics {
                        let harbor_id = harbor_id_from_topic(&topic_id);
                        if packet.harbor_id != harbor_id {
                            continue;
                        }

                        trace!(topic = %hex::encode(&topic_id[..8]), "found matching topic for packet");

                        // Delegate business logic to the service layer
                        match self.process_incoming_packet(&packet, &topic_id, sender_id, PacketSource::Direct).await {
                            Ok(result) => {
                                receipt = Some(result.receipt);
                                break;
                            }
                            Err(ProcessError::VerificationFailed(e)) => {
                                trace!(error = %e, topic = %hex::encode(topic_id), "verification failed for topic");
                                continue;
                            }
                            Err(e) => {
                                debug!(error = %e, "packet processing error");
                                continue;
                            }
                        }
                    }

                    // Respond with receipt (or a default "unknown topic" receipt)
                    let response = receipt.unwrap_or_else(|| {
                        debug!(harbor_id = hex::encode(packet.harbor_id), "packet for unknown topic");
                        Receipt::new(packet.packet_id, our_id)
                    });

                    // Send response via irpc oneshot
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
