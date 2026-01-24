//! Sync protocol incoming handler
//!
//! Handles direct SYNC_ALPN connections for sync transport:
//! - SyncRequest: peer requesting full sync state
//! - SyncResponse: peer providing full sync state
//!
//! This is a pure transport handler - it emits events for the app layer
//! to process. The app layer is responsible for:
//! - Maintaining its own CRDT state
//! - Responding to SyncRequest events via Protocol::respond_sync()
//!
//! The handler is responsible for:
//! - Accepting streams and reading messages
//! - Delegating business logic to the service layer

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::network::sync::service::process_incoming_sync_message;
use crate::protocol::{Protocol, ProtocolEvent, ProtocolError};

impl Protocol {
    /// Handle incoming direct sync connection (SyncRequest or SyncResponse)
    ///
    /// Emits events for the app layer to handle:
    /// - SyncRequestEvent: app should call respond_sync() with its CRDT state
    /// - SyncResponseEvent: app should merge the received CRDT state
    pub(crate) async fn handle_sync_connection(
        conn: iroh::endpoint::Connection,
        _endpoint: iroh::Endpoint,
        event_tx: mpsc::Sender<ProtocolEvent>,
        sender_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
        // Maximum snapshot size (100MB - generous for large documents)
        const MAX_READ_SIZE: usize = 100 * 1024 * 1024;

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
            let buf = match recv.read_to_end(MAX_READ_SIZE).await {
                Ok(data) => data,
                Err(e) => {
                    warn!(error = %e, sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: failed to read stream");
                    continue;
                }
            };

            // Delegate all business logic (parsing, validation, event emission) to service
            if let Err(e) = process_incoming_sync_message(&buf, sender_id, &event_tx).await {
                debug!(error = %e, "SYNC_HANDLER: failed to process message");
            }
        }

        Ok(())
    }
}
