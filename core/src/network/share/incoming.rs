//! Incoming message handlers for ShareService
//!
//! Processes requests from peers for:
//! - Chunk maps (who has what)
//! - Individual chunks
//! - Chunk responses (when we're pulling)
//! - Bitfield exchanges (unused)

use tokio::sync::Mutex;
use tracing::{debug, info, trace, warn};

use crate::data::{get_blob, get_section_traces, mark_blob_complete, BlobState, BlobStore};
use rusqlite::Connection as DbConnection;

use super::protocol::{
    BitfieldMessage, ChunkAck, ChunkMapRequest, ChunkMapResponse, ChunkRequest, ChunkResponse,
    PeerChunks, ShareMessage,
};
use super::service::ShareService;

impl ShareService {
    /// Process an incoming share message - delegate to appropriate handler
    ///
    /// This should be called by the handler after reading a message.
    /// Returns response messages to send back to the peer.
    pub async fn process_incoming_message(
        &self,
        message: ShareMessage,
        sender_id: [u8; 32],
    ) -> Vec<ShareMessage> {
        let our_id = self.endpoint_id();
        let db = self.db();
        let blob_store = self.blob_store();

        match message {
            ShareMessage::ChunkMapRequest(req) => {
                handle_chunk_map_request(db, blob_store, our_id, req)
                    .await
                    .into_iter()
                    .collect()
            }
            ShareMessage::ChunkRequest(req) => {
                handle_chunk_request(db, blob_store, sender_id, req).await
            }
            ShareMessage::Bitfield(bitfield) => {
                handle_bitfield(sender_id, bitfield);
                Vec::new()
            }
            ShareMessage::ChunkResponse(resp) => {
                handle_chunk_response(db, blob_store, sender_id, resp)
                    .await
                    .into_iter()
                    .collect()
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
        }
    }

    /// Handle ChunkMapRequest - called directly by handler
    pub async fn handle_chunk_map_request(
        &self,
        req: ChunkMapRequest,
        _sender_id: [u8; 32],
    ) -> Vec<ShareMessage> {
        handle_chunk_map_request(self.db(), self.blob_store(), self.endpoint_id(), req)
            .await
            .into_iter()
            .collect()
    }

    /// Handle ChunkRequest - called directly by handler
    pub async fn handle_chunk_request(
        &self,
        req: ChunkRequest,
        sender_id: [u8; 32],
    ) -> Vec<ShareMessage> {
        handle_chunk_request(self.db(), self.blob_store(), sender_id, req).await
    }

    /// Handle ChunkResponse - called directly by handler
    pub async fn handle_chunk_response(
        &self,
        resp: ChunkResponse,
        sender_id: [u8; 32],
    ) -> Vec<ShareMessage> {
        handle_chunk_response(self.db(), self.blob_store(), sender_id, resp)
            .await
            .into_iter()
            .collect()
    }

    /// Handle Bitfield - called directly by handler
    pub async fn handle_bitfield(&self, bitfield: BitfieldMessage, sender_id: [u8; 32]) {
        handle_bitfield(sender_id, bitfield);
    }
}

/// Handle ChunkMapRequest - respond with who has what
async fn handle_chunk_map_request(
    db: &Mutex<DbConnection>,
    _blob_store: &BlobStore,
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
    let traces = match get_section_traces(&db_lock, &req.hash) {
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

/// Handle ChunkRequest - send the requested chunk
///
/// Returns a single ChunkResponse (1:1 RPC)
async fn handle_chunk_request(
    db: &Mutex<DbConnection>,
    blob_store: &BlobStore,
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

    match blob_store.read_chunk(&req.hash, req.chunk_index) {
        Ok(data) => {
            trace!(
                hash = %hex::encode(&req.hash[..8]),
                chunk = req.chunk_index,
                to = %hex::encode(&sender_id[..8]),
                "sending chunk"
            );

            vec![ShareMessage::ChunkResponse(ChunkResponse {
                hash: req.hash,
                chunk_index: req.chunk_index,
                data,
            })]
        }
        Err(e) => {
            debug!(error = %e, chunk = req.chunk_index, "failed to read chunk");
            Vec::new()
        }
    }
}

/// Handle incoming Bitfield - log for tracking (dead code, kept for protocol compatibility)
fn handle_bitfield(sender_id: [u8; 32], bitfield: BitfieldMessage) {
    let chunk_count = bitfield.bitfield.len() * 8;
    trace!(
        hash = %hex::encode(&bitfield.hash[..8]),
        from = %hex::encode(&sender_id[..8]),
        chunks = chunk_count,
        "received bitfield"
    );
}

/// Handle incoming ChunkResponse - store the chunk
async fn handle_chunk_response(
    db: &Mutex<DbConnection>,
    blob_store: &BlobStore,
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
    if blob_store
        .is_complete(&resp.hash, blob.total_chunks)
        .unwrap_or(false)
    {
        // Mark as complete in database
        {
            let db_lock = db.lock().await;
            if let Err(e) = mark_blob_complete(&db_lock, &resp.hash) {
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
