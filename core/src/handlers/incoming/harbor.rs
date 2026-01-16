//! Harbor protocol incoming handler
//!
//! Handles incoming Harbor protocol connections:
//! - Store requests (store packets for offline delivery)
//! - Pull requests (return packets for a recipient)
//! - Ack messages (mark packets as delivered)
//! - Sync requests (Harbor-to-Harbor synchronization)

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::Mutex;
use tracing::{debug, trace, warn};

use crate::network::harbor::protocol::HarborMessage;
use crate::network::harbor::service::HarborService;

use crate::protocol::{Protocol, ProtocolError};

impl Protocol {
    /// Handle an incoming Harbor protocol connection
    /// 
    /// Processes Store, Pull, Ack, and Sync requests from other nodes.
    /// This allows this node to act as a Harbor Node (store packets for others).
    pub(crate) async fn handle_harbor_connection(
        conn: iroh::endpoint::Connection,
        db: Arc<Mutex<Connection>>,
        sender_id: [u8; 32],
        our_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
        trace!(sender = %hex::encode(sender_id), "handling Harbor connection");

        // Create a Harbor service for this connection
        let service = HarborService::new(our_id);

        loop {
            // Accept unidirectional stream for request
            let mut recv = match conn.accept_uni().await {
                Ok(r) => r,
                Err(e) => {
                    trace!(error = %e, "Harbor stream accept ended");
                    break;
                }
            };

            // Read message with size limit (1MB should be plenty for Harbor messages)
            const MAX_HARBOR_MSG_SIZE: usize = 1024 * 1024;
            let buf = match recv.read_to_end(MAX_HARBOR_MSG_SIZE).await {
                Ok(data) => data,
                Err(e) => {
                    debug!(error = %e, "failed to read Harbor message");
                    continue;
                }
            };

            trace!(bytes = buf.len(), "read Harbor message");

            // Decode message
            let message = match HarborMessage::decode(&buf) {
                Ok(m) => m,
                Err(e) => {
                    debug!(error = %e, "failed to decode Harbor message");
                    continue;
                }
            };

            trace!("decoded HarborMessage: {:?}", std::mem::discriminant(&message));

            // Process with HarborService
            let response = {
                let mut db_lock = db.lock().await;
                match service.handle_message(&mut db_lock, message, &sender_id) {
                    Ok(resp) => resp,
                    Err(e) => {
                        warn!(error = %e, "Harbor message handling failed");
                        // Send error response for Store requests
                        // Other request types may need specific error handling
                        continue;
                    }
                }
            };

            // Send response if there is one
            if let Some(resp_msg) = response {
                let resp_bytes = resp_msg.encode();
                
                if let Ok(mut send) = conn.open_uni().await {
                    if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut send, &resp_bytes).await {
                        debug!(error = %e, "failed to send Harbor response");
                        continue;
                    }
                    if let Err(e) = send.finish() {
                        debug!(error = %e, "failed to finish Harbor response stream");
                    }
                } else {
                    debug!("failed to open response stream");
                }
            }
        }

        Ok(())
    }
}

