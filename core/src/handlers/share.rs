//! Share protocol incoming handler
//!
//! Handles incoming Share protocol connections via bidirectional and unidirectional streams.
//! Dispatches to service methods for actual processing.

use iroh::protocol::{AcceptError, ProtocolHandler};
use tracing::{debug, trace};

use crate::data::CHUNK_SIZE;
use crate::network::share::ShareService;
use crate::network::share::protocol::ShareMessage;

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
/// Uses bidirectional streams for request/response and unidirectional streams
/// for opportunistic chunk pushes.
async fn handle_share_connection(
    service: &ShareService,
    conn: iroh::endpoint::Connection,
    sender_id: [u8; 32],
) -> Result<(), crate::protocol::ProtocolError> {
    trace!(sender = %hex::encode(&sender_id[..8]), "handling Share connection");

    loop {
        tokio::select! {
            bi = conn.accept_bi() => {
                let (mut send, mut recv) = match bi {
                    Ok(streams) => streams,
                    Err(e) => {
                        trace!(error = %e, "bidirectional stream accept ended");
                        break;
                    }
                };

                let buf = match recv.read_to_end(MAX_READ_SIZE).await {
                    Ok(data) => data,
                    Err(e) => {
                        debug!(error = %e, "failed to read bidirectional stream");
                        continue;
                    }
                };

                let message = match ShareMessage::decode(&buf) {
                    Ok(m) => m,
                    Err(e) => {
                        debug!(error = %e, "failed to decode share message");
                        continue;
                    }
                };

                let responses = dispatch_share_message(service, sender_id, message).await;
                if let Err(e) = write_share_responses(&mut send, responses).await {
                    debug!(error = %e, "failed to send share responses");
                }
            }
            uni = conn.accept_uni() => {
                let mut recv = match uni {
                    Ok(stream) => stream,
                    Err(e) => {
                        trace!(error = %e, "unidirectional stream accept ended");
                        break;
                    }
                };

                let buf = match recv.read_to_end(MAX_READ_SIZE).await {
                    Ok(data) => data,
                    Err(e) => {
                        debug!(error = %e, "failed to read unidirectional stream");
                        continue;
                    }
                };

                let message = match ShareMessage::decode(&buf) {
                    Ok(m) => m,
                    Err(e) => {
                        debug!(error = %e, "failed to decode share message");
                        continue;
                    }
                };

                if !supports_uni_dispatch(&message) {
                    debug!(
                        msg = ?message,
                        "ignoring unidirectional Share message that requires bidirectional response path"
                    );
                    continue;
                }

                let responses = dispatch_share_message(service, sender_id, message).await;
                if !responses.is_empty() {
                    trace!(
                        response_count = responses.len(),
                        "dropping Share responses for unidirectional stream"
                    );
                }
            }
        }
    }

    Ok(())
}

fn supports_uni_dispatch(message: &ShareMessage) -> bool {
    matches!(
        message,
        ShareMessage::ChunkResponse(_)
            | ShareMessage::Bitfield(_)
            | ShareMessage::PeerSuggestion(_)
            | ShareMessage::ChunkAck(_)
    )
}

async fn dispatch_share_message(
    service: &ShareService,
    sender_id: [u8; 32],
    message: ShareMessage,
) -> Vec<ShareMessage> {
    match message {
        ShareMessage::ChunkMapRequest(req) => {
            service.handle_chunk_map_request(req, sender_id).await
        }
        ShareMessage::ChunkRequest(req) => service.handle_chunk_request(req, sender_id).await,
        ShareMessage::ChunkResponse(resp) => service.handle_chunk_response(resp, sender_id).await,
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
            // Response message, shouldn't receive this as a request.
            Vec::new()
        }
    }
}

async fn write_share_responses(
    send: &mut iroh::endpoint::SendStream,
    responses: Vec<ShareMessage>,
) -> Result<(), std::io::Error> {
    if responses.is_empty() {
        return Ok(());
    }

    for response_msg in responses {
        let response_bytes = response_msg.encode();
        tokio::io::AsyncWriteExt::write_all(send, &response_bytes).await?;
    }
    send.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::share::protocol::{ChunkMapRequest, ChunkResponse};

    #[test]
    fn test_supports_uni_dispatch_chunk_response() {
        let message = ShareMessage::ChunkResponse(ChunkResponse {
            hash: [1u8; 32],
            chunk_index: 0,
            data: vec![1, 2, 3],
        });
        assert!(supports_uni_dispatch(&message));
    }

    #[test]
    fn test_supports_uni_dispatch_rejects_request_types() {
        let message = ShareMessage::ChunkMapRequest(ChunkMapRequest { hash: [2u8; 32] });
        assert!(!supports_uni_dispatch(&message));
    }
}
