//! Share protocol incoming handler
//!
//! Handles incoming Share protocol connections:
//! - ChunkMapRequest: Respond with who has what
//! - ChunkRequest: Send requested chunks
//! - Bitfield: Exchange chunk availability
//! - PeerSuggestion handling

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::Mutex;
use tracing::{debug, info, trace, warn};

use crate::data::{BlobStore, get_blob, BlobState, CHUNK_SIZE};
use crate::network::share::protocol::{
    BitfieldMessage, ChunkMapRequest, ChunkMapResponse, ChunkRequest, ChunkResponse,
    ChunkAck, PeerChunks, ShareMessage,
};
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

            // Handle message
            let responses = Self::handle_share_message(
                &db,
                &blob_store,
                sender_id,
                our_id,
                message,
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

    /// Handle a share message and return responses (may be multiple for chunk requests)
    async fn handle_share_message(
        db: &Arc<Mutex<Connection>>,
        blob_store: &Arc<BlobStore>,
        sender_id: [u8; 32],
        our_id: [u8; 32],
        message: ShareMessage,
    ) -> Vec<ShareMessage> {
        match message {
            ShareMessage::ChunkMapRequest(req) => {
                Self::handle_chunk_map_request(db, blob_store, our_id, req)
                    .await
                    .into_iter()
                    .collect()
            }
            ShareMessage::ChunkRequest(req) => {
                // Returns multiple chunk responses
                Self::handle_chunk_request(db, blob_store, sender_id, req).await
            }
            ShareMessage::Bitfield(bitfield) => {
                Self::handle_bitfield(db, blob_store, sender_id, bitfield)
                    .await
                    .into_iter()
                    .collect()
            }
            ShareMessage::ChunkResponse(resp) => {
                Self::handle_chunk_response(db, blob_store, sender_id, resp)
                    .await
                    .into_iter()
                    .collect()
            }
            ShareMessage::PeerSuggestion(suggestion) => {
                // Log suggestion for potential use
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
        }
    }

    /// Handle ChunkMapRequest - respond with who has what
    async fn handle_chunk_map_request(
        db: &Arc<Mutex<Connection>>,
        _blob_store: &Arc<BlobStore>,
        our_id: [u8; 32],
        req: ChunkMapRequest,
    ) -> Option<ShareMessage> {
        let db_lock = db.lock().await;
        
        // Get blob metadata
        let blob = match get_blob(&db_lock, &req.hash) {
            Ok(Some(b)) => b,
            _ => return None,
        };
        
        // Get section traces
        let traces = match crate::data::get_section_traces(&db_lock, &req.hash) {
            Ok(t) => t,
            Err(_) => return None,
        };
        
        let mut peers = Vec::new();
        
        // Add peers from traces
        for trace in traces {
            if let Some(peer_id) = trace.received_from {
                peers.push(PeerChunks {
                    endpoint_id: peer_id,
                    chunk_start: trace.chunk_start,
                    chunk_end: trace.chunk_end,
                });
            }
        }
        
        // Add ourselves if we're the source and complete
        if blob.source_id == our_id && blob.state == BlobState::Complete {
            peers.push(PeerChunks {
                endpoint_id: our_id,
                chunk_start: 0,
                chunk_end: blob.total_chunks,
            });
        }
        
        Some(ShareMessage::ChunkMapResponse(ChunkMapResponse {
            hash: req.hash,
            peers,
        }))
    }

    /// Handle ChunkRequest - send requested chunks
    /// 
    /// Returns all requested chunks concatenated as multiple ChunkResponse messages
    async fn handle_chunk_request(
        db: &Arc<Mutex<Connection>>,
        blob_store: &Arc<BlobStore>,
        sender_id: [u8; 32],
        req: ChunkRequest,
    ) -> Vec<ShareMessage> {
        // Check if we have this blob
        {
            let db_lock = db.lock().await;
            match get_blob(&db_lock, &req.hash) {
                Ok(Some(_)) => {}
                _ => return Vec::new(),
            }
        };
        
        // Send all requested chunks
        let mut responses = Vec::with_capacity(req.chunks.len());
        
        for &chunk_index in &req.chunks {
            match blob_store.read_chunk(&req.hash, chunk_index) {
                Ok(data) => {
                    trace!(
                        hash = %hex::encode(&req.hash[..8]),
                        chunk = chunk_index,
                        to = %hex::encode(&sender_id[..8]),
                        "sending chunk"
                    );
                    
                    responses.push(ShareMessage::ChunkResponse(ChunkResponse {
                        hash: req.hash,
                        chunk_index,
                        data,
                    }));
                }
                Err(e) => {
                    debug!(error = %e, chunk = chunk_index, "failed to read chunk");
                    // Continue with other chunks even if one fails
                }
            }
        }
        
        if !responses.is_empty() {
            debug!(
                hash = %hex::encode(&req.hash[..8]),
                chunks = responses.len(),
                to = %hex::encode(&sender_id[..8]),
                "sending {} chunks",
                responses.len()
            );
        }
        
        responses
    }

    /// Handle incoming Bitfield - store for tracking
    async fn handle_bitfield(
        _db: &Arc<Mutex<Connection>>,
        _blob_store: &Arc<BlobStore>,
        sender_id: [u8; 32],
        bitfield: BitfieldMessage,
    ) -> Option<ShareMessage> {
        // Log the bitfield for debugging
        let chunk_count = bitfield.bitfield.len() * 8;
        trace!(
            hash = %hex::encode(&bitfield.hash[..8]),
            from = %hex::encode(&sender_id[..8]),
            chunks = chunk_count,
            "received bitfield"
        );
        
        // Could respond with our own bitfield
        // For now, just acknowledge
        None
    }

    /// Handle incoming ChunkResponse - store the chunk
    async fn handle_chunk_response(
        db: &Arc<Mutex<Connection>>,
        blob_store: &Arc<BlobStore>,
        sender_id: [u8; 32],
        resp: ChunkResponse,
    ) -> Option<ShareMessage> {
        // Get blob metadata for total_size
        let blob = {
            let db_lock = db.lock().await;
            match get_blob(&db_lock, &resp.hash) {
                Ok(Some(b)) => b,
                _ => return None,
            }
        };
        
        // Write chunk to storage
        if let Err(e) = blob_store.write_chunk(
            &resp.hash,
            resp.chunk_index,
            &resp.data,
            blob.total_size,
        ) {
            warn!(error = %e, "failed to write chunk");
            return None;
        }
        
        debug!(
            hash = %hex::encode(&resp.hash[..8]),
            chunk = resp.chunk_index,
            total = blob.total_chunks,
            from = %hex::encode(&sender_id[..8]),
            "SHARE: Received chunk"
        );
        
        // Check if blob is now complete
        if blob_store.is_complete(&resp.hash, blob.total_chunks).unwrap_or(false) {
            // Mark as complete in database
            {
                let db_lock = db.lock().await;
                if let Err(e) = crate::data::mark_blob_complete(&db_lock, &resp.hash) {
                    warn!(error = %e, "failed to mark blob complete");
                }
            }
            
            info!(
                hash = hex::encode(&resp.hash[..8]),
                name = blob.display_name,
                size = blob.total_size,
                chunks = blob.total_chunks,
                "SHARE: File complete"
            );
        }
        
        // Send acknowledgment
        Some(ShareMessage::ChunkAck(ChunkAck {
            hash: resp.hash,
            chunk_index: resp.chunk_index,
        }))
    }
}

