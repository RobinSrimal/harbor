//! Share pull task - Background retry for incomplete blobs
//!
//! Periodically checks for incomplete blobs and attempts to fetch missing chunks
//! from peers. Implements retry logic with exponential backoff.
//!
//! Section downloads happen in parallel (up to 5 concurrent) for faster transfers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::join_all;
use iroh::endpoint::Connection;
use rusqlite::Connection as DbConnection;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::data::{
    get_blob, get_blobs_for_scope, get_section_peer_suggestion,
    get_all_topics, BlobState, BlobStore, CHUNK_SIZE,
};
use crate::network::share::protocol::{
    ChunkRequest, ShareMessage,
};
use crate::network::share::ShareService;
use crate::protocol::{ProtocolEvent, FileProgressEvent, FileCompleteEvent};

/// Tracks retry state for a blob
#[derive(Debug)]
struct BlobRetryState {
    /// Number of failed attempts
    attempts: u32,
    /// Last attempt time
    last_attempt: Instant,
    /// Backoff duration for next retry
    backoff: Duration,
}

impl Default for BlobRetryState {
    fn default() -> Self {
        Self {
            attempts: 0,
            last_attempt: Instant::now() - Duration::from_secs(3600), // Allow immediate first try
            backoff: Duration::from_secs(5),
        }
    }
}

impl BlobRetryState {
    /// Check if ready for retry
    fn ready_for_retry(&self) -> bool {
        self.last_attempt.elapsed() >= self.backoff
    }

    /// Record a failed attempt, increase backoff
    fn record_failure(&mut self) {
        self.attempts += 1;
        self.last_attempt = Instant::now();
        // Exponential backoff: 5s, 10s, 20s, 40s, 80s, max 5 minutes
        self.backoff = Duration::from_secs(
            std::cmp::min(300, 5 * (1 << self.attempts.min(6)))
        );
    }

    /// Record a successful chunk reception, reset backoff
    fn record_success(&mut self) {
        self.attempts = 0;
        self.backoff = Duration::from_secs(5);
        self.last_attempt = Instant::now();
    }
}

/// Run the share pull background task
pub async fn run_share_pull_task(
    db: Arc<Mutex<DbConnection>>,
    share_service: Arc<ShareService>,
    our_id: [u8; 32],
    running: Arc<RwLock<bool>>,
    check_interval: Duration,
    blob_path: std::path::PathBuf,
    event_tx: tokio::sync::mpsc::Sender<crate::protocol::ProtocolEvent>,
) {
    info!(path = %blob_path.display(), "Share pull task started");

    // Track retry state per blob
    let mut retry_states: HashMap<[u8; 32], BlobRetryState> = HashMap::new();

    loop {
        // Check if we should stop
        if !*running.read().await {
            info!("Share pull task stopping");
            break;
        }

        // Sleep before next check
        tokio::time::sleep(check_interval).await;

        // Find incomplete blobs
        let incomplete_blobs = {
            let db_lock = db.lock().await;
            find_incomplete_blobs(&db_lock)
        };

        if incomplete_blobs.is_empty() {
            continue;
        }

        debug!(count = incomplete_blobs.len(), "Found incomplete blobs");

        // Process each incomplete blob
        for (hash, topic_id, source_id, total_chunks) in incomplete_blobs {
            // Get or create retry state
            let state = retry_states.entry(hash).or_default();

            // Check if ready for retry
            if !state.ready_for_retry() {
                continue;
            }

            debug!(
                hash = hex::encode(&hash[..8]),
                attempts = state.attempts,
                "Attempting to pull incomplete blob"
            );

            // Get blob store
            let blob_store = match BlobStore::new(&blob_path) {
                Ok(store) => store,
                Err(e) => {
                    warn!(error = %e, "Failed to init blob store");
                    continue;
                }
            };

            // Find which chunks we're missing
            let local_chunks = match blob_store.get_chunk_bitfield(&hash, total_chunks) {
                Ok(bf) => bf,
                Err(e) => {
                    warn!(error = %e, "Failed to get chunk bitfield");
                    state.record_failure();
                    continue;
                }
            };

            let missing_chunks: Vec<u32> = local_chunks
                .iter()
                .enumerate()
                .filter(|&(_, has)| !has)
                .map(|(i, _)| i as u32)
                .collect();

            if missing_chunks.is_empty() {
                // All chunks present, mark complete
                let blob_name;
                let blob_size;
                {
                    let db_lock = db.lock().await;
                    if let Err(e) = crate::data::mark_blob_complete(&db_lock, &hash) {
                        warn!(error = %e, "Failed to mark blob complete");
                    }
                    // Get blob info for the event
                    let blob_info = get_blob(&db_lock, &hash).ok().flatten();
                    blob_name = blob_info.as_ref().map(|b| b.display_name.clone()).unwrap_or_default();
                    blob_size = blob_info.as_ref().map(|b| b.total_size).unwrap_or(0);
                }
                retry_states.remove(&hash);
                info!(
                    hash = hex::encode(&hash[..8]),
                    "Blob complete (via pull task)"
                );
                
                // Emit FileComplete event
                let complete_event = ProtocolEvent::FileComplete(FileCompleteEvent {
                    hash,
                    display_name: blob_name,
                    total_size: blob_size,
                });
                let _ = event_tx.send(complete_event).await;
                
                continue;
            }

            debug!(
                hash = hex::encode(&hash[..8]),
                missing = missing_chunks.len(),
                total = total_chunks,
                "Missing chunks"
            );

            // Try to pull from peers (only from topic members to prevent spam)
            let success = pull_missing_chunks(
                &db,
                &share_service,
                &blob_store,
                &hash,
                &topic_id,
                &source_id,
                &missing_chunks,
                our_id,
            ).await;

            if success {
                state.record_success();
                
                // Emit FileProgress event
                // Recalculate chunks_complete after pull
                let chunks_complete = match blob_store.get_chunk_bitfield(&hash, total_chunks) {
                    Ok(bitfield) => bitfield.iter().filter(|&&has| has).count() as u32,
                    Err(_) => total_chunks - missing_chunks.len() as u32,
                };
                
                let progress_event = ProtocolEvent::FileProgress(FileProgressEvent {
                    hash,
                    chunks_complete,
                    total_chunks,
                });
                let _ = event_tx.send(progress_event).await;
            } else {
                state.record_failure();
                debug!(
                    hash = hex::encode(&hash[..8]),
                    next_retry_secs = state.backoff.as_secs(),
                    "Pull failed, will retry"
                );
            }
        }
    }
}

/// Find all incomplete blobs across all topics
fn find_incomplete_blobs(db: &DbConnection) -> Vec<([u8; 32], [u8; 32], [u8; 32], u32)> {
    let mut results = Vec::new();

    // Get all topics we're subscribed to
    let topics = match get_all_topics(db) {
        Ok(t) => t,
        Err(e) => {
            warn!(error = %e, "Failed to get topics");
            return results;
        }
    };

    // Check each topic for incomplete blobs
    for topic in topics {
        let topic_id = topic.topic_id;
        let blobs = match get_blobs_for_scope(db, &topic_id) {
            Ok(b) => b,
            Err(_) => continue,
        };

        for blob in blobs {
            if blob.state == BlobState::Partial {
                results.push((blob.hash, topic_id, blob.source_id, blob.total_chunks));
            }
        }
    }

    results
}

/// Attempt to pull missing chunks from available peers
/// Only pulls from topic members to prevent spam attacks
async fn pull_missing_chunks(
    db: &Arc<Mutex<DbConnection>>,
    share_service: &Arc<ShareService>,
    blob_store: &BlobStore,
    hash: &[u8; 32],
    topic_id: &[u8; 32],
    source_id: &[u8; 32],
    missing_chunks: &[u32],
    _our_id: [u8; 32],
) -> bool {
    // Strategy:
    // 1. Get topic members (only pull from members to prevent spam)
    // 2. Check section traces for peers that have the sections we need
    // 3. Filter to only topic members
    // 4. Try connecting to those peers first
    // 5. Fall back to source if no peers available (if source is a member)

    // Get topic members - we only pull from members to prevent spam
    let topic_members: std::collections::HashSet<[u8; 32]> = {
        let db_lock = db.lock().await;
        match crate::data::get_topic_members(&db_lock, topic_id) {
            Ok(members) => members.into_iter().collect(),
            Err(e) => {
                warn!(error = %e, "Failed to get topic members for share pull");
                return false;
            }
        }
    };

    // Validate source is a topic member
    if !topic_members.contains(source_id) {
        debug!(
            source = hex::encode(&source_id[..8]),
            "Source is not a topic member, skipping pull"
        );
        return false;
    }

    // Group missing chunks by section
    let blob_metadata = {
        let db_lock = db.lock().await;
        match get_blob(&db_lock, hash) {
            Ok(Some(m)) => m,
            _ => return false,
        }
    };

    let num_sections = blob_metadata.num_sections as u32;
    let chunks_per_section = blob_metadata.total_chunks.div_ceil(num_sections);

    // For each missing chunk, find which section it belongs to
    let mut sections_needed: HashMap<u8, Vec<u32>> = HashMap::new();
    for &chunk in missing_chunks {
        let section = (chunk / chunks_per_section) as u8;
        sections_needed.entry(section).or_default().push(chunk);
    }

    // Get peer suggestions for all sections upfront (to minimize lock contention)
    let section_targets: Vec<(u8, Vec<u32>, [u8; 32])> = {
        let db_lock = db.lock().await;
        sections_needed
            .into_iter()
            .filter_map(|(section_id, chunks)| {
                // Look for a peer that has this section (must be a topic member)
                let peer_id = get_section_peer_suggestion(&db_lock, hash, section_id)
                    .ok()
                    .flatten()
                    .filter(|peer| topic_members.contains(peer));

                // Determine who to connect to (must be a topic member)
                let target_id = peer_id.unwrap_or(*source_id);

                // Double-check target is a topic member
                if !topic_members.contains(&target_id) {
                    debug!(
                        target = hex::encode(&target_id[..8]),
                        section = section_id,
                        "Target is not a topic member, skipping"
                    );
                    return None;
                }

                Some((section_id, chunks, target_id))
            })
            .collect()
    };

    let num_sections_to_pull = section_targets.len();
    if num_sections_to_pull == 0 {
        return false;
    }

    info!(
        hash = hex::encode(&hash[..8]),
        sections = num_sections_to_pull,
        "Pulling {} sections in parallel",
        num_sections_to_pull
    );

    // Pull all sections in parallel (max 5 sections)
    let section_futures: Vec<_> = section_targets
        .into_iter()
        .map(|(section_id, chunks, target_id)| {
            let share_service = share_service.clone();
            let blob_store = blob_store.clone();
            let hash = *hash;
            let source_id = *source_id;
            let total_size = blob_metadata.total_size;
            let topic_members = topic_members.clone();

            async move {
                pull_section(
                    &share_service,
                    &blob_store,
                    &hash,
                    section_id,
                    &chunks,
                    target_id,
                    &source_id,
                    total_size,
                    &topic_members,
                )
                .await
            }
        })
        .collect();

    // Execute all section pulls in parallel
    let results = join_all(section_futures).await;

    // Sum up chunks received from all sections
    let total_received: usize = results.iter().sum();

    if total_received > 0 {
        info!(
            hash = hex::encode(&hash[..8]),
            chunks = total_received,
            sections = num_sections_to_pull,
            "Parallel pull complete"
        );
    }

    total_received > 0
}

/// Pull a single section from a target peer
/// Returns the number of chunks received
async fn pull_section(
    share_service: &ShareService,
    blob_store: &BlobStore,
    hash: &[u8; 32],
    section_id: u8,
    chunks: &[u32],
    target_id: [u8; 32],
    source_id: &[u8; 32],
    total_size: u64,
    topic_members: &HashSet<[u8; 32]>,
) -> usize {
    let mut chunks_received = 0;

    debug!(
        section = section_id,
        target = hex::encode(&target_id[..8]),
        chunks = chunks.len(),
        "Attempting to pull section"
    );

    // Try to connect to target
    let target_node_id = match iroh::EndpointId::from_bytes(&target_id) {
        Ok(id) => id,
        Err(e) => {
            debug!(error = %e, "Invalid target node ID");
            return 0;
        }
    };
    match share_service.connect_for_share(target_node_id).await {
        Ok(conn) => {
            // Request chunks
            let result = request_chunks_from_peer(&conn, blob_store, hash, chunks, total_size).await;
            chunks_received += result.received;

            if result.received > 0 {
                info!(
                    hash = hex::encode(&hash[..8]),
                    section = section_id,
                    received = result.received,
                    "Received chunks from peer"
                );
            }

            // If we didn't get all chunks and peer suggested alternatives, try them
            // Only try suggested peers that are topic members (spam prevention)
            if result.received < chunks.len() && !result.suggested_peers.is_empty() {
                for suggested_peer in result.suggested_peers {
                    // Skip if this is the same as our current target
                    if suggested_peer == target_id {
                        continue;
                    }

                    // Only accept suggested peers that are topic members
                    if !topic_members.contains(&suggested_peer) {
                        debug!(
                            suggested = hex::encode(&suggested_peer[..8]),
                            "Ignoring suggested peer - not a topic member"
                        );
                        continue;
                    }

                    debug!(
                        suggested = hex::encode(&suggested_peer[..8]),
                        "Trying suggested peer (verified topic member)"
                    );

                    let suggested_node_id = match iroh::EndpointId::from_bytes(&suggested_peer) {
                        Ok(id) => id,
                        Err(_) => continue,
                    };
                    if let Ok(suggested_conn) = share_service.connect_for_share(suggested_node_id).await {
                        let suggested_result =
                            request_chunks_from_peer(&suggested_conn, blob_store, hash, chunks, total_size)
                                .await;
                        chunks_received += suggested_result.received;

                        if suggested_result.received > 0 {
                            info!(
                                hash = hex::encode(&hash[..8]),
                                section = section_id,
                                received = suggested_result.received,
                                peer = hex::encode(&suggested_peer[..8]),
                                "Received chunks from suggested peer"
                            );
                            break; // Got chunks from suggested peer, done with this section
                        }
                    }
                }
            }
        }
        Err(e) => {
            debug!(
                target = hex::encode(&target_id[..8]),
                error = %e,
                "Failed to connect to peer for share"
            );

            // If this wasn't the source, try the source as fallback
            // (source was already validated as topic member in caller)
            if target_id != *source_id {
                let source_node_id = match iroh::EndpointId::from_bytes(source_id) {
                    Ok(id) => id,
                    Err(_) => return chunks_received,
                };
                if let Ok(conn) = share_service.connect_for_share(source_node_id).await {
                    let result =
                        request_chunks_from_peer(&conn, blob_store, hash, chunks, total_size).await;
                    chunks_received += result.received;
                }
            }
        }
    }

    chunks_received
}

/// Result of requesting chunks from a peer
struct ChunkRequestResult {
    /// Number of chunks received
    received: usize,
    /// Suggested alternative peers (if current peer is busy)
    suggested_peers: Vec<[u8; 32]>,
}

/// Request specific chunks from a connected peer
async fn request_chunks_from_peer(
    conn: &Connection,
    blob_store: &BlobStore,
    hash: &[u8; 32],
    chunks: &[u32],
    total_size: u64,
) -> ChunkRequestResult {
    let mut received = 0;
    let mut suggested_peers = Vec::new();

    // Request each chunk individually (1:1 RPC)
    for &chunk_index in chunks {
        let request = ShareMessage::ChunkRequest(ChunkRequest {
            hash: *hash,
            chunk_index,
        });

        // Open bidirectional stream
        let (mut send, mut recv) = match conn.open_bi().await {
            Ok(s) => s,
            Err(e) => {
                debug!(error = %e, "Failed to open stream");
                break;
            }
        };

        // Send request
        let encoded = request.encode();
        if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut send, &encoded).await {
            debug!(error = %e, "Failed to send chunk request");
            break;
        }
        let _ = send.finish();

        // Read single response
        let max_response_size = CHUNK_SIZE as usize + 1024;
        let response_data = match recv.read_to_end(max_response_size).await {
            Ok(data) => data,
            Err(e) => {
                debug!(error = %e, "Failed to read chunk response");
                continue;
            }
        };

        // Parse response
        match ShareMessage::decode(&response_data) {
            Ok(ShareMessage::ChunkResponse(resp)) => {
                if &resp.hash == hash {
                    if let Err(e) = blob_store.write_chunk(
                        hash,
                        resp.chunk_index,
                        &resp.data,
                        total_size,
                    ) {
                        warn!(error = %e, chunk = resp.chunk_index, "Failed to write chunk");
                    } else {
                        received += 1;
                    }
                }
            }
            Ok(ShareMessage::PeerSuggestion(suggestion)) => {
                info!(
                    suggested = hex::encode(&suggestion.suggested_peer[..8]),
                    section = suggestion.section_id,
                    "Peer suggested alternative - will try if current attempt fails"
                );
                suggested_peers.push(suggestion.suggested_peer);
            }
            _ => {}
        }
    }

    ChunkRequestResult { received, suggested_peers }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    // ========================================================================
    // BlobRetryState Tests
    // ========================================================================

    #[test]
    fn test_retry_state_default_allows_immediate_retry() {
        let state = BlobRetryState::default();
        
        // Default state should allow immediate retry (last_attempt is 1 hour ago)
        assert!(state.ready_for_retry());
        assert_eq!(state.attempts, 0);
        assert_eq!(state.backoff, Duration::from_secs(5));
    }

    #[test]
    fn test_retry_state_not_ready_immediately_after_failure() {
        let mut state = BlobRetryState::default();
        
        state.record_failure();
        
        // Immediately after failure, should NOT be ready
        assert!(!state.ready_for_retry());
        assert_eq!(state.attempts, 1);
    }

    #[test]
    fn test_retry_state_exponential_backoff() {
        let mut state = BlobRetryState::default();
        
        // First failure: backoff should be 10s (5 * 2^1)
        state.record_failure();
        assert_eq!(state.backoff, Duration::from_secs(10));
        assert_eq!(state.attempts, 1);
        
        // Second failure: backoff should be 20s (5 * 2^2)
        state.record_failure();
        assert_eq!(state.backoff, Duration::from_secs(20));
        assert_eq!(state.attempts, 2);
        
        // Third failure: backoff should be 40s (5 * 2^3)
        state.record_failure();
        assert_eq!(state.backoff, Duration::from_secs(40));
        assert_eq!(state.attempts, 3);
        
        // Fourth failure: backoff should be 80s (5 * 2^4)
        state.record_failure();
        assert_eq!(state.backoff, Duration::from_secs(80));
        assert_eq!(state.attempts, 4);
        
        // Fifth failure: backoff should be 160s (5 * 2^5)
        state.record_failure();
        assert_eq!(state.backoff, Duration::from_secs(160));
        assert_eq!(state.attempts, 5);
        
        // Sixth failure: backoff should be 300s (capped at 5 minutes)
        state.record_failure();
        assert_eq!(state.backoff, Duration::from_secs(300));
        assert_eq!(state.attempts, 6);
        
        // Seventh failure: still capped at 300s
        state.record_failure();
        assert_eq!(state.backoff, Duration::from_secs(300));
        assert_eq!(state.attempts, 7);
    }

    #[test]
    fn test_retry_state_success_resets() {
        let mut state = BlobRetryState::default();
        
        // Simulate several failures
        state.record_failure();
        state.record_failure();
        state.record_failure();
        assert_eq!(state.attempts, 3);
        assert_eq!(state.backoff, Duration::from_secs(40));
        
        // Success should reset
        state.record_success();
        assert_eq!(state.attempts, 0);
        assert_eq!(state.backoff, Duration::from_secs(5));
    }

    #[test]
    fn test_retry_state_ready_after_backoff_elapsed() {
        let mut state = BlobRetryState::default();
        
        // Set a very short backoff for testing
        state.backoff = Duration::from_millis(50);
        state.last_attempt = Instant::now();
        
        // Not ready immediately
        assert!(!state.ready_for_retry());
        
        // Wait for backoff
        thread::sleep(Duration::from_millis(60));
        
        // Now should be ready
        assert!(state.ready_for_retry());
    }

    #[test]
    fn test_retry_state_backoff_max_cap() {
        let mut state = BlobRetryState::default();
        
        // Simulate many failures
        for _ in 0..20 {
            state.record_failure();
        }
        
        // Backoff should be capped at 300 seconds (5 minutes)
        assert_eq!(state.backoff, Duration::from_secs(300));
        assert_eq!(state.attempts, 20);
    }

    #[test]
    fn test_retry_state_attempts_overflow_protection() {
        let mut state = BlobRetryState::default();
        
        // The min(6) in the backoff calculation prevents overflow
        // Even with max attempts, backoff calculation should work
        state.attempts = u32::MAX - 1;
        state.record_failure();
        
        // Should not panic, backoff should be capped
        assert_eq!(state.backoff, Duration::from_secs(300));
    }

    // ========================================================================
    // Integration-style tests (with test database)
    // ========================================================================

    #[test]
    fn test_find_incomplete_blobs_empty_db() {
        // Create in-memory test database
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        
        // Initialize schema (topics table needed)
        conn.execute(
            "CREATE TABLE topics (
                topic_id BLOB PRIMARY KEY,
                harbor_id BLOB,
                created_at INTEGER
            )",
            [],
        ).unwrap();
        
        // Should return empty vec for empty DB
        let result = find_incomplete_blobs(&conn);
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_incomplete_blobs_with_partial_blob() {
        // Create in-memory test database
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        
        // Initialize schema
        conn.execute(
            "CREATE TABLE topics (
                topic_id BLOB PRIMARY KEY,
                harbor_id BLOB,
                created_at INTEGER DEFAULT (strftime('%s', 'now'))
            )",
            [],
        ).unwrap();

        conn.execute(
            "CREATE TABLE peers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint_id BLOB UNIQUE NOT NULL,
                last_seen INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        
        conn.execute(
            "CREATE TABLE blobs (
                hash BLOB PRIMARY KEY,
                scope_id BLOB NOT NULL,
                source_peer_id INTEGER NOT NULL,
                display_name TEXT NOT NULL,
                total_size INTEGER NOT NULL,
                total_chunks INTEGER NOT NULL,
                num_sections INTEGER NOT NULL DEFAULT 1,
                state INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER DEFAULT (strftime('%s', 'now')),
                FOREIGN KEY (source_peer_id) REFERENCES peers(id)
            )",
            [],
        ).unwrap();
        
        let topic_id = [1u8; 32];
        let hash = [2u8; 32];
        let source_id = [3u8; 32];
        
        // Insert topic
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id) VALUES (?1, ?2)",
            rusqlite::params![topic_id.as_slice(), [0u8; 32].as_slice()],
        ).unwrap();

        // Insert source peer
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, 0)",
            rusqlite::params![source_id.as_slice()],
        )
        .unwrap();
        
        // Insert partial blob (state = 0 = Partial)
        conn.execute(
            "INSERT INTO blobs (hash, scope_id, source_peer_id, display_name, total_size, total_chunks, state)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3), 'test.bin', 1048576, 2, 0)",
            rusqlite::params![hash.as_slice(), topic_id.as_slice(), source_id.as_slice()],
        ).unwrap();
        
        let result = find_incomplete_blobs(&conn);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, hash);
        assert_eq!(result[0].1, topic_id);
        assert_eq!(result[0].2, source_id);
        assert_eq!(result[0].3, 2); // total_chunks
    }

    #[test]
    fn test_find_incomplete_blobs_ignores_complete() {
        // Create in-memory test database
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        
        // Initialize schema
        conn.execute(
            "CREATE TABLE topics (
                topic_id BLOB PRIMARY KEY,
                harbor_id BLOB,
                created_at INTEGER DEFAULT (strftime('%s', 'now'))
            )",
            [],
        ).unwrap();

        conn.execute(
            "CREATE TABLE peers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint_id BLOB UNIQUE NOT NULL,
                last_seen INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        
        conn.execute(
            "CREATE TABLE blobs (
                hash BLOB PRIMARY KEY,
                scope_id BLOB NOT NULL,
                source_peer_id INTEGER NOT NULL,
                display_name TEXT NOT NULL,
                total_size INTEGER NOT NULL,
                total_chunks INTEGER NOT NULL,
                num_sections INTEGER NOT NULL DEFAULT 1,
                state INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER DEFAULT (strftime('%s', 'now')),
                FOREIGN KEY (source_peer_id) REFERENCES peers(id)
            )",
            [],
        ).unwrap();
        
        let topic_id = [1u8; 32];
        let hash = [2u8; 32];
        let source_id = [3u8; 32];
        
        // Insert topic
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id) VALUES (?1, ?2)",
            rusqlite::params![topic_id.as_slice(), [0u8; 32].as_slice()],
        ).unwrap();

        // Insert source peer
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, 0)",
            rusqlite::params![source_id.as_slice()],
        )
        .unwrap();
        
        // Insert complete blob (state = 1 = Complete)
        conn.execute(
            "INSERT INTO blobs (hash, scope_id, source_peer_id, display_name, total_size, total_chunks, state)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3), 'test.bin', 1048576, 2, 1)",
            rusqlite::params![hash.as_slice(), topic_id.as_slice(), source_id.as_slice()],
        ).unwrap();
        
        let result = find_incomplete_blobs(&conn);
        assert!(result.is_empty(), "Complete blobs should be ignored");
    }

    #[test]
    fn test_find_incomplete_blobs_multiple_topics() {
        // Create in-memory test database
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        
        // Initialize schema
        conn.execute(
            "CREATE TABLE topics (
                topic_id BLOB PRIMARY KEY,
                harbor_id BLOB,
                created_at INTEGER DEFAULT (strftime('%s', 'now'))
            )",
            [],
        ).unwrap();

        conn.execute(
            "CREATE TABLE peers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint_id BLOB UNIQUE NOT NULL,
                last_seen INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        
        conn.execute(
            "CREATE TABLE blobs (
                hash BLOB PRIMARY KEY,
                scope_id BLOB NOT NULL,
                source_peer_id INTEGER NOT NULL,
                display_name TEXT NOT NULL,
                total_size INTEGER NOT NULL,
                total_chunks INTEGER NOT NULL,
                num_sections INTEGER NOT NULL DEFAULT 1,
                state INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER DEFAULT (strftime('%s', 'now')),
                FOREIGN KEY (source_peer_id) REFERENCES peers(id)
            )",
            [],
        ).unwrap();
        
        let topic1 = [1u8; 32];
        let topic2 = [2u8; 32];
        let hash1 = [10u8; 32];
        let hash2 = [20u8; 32];
        let hash3 = [30u8; 32];
        let source = [99u8; 32];
        
        // Insert topics
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id) VALUES (?1, ?2)",
            rusqlite::params![topic1.as_slice(), [0u8; 32].as_slice()],
        ).unwrap();
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id) VALUES (?1, ?2)",
            rusqlite::params![topic2.as_slice(), [0u8; 32].as_slice()],
        ).unwrap();

        // Insert source peer
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, 0)",
            rusqlite::params![source.as_slice()],
        )
        .unwrap();
        
        // Insert partial blobs in both topics
        conn.execute(
            "INSERT INTO blobs (hash, scope_id, source_peer_id, display_name, total_size, total_chunks, state)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3), 'file1.bin', 1048576, 2, 0)",
            rusqlite::params![hash1.as_slice(), topic1.as_slice(), source.as_slice()],
        ).unwrap();
        conn.execute(
            "INSERT INTO blobs (hash, scope_id, source_peer_id, display_name, total_size, total_chunks, state)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3), 'file2.bin', 2097152, 4, 0)",
            rusqlite::params![hash2.as_slice(), topic1.as_slice(), source.as_slice()],
        ).unwrap();
        conn.execute(
            "INSERT INTO blobs (hash, scope_id, source_peer_id, display_name, total_size, total_chunks, state)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3), 'file3.bin', 3145728, 6, 0)",
            rusqlite::params![hash3.as_slice(), topic2.as_slice(), source.as_slice()],
        ).unwrap();
        
        let result = find_incomplete_blobs(&conn);
        assert_eq!(result.len(), 3);
        
        // Verify all hashes are found
        let hashes: Vec<[u8; 32]> = result.iter().map(|r| r.0).collect();
        assert!(hashes.contains(&hash1));
        assert!(hashes.contains(&hash2));
        assert!(hashes.contains(&hash3));
    }

    // ========================================================================
    // ChunkRequestResult Tests
    // ========================================================================

    #[test]
    fn test_chunk_request_result_empty() {
        let result = ChunkRequestResult {
            received: 0,
            suggested_peers: vec![],
        };
        
        assert_eq!(result.received, 0);
        assert!(result.suggested_peers.is_empty());
    }

    #[test]
    fn test_chunk_request_result_with_chunks() {
        let result = ChunkRequestResult {
            received: 5,
            suggested_peers: vec![],
        };
        
        assert_eq!(result.received, 5);
        assert!(result.suggested_peers.is_empty());
    }

    #[test]
    fn test_chunk_request_result_with_suggestions() {
        let peer1 = [1u8; 32];
        let peer2 = [2u8; 32];
        
        let result = ChunkRequestResult {
            received: 2,
            suggested_peers: vec![peer1, peer2],
        };
        
        assert_eq!(result.received, 2);
        assert_eq!(result.suggested_peers.len(), 2);
        assert_eq!(result.suggested_peers[0], peer1);
        assert_eq!(result.suggested_peers[1], peer2);
    }

    #[test]
    fn test_chunk_request_result_partial_success_with_fallback() {
        // Simulates scenario where we got some chunks but peer suggested others
        let suggested = [3u8; 32];
        
        let result = ChunkRequestResult {
            received: 3, // Got 3 of 10 requested
            suggested_peers: vec![suggested],
        };
        
        // Caller should try suggested peer for remaining 7 chunks
        assert!(result.received < 10);
        assert!(!result.suggested_peers.is_empty());
    }

    #[test]
    fn test_chunk_request_result_multiple_suggestions() {
        // Peer might suggest multiple alternatives
        let peers: Vec<[u8; 32]> = (1..=5).map(|i| [i as u8; 32]).collect();
        
        let result = ChunkRequestResult {
            received: 0,
            suggested_peers: peers.clone(),
        };
        
        assert_eq!(result.suggested_peers.len(), 5);
        for (i, peer) in result.suggested_peers.iter().enumerate() {
            assert_eq!(*peer, [(i + 1) as u8; 32]);
        }
    }
}
