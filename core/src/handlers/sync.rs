//! Sync protocol incoming handler
//!
//! Handles incoming SYNC_ALPN connections for sync transport:
//! - SyncRequest: peer requesting full sync state
//! - SyncResponse: peer providing full sync state
//!
//! This is a pure transport handler - it emits events for the app layer
//! to process. The app layer is responsible for:
//! - Maintaining its own CRDT state
//! - Responding to SyncRequest events via Protocol::respond_sync()

use iroh::protocol::{AcceptError, ProtocolHandler};
use tracing::{debug, info, warn};

use crate::network::sync::service::SyncService;
use crate::protocol::ProtocolError;

impl ProtocolHandler for SyncService {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let sender_id = *conn.remote_id().as_bytes();
        if let Err(e) = self.handle_sync_connection(conn, sender_id).await {
            debug!(error = %e, sender = %hex::encode(sender_id), "Sync connection handler error");
        }
        Ok(())
    }
}

impl SyncService {
    /// Handle an incoming direct sync connection (SyncRequest or SyncResponse)
    ///
    /// Emits events for the app layer to handle:
    /// - SyncRequestEvent: app should call respond_sync() with its CRDT state
    /// - SyncResponseEvent: app should merge the received CRDT state
    pub async fn handle_sync_connection(
        &self,
        conn: iroh::endpoint::Connection,
        sender_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
        info!(sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: waiting for sync streams");

        loop {
            // Accept unidirectional stream
            let mut recv = match conn.accept_uni().await {
                Ok(r) => r,
                Err(e) => {
                    debug!(error = %e, sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: accept_uni ended");
                    break;
                }
            };

            // Read message
            let buf = match recv.read_to_end(self.config().max_response_size).await {
                Ok(data) => data,
                Err(e) => {
                    warn!(error = %e, sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: failed to read stream");
                    continue;
                }
            };

            // Delegate all business logic (parsing, validation, event emission) to service
            if let Err(e) = self.process_incoming_message(&buf, sender_id).await {
                debug!(error = %e, "SYNC_HANDLER: failed to process message");
            }
        }

        Ok(())
    }
}
