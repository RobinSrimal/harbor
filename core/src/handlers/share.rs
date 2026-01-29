//! Share protocol incoming handler
//!
//! Handles incoming Share protocol connections:
//! - ChunkMapRequest: Respond with who has what
//! - ChunkRequest: Send requested chunk
//! - Bitfield: Exchange chunk availability
//! - PeerSuggestion handling
//!
//! The handler is responsible for:
//! - Accepting streams and decoding messages
//! - Delegating business logic to the service layer
//! - Sending responses

use iroh::protocol::{AcceptError, ProtocolHandler};
use tracing::{debug, trace};

use crate::data::CHUNK_SIZE;
use crate::network::share::protocol::ShareMessage;
use crate::network::share::service::ShareService;

/// Maximum message size for share protocol (chunk size + overhead)
const MAX_READ_SIZE: usize = (CHUNK_SIZE as usize) + 1024;

impl ProtocolHandler for ShareService {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let sender_id = *conn.remote_id().as_bytes();
        if let Err(e) = self.handle_share_connection(conn, sender_id).await {
            debug!(error = %e, sender = %hex::encode(sender_id), "Share connection handler error");
        }
        Ok(())
    }
}

impl ShareService {
    /// Handle a single incoming Share protocol connection
    pub(crate) async fn handle_share_connection(
        &self,
        conn: iroh::endpoint::Connection,
        sender_id: [u8; 32],
    ) -> Result<(), crate::protocol::ProtocolError> {
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
            let responses = self.process_incoming_message(
                message,
                sender_id,
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
