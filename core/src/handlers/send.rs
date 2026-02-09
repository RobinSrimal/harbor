//! Send protocol incoming handler
//!
//! Handles incoming Send protocol connections via irpc:
//! - DeliverTopic: receives raw topic payload, processes it, responds with Receipt
//! - DeliverDm: receives raw DM payload, processes it, responds with Receipt
//!
//! Direct delivery uses QUIC TLS for encryption - no app-level crypto needed.
//! The handler dispatches to service methods for actual processing.

use iroh::protocol::{AcceptError, ProtocolHandler};
use tracing::{debug, trace};

use crate::network::send::protocol::{SendRpcMessage, SendRpcProtocol};
use crate::network::send::SendService;
use crate::protocol::ProtocolError;

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
    /// Dispatches DeliverTopic and DeliverDm requests to service methods.
    pub(crate) async fn handle_send_connection(
        &self,
        conn: iroh::endpoint::Connection,
        sender_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
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

            // Dispatch to service methods
            match msg {
                SendRpcMessage::DeliverTopic(deliver_msg) => {
                    let response = self.handle_deliver_topic(&deliver_msg, sender_id).await;
                    deliver_msg.tx.send(response).await.ok();
                }
                SendRpcMessage::DeliverDm(deliver_msg) => {
                    let response = self.handle_deliver_dm(&deliver_msg, sender_id).await;
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
