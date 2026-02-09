//! Harbor protocol incoming handler
//!
//! Handles incoming Harbor protocol connections:
//! - Store requests (storing packets for offline recipients)
//! - Pull requests (retrieving missed packets)
//! - Acknowledgments (marking packets as delivered)
//! - Sync requests (Harbor-to-Harbor synchronization)

use iroh::protocol::{AcceptError, ProtocolHandler};
use tracing::{debug, trace, warn};

use crate::network::harbor::protocol::HarborMessage;
use crate::network::harbor::HarborService;
use crate::protocol::ProtocolError;

impl ProtocolHandler for HarborService {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let sender_id = *conn.remote_id().as_bytes();
        if let Err(e) = self.handle_harbor_connection(conn, sender_id).await {
            debug!(error = %e, sender = %hex::encode(sender_id), "Harbor connection handler error");
        }
        Ok(())
    }
}

/// Send a response message on the connection
async fn send_response(conn: &iroh::endpoint::Connection, msg: &HarborMessage) {
    let bytes = msg.encode();
    if let Ok(mut send) = conn.open_uni().await {
        if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut send, &bytes).await {
            debug!(error = %e, "failed to send Harbor response");
            return;
        }
        if let Err(e) = send.finish() {
            debug!(error = %e, "failed to finish Harbor response stream");
        }
    } else {
        debug!("failed to open response stream");
    }
}

impl HarborService {
    /// Handle an incoming Harbor protocol connection
    ///
    /// Processes Store, Pull, Ack, and Sync requests from other nodes.
    /// Dispatches each message variant to the appropriate handler in `incoming`.
    pub(crate) async fn handle_harbor_connection(
        &self,
        conn: iroh::endpoint::Connection,
        sender_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
        trace!(sender = %hex::encode(sender_id), "handling Harbor connection");

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

            // Dispatch to handlers
            let db = self.db();
            match message {
                HarborMessage::Store(req) => {
                    let mut db_lock = db.lock().await;
                    match self.handle_store(&mut db_lock, req) {
                        Ok(resp) => {
                            send_response(&conn, &HarborMessage::StoreResponse(resp)).await;
                        }
                        Err(e) => {
                            warn!(error = %e, "Harbor store failed");
                        }
                    }
                }
                HarborMessage::Pull(req) => {
                    let db_lock = db.lock().await;
                    match self.handle_pull(&db_lock, req, &sender_id) {
                        Ok(resp) => {
                            send_response(&conn, &HarborMessage::PullResponse(resp)).await;
                        }
                        Err(e) => {
                            warn!(error = %e, "Harbor pull failed");
                        }
                    }
                }
                HarborMessage::Ack(ack) => {
                    let db_lock = db.lock().await;
                    if let Err(e) = self.handle_ack(&db_lock, ack, &sender_id) {
                        warn!(error = %e, "Harbor ack failed");
                    }
                }
                HarborMessage::SyncRequest(req) => {
                    let db_lock = db.lock().await;
                    match self.handle_sync(&db_lock, req, &sender_id) {
                        Ok(resp) => {
                            send_response(&conn, &HarborMessage::SyncResponse(resp)).await;
                        }
                        Err(e) => {
                            warn!(error = %e, "Harbor sync failed");
                        }
                    }
                }
                HarborMessage::StoreResponse(_)
                | HarborMessage::PullResponse(_)
                | HarborMessage::SyncResponse(_) => {
                    debug!("Harbor: ignoring response message from peer");
                }
            }
        }

        Ok(())
    }
}
