//! Share protocol incoming handler
//!
//! Handles incoming Share protocol connections:
//! - ChunkMapRequest: Respond with who has what
//! - ChunkRequest: Send requested chunks
//! - Bitfield: Exchange chunk availability
//! - PeerSuggestion handling
//!
//! The handler is responsible for:
//! - Accepting streams and decoding messages
//! - Delegating business logic to the service layer
//! - Sending responses

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::Mutex;
use tracing::{debug, trace};

use crate::data::{BlobStore, CHUNK_SIZE};
use crate::network::share::protocol::ShareMessage;
use crate::network::share::service::process_incoming_share_message;
use crate::protocol::{Protocol, ProtocolError};

/// Maximum message size for share protocol (chunk size + overhead)
const MAX_READ_SIZE: usize = (CHUNK_SIZE as usize) + 1024;

impl Protocol {
    /// Handle a single incoming Share protocol connection
    pub(crate) async fn handle_share_connection(
        conn: iroh::endpoint::Connection,
        db: Arc<Mutex<Connection>>,
        blob_store: Arc<BlobStore>,
        sender_id: [u8; 32],
        our_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
        trace!(sender = %hex::encode(sender_id), "share connection established");

        loop {
            // Accept bidirectional stream for request/response
            let (mut send, mut recv) = match conn.accept_bi().await {
                Ok(streams) => streams,
                Err(e) => {
                    trace!(error = %e, "stream accept ended");
                    break;
                }
            };

            // Read message
            let buf = match recv.read_to_end(MAX_READ_SIZE).await {
                Ok(data) => data,
                Err(e) => {
                    debug!(error = %e, "failed to read stream");
                    continue;
                }
            };

            // Decode message
            let message = match ShareMessage::decode(&buf) {
                Ok(m) => m,
                Err(e) => {
                    debug!(error = %e, "failed to decode share message");
                    continue;
                }
            };

            // Delegate business logic to service layer
            let responses = process_incoming_share_message(
                message,
                sender_id,
                our_id,
                &db,
                &blob_store,
            ).await;

            // Send all responses
            if !responses.is_empty() {
                for response_msg in responses {
                    let response_bytes = response_msg.encode();
                    if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut send, &response_bytes).await {
                        debug!(error = %e, "failed to send response");
                        break;
                    }
                }
                let _ = send.finish();
            }
        }

        Ok(())
    }
}
