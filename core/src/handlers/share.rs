//! Share protocol incoming handler
//!
//! Handles incoming Share protocol connections via bidirectional streams.
//! Dispatches to service methods for actual processing.

use iroh::protocol::{AcceptError, ProtocolHandler};
use tracing::{debug, trace};

use crate::data::CHUNK_SIZE;
use crate::network::share::protocol::ShareMessage;
use crate::network::share::ShareService;

/// Maximum message size for share protocol (chunk size + overhead)
const MAX_READ_SIZE: usize = (CHUNK_SIZE as usize) + 1024;

impl ProtocolHandler for ShareService {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let sender_id = *conn.remote_id().as_bytes();

        // Connection gate check - must be a connected peer or share a topic
        if let Some(gate) = self.connection_gate() {
            if !gate.is_allowed(&sender_id).await {
                trace!(sender = %hex::encode(sender_id), "SHARE: rejected connection from non-connected peer");
                return Ok(()); // Silent drop
            }
        }

        if let Err(e) = handle_share_connection(self, conn, sender_id).await {
            debug!(error = %e, sender = %hex::encode(sender_id), "Share connection handler error");
        }
        Ok(())
    }
}

/// Handle a single incoming Share protocol connection
///
/// Uses bidirectional streams for request/response.
/// Dispatches share messages to service methods.
async fn handle_share_connection(
    service: &ShareService,
    conn: iroh::endpoint::Connection,
    sender_id: [u8; 32],
) -> Result<(), crate::protocol::ProtocolError> {
    trace!(sender = %hex::encode(&sender_id[..8]), "handling Share connection");

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

        // Dispatch to service methods
        let responses = match message {
            ShareMessage::ChunkMapRequest(req) => {
                service.handle_chunk_map_request(req, sender_id).await
            }
            ShareMessage::ChunkRequest(req) => {
                service.handle_chunk_request(req, sender_id).await
            }
            ShareMessage::ChunkResponse(resp) => {
                service.handle_chunk_response(resp, sender_id).await
            }
            ShareMessage::Bitfield(msg) => {
                service.handle_bitfield(msg, sender_id).await;
                Vec::new()
            }
            ShareMessage::PeerSuggestion(suggestion) => {
                debug!(
                    hash = %hex::encode(&suggestion.hash[..8]),
                    section = suggestion.section_id,
                    suggested = %hex::encode(&suggestion.suggested_peer[..8]),
                    "received peer suggestion"
                );
                Vec::new()
            }
            ShareMessage::ChunkAck(ack) => {
                trace!(
                    hash = %hex::encode(&ack.hash[..8]),
                    chunk = ack.chunk_index,
                    "received chunk ack"
                );
                Vec::new()
            }
            ShareMessage::ChunkMapResponse(_) => {
                // Response message, shouldn't receive this as a request
                Vec::new()
            }
        };

        // Send all responses
        if !responses.is_empty() {
            for response_msg in responses {
                let response_bytes = response_msg.encode();
                if let Err(e) =
                    tokio::io::AsyncWriteExt::write_all(&mut send, &response_bytes).await
                {
                    debug!(error = %e, "failed to send response");
                    break;
                }
            }
            let _ = send.finish();
        }
    }

    Ok(())
}
