//! Incoming message handlers for ShareService
//!
//! Processes requests from peers for:
//! - Chunk maps (who has what)
//! - Individual chunks
//! - Chunk responses (when we're pulling)
//! - Bitfield exchanges (compatibility-only)

use std::io::ErrorKind;

use rusqlite::Connection as DbConnection;
use tokio::sync::Mutex;
use tracing::{debug, info, trace, warn};

use crate::data::{BlobState, BlobStore, get_blob, get_section_traces, mark_blob_complete};

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
                // Response message, shouldn't receive this as a request.
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

/// Handle ChunkMapRequest - respond with who has what.
async fn handle_chunk_map_request(
    db: &Mutex<DbConnection>,
    _blob_store: &BlobStore,
    our_id: [u8; 32],
    req: ChunkMapRequest,
) -> Option<ShareMessage> {
    let db_lock = db.lock().await;

    let blob = match get_blob(&db_lock, &req.hash) {
        Ok(Some(b)) => b,
        Ok(None) => return None,
        Err(e) => {
            warn!(
                error = %e,
                hash = %hex::encode(&req.hash[..8]),
                "failed to load blob metadata for chunk map request"
            );
            return None;
        }
    };

    let traces = match get_section_traces(&db_lock, &req.hash) {
        Ok(t) => t,
        Err(e) => {
            warn!(
                error = %e,
                hash = %hex::encode(&req.hash[..8]),
                "failed to load section traces for chunk map request"
            );
            return None;
        }
    };

    let mut peers = Vec::new();
    for trace in traces {
        if let Some(peer_id) = trace.received_from {
            peers.push(PeerChunks {
                endpoint_id: peer_id,
                chunk_start: trace.chunk_start,
                chunk_end: trace.chunk_end,
            });
        }
    }

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

/// Handle ChunkRequest - send the requested chunk.
///
/// Returns a single ChunkResponse (1:1 RPC).
async fn handle_chunk_request(
    db: &Mutex<DbConnection>,
    blob_store: &BlobStore,
    sender_id: [u8; 32],
    req: ChunkRequest,
) -> Vec<ShareMessage> {
    let total_chunks = {
        let db_lock = db.lock().await;
        match get_blob(&db_lock, &req.hash) {
            Ok(Some(blob)) => blob.total_chunks,
            Ok(None) => return Vec::new(),
            Err(e) => {
                warn!(
                    error = %e,
                    hash = %hex::encode(&req.hash[..8]),
                    "failed to load blob metadata for chunk request"
                );
                return Vec::new();
            }
        }
    };

    let bitfield = match blob_store.get_chunk_bitfield(&req.hash, total_chunks) {
        Ok(bitfield) => bitfield,
        Err(e) => {
            warn!(
                error = %e,
                hash = %hex::encode(&req.hash[..8]),
                "failed to load chunk bitfield for chunk request"
            );
            return Vec::new();
        }
    };

    let chunk_idx = req.chunk_index as usize;
    if chunk_idx >= bitfield.len() || !bitfield[chunk_idx] {
        debug!(
            hash = %hex::encode(&req.hash[..8]),
            chunk = req.chunk_index,
            "requested chunk is not available locally"
        );
        return Vec::new();
    }

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

/// Handle incoming Bitfield - log for tracking.
fn handle_bitfield(sender_id: [u8; 32], bitfield: BitfieldMessage) {
    let chunk_count = bitfield.bitfield.len() * 8;
    trace!(
        hash = %hex::encode(&bitfield.hash[..8]),
        from = %hex::encode(&sender_id[..8]),
        chunks = chunk_count,
        "received bitfield"
    );
}

fn sender_is_known_for_blob(blob: &crate::data::BlobMetadata, sender_id: &[u8; 32]) -> bool {
    blob.source_id == *sender_id
}

/// Handle incoming ChunkResponse - store the chunk.
async fn handle_chunk_response(
    db: &Mutex<DbConnection>,
    blob_store: &BlobStore,
    sender_id: [u8; 32],
    resp: ChunkResponse,
) -> Option<ShareMessage> {
    let blob = {
        let db_lock = db.lock().await;
        let blob = match get_blob(&db_lock, &resp.hash) {
            Ok(Some(b)) => b,
            Ok(None) => return None,
            Err(e) => {
                warn!(
                    error = %e,
                    hash = %hex::encode(&resp.hash[..8]),
                    "failed to load blob metadata for chunk response"
                );
                return None;
            }
        };

        let known_sender = if sender_is_known_for_blob(&blob, &sender_id) {
            true
        } else {
            match get_section_traces(&db_lock, &resp.hash) {
                Ok(traces) => traces
                    .iter()
                    .any(|trace| trace.received_from == Some(sender_id)),
                Err(e) => {
                    warn!(
                        error = %e,
                        hash = %hex::encode(&resp.hash[..8]),
                        "failed to load section traces while validating chunk sender"
                    );
                    false
                }
            }
        };

        if !known_sender {
            warn!(
                hash = %hex::encode(&resp.hash[..8]),
                sender = %hex::encode(&sender_id[..8]),
                "dropping unsolicited chunk response from unknown sender"
            );
            return None;
        }

        blob
    };

    match blob_store.verify_chunk(&resp.hash, resp.chunk_index, &resp.data) {
        Ok(true) => {}
        Ok(false) => match blob_store.read_outboard(&resp.hash) {
            Ok(_) => {
                warn!(
                    hash = %hex::encode(&resp.hash[..8]),
                    chunk = resp.chunk_index,
                    "chunk verification failed with outboard present; dropping response"
                );
                return None;
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                debug!(
                    hash = %hex::encode(&resp.hash[..8]),
                    chunk = resp.chunk_index,
                    "accepting chunk response without outboard verification"
                );
            }
            Err(e) => {
                warn!(
                    error = %e,
                    hash = %hex::encode(&resp.hash[..8]),
                    "failed to inspect outboard for chunk verification"
                );
                return None;
            }
        },
        Err(e) => {
            warn!(
                error = %e,
                hash = %hex::encode(&resp.hash[..8]),
                chunk = resp.chunk_index,
                "failed to verify chunk response"
            );
            return None;
        }
    }

    if let Err(e) =
        blob_store.write_chunk(&resp.hash, resp.chunk_index, &resp.data, blob.total_size)
    {
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

    match blob_store.is_complete(&resp.hash, blob.total_chunks) {
        Ok(true) => {
            let db_lock = db.lock().await;
            if let Err(e) = mark_blob_complete(&db_lock, &resp.hash) {
                warn!(error = %e, "failed to mark blob complete");
            }

            info!(
                hash = hex::encode(&resp.hash[..8]),
                name = blob.display_name,
                size = blob.total_size,
                chunks = blob.total_chunks,
                "SHARE: File complete"
            );
        }
        Ok(false) => {}
        Err(e) => {
            warn!(
                error = %e,
                hash = %hex::encode(&resp.hash[..8]),
                "failed to check blob completion after chunk write"
            );
        }
    }

    Some(ShareMessage::ChunkAck(ChunkAck {
        hash: resp.hash,
        chunk_index: resp.chunk_index,
    }))
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    use super::*;
    use crate::data::{BlobStore, CHUNK_SIZE, create_all_tables, get_blob, insert_blob};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB should open");
        conn.execute("PRAGMA foreign_keys = ON", [])
            .expect("foreign keys should be enabled");
        create_all_tables(&conn).expect("schema should initialize");
        conn
    }

    fn setup_blob_store() -> (BlobStore, TempDir) {
        let temp_dir = TempDir::new().expect("temp dir should create");
        let store = BlobStore::new(temp_dir.path().join("blobs")).expect("blob store should init");
        (store, temp_dir)
    }

    #[tokio::test]
    async fn test_handle_chunk_request_skips_missing_chunk() {
        let db = Mutex::new(setup_db());
        let (blob_store, _temp) = setup_blob_store();

        let hash = [1u8; 32];
        let scope_id = [2u8; 32];
        let source_id = [3u8; 32];
        {
            let db_lock = db.lock().await;
            insert_blob(
                &db_lock, &hash, &scope_id, &source_id, "test.bin", CHUNK_SIZE, 1,
            )
            .expect("blob insert should work");
        }

        let responses = handle_chunk_request(
            &db,
            &blob_store,
            [9u8; 32],
            ChunkRequest {
                hash,
                chunk_index: 0,
            },
        )
        .await;

        assert!(responses.is_empty(), "missing chunk should not be served");
    }

    #[tokio::test]
    async fn test_handle_chunk_response_rejects_unknown_sender() {
        let db = Mutex::new(setup_db());
        let (blob_store, _temp) = setup_blob_store();

        let hash = [4u8; 32];
        let scope_id = [5u8; 32];
        let source_id = [6u8; 32];
        {
            let db_lock = db.lock().await;
            insert_blob(
                &db_lock, &hash, &scope_id, &source_id, "test.bin", CHUNK_SIZE, 1,
            )
            .expect("blob insert should work");
        }

        let response = handle_chunk_response(
            &db,
            &blob_store,
            [9u8; 32],
            ChunkResponse {
                hash,
                chunk_index: 0,
                data: vec![7u8; CHUNK_SIZE as usize],
            },
        )
        .await;

        assert!(response.is_none(), "unknown sender should be rejected");
    }

    #[tokio::test]
    async fn test_handle_chunk_response_accepts_source_and_marks_complete() {
        let db = Mutex::new(setup_db());
        let (blob_store, _temp) = setup_blob_store();

        let hash = [7u8; 32];
        let scope_id = [8u8; 32];
        let source_id = [9u8; 32];
        {
            let db_lock = db.lock().await;
            insert_blob(
                &db_lock, &hash, &scope_id, &source_id, "test.bin", CHUNK_SIZE, 1,
            )
            .expect("blob insert should work");
        }

        let response = handle_chunk_response(
            &db,
            &blob_store,
            source_id,
            ChunkResponse {
                hash,
                chunk_index: 0,
                data: vec![1u8; CHUNK_SIZE as usize],
            },
        )
        .await;

        match response {
            Some(ShareMessage::ChunkAck(ack)) => {
                assert_eq!(ack.hash, hash);
                assert_eq!(ack.chunk_index, 0);
            }
            other => panic!("expected chunk ack, got {other:?}"),
        }

        let state = {
            let db_lock = db.lock().await;
            let blob = get_blob(&db_lock, &hash)
                .expect("get_blob should succeed")
                .expect("blob should exist");
            blob.state
        };
        assert_eq!(state, BlobState::Complete);
    }

    #[tokio::test]
    async fn test_handle_chunk_response_rejects_when_outboard_verification_fails() {
        let db = Mutex::new(setup_db());
        let (blob_store, _temp) = setup_blob_store();

        let hash = [10u8; 32];
        let scope_id = [11u8; 32];
        let source_id = [12u8; 32];
        {
            let db_lock = db.lock().await;
            insert_blob(
                &db_lock, &hash, &scope_id, &source_id, "test.bin", CHUNK_SIZE, 1,
            )
            .expect("blob insert should work");
        }

        blob_store
            .write_outboard(&hash, &[0u8; 32])
            .expect("outboard should write");

        let response = handle_chunk_response(
            &db,
            &blob_store,
            source_id,
            ChunkResponse {
                hash,
                chunk_index: 0,
                data: vec![2u8; CHUNK_SIZE as usize],
            },
        )
        .await;

        assert!(
            response.is_none(),
            "verification failure with outboard should reject response"
        );
    }
}
