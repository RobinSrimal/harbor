//! File acquisition methods for ShareService
//!
//! Handles the "pull" side of file sharing:
//! - Requesting blobs from peers
//! - Planning chunk pulls
//! - Executing multi-peer downloads
//! - Recording section traces

use iroh::EndpointId;
use tracing::{debug, info, warn};

use crate::data::{
    get_blob, get_section_peer_suggestion, get_section_traces, mark_blob_complete,
    record_section_received, BlobMetadata, BlobState, CHUNK_SIZE,
};
use crate::data::dht::peer::{current_timestamp, get_peer_relay_info, update_peer_relay_url};
use crate::network::connect;

use super::protocol::{
    ChunkMapRequest, ChunkMapResponse, ChunkRequest, ChunkResponse, PeerChunks, PeerSuggestion,
    ShareMessage, SHARE_ALPN,
};
use super::service::{ChunkProcessResult, PeerAssignment, PullPlan, ShareError, ShareService};

/// Helper to calculate number of sections (same as in service.rs)
fn calculate_num_sections(total_chunks: u32) -> u8 {
    std::cmp::min(5, std::cmp::max(1, total_chunks)) as u8
}

impl ShareService {
    /// Request to download a file by hash
    ///
    /// Looks up metadata, checks completion, then runs the pull flow.
    pub async fn request_blob(&self, hash: &[u8; 32]) -> Result<(), ShareError> {
        let metadata = {
            let db_lock = self.db.lock().await;
            get_blob(&db_lock, hash).map_err(|e| ShareError::Database(e.to_string()))?
        };

        let metadata = metadata.ok_or(ShareError::BlobNotFound)?;

        if metadata.state == BlobState::Complete {
            debug!(hash = hex::encode(&hash[..8]), "Blob already complete");
            return Ok(());
        }

        info!(
            hash = hex::encode(&hash[..8]),
            name = metadata.display_name,
            "Starting blob pull"
        );

        self.execute_blob_pull(hash, &metadata).await
    }

    /// Create a pull plan for fetching a blob
    ///
    /// Determines which chunks we're missing and which peers have them.
    pub fn plan_blob_pull(
        &self,
        hash: &[u8; 32],
        metadata: &BlobMetadata,
        peers_with_chunks: &[PeerChunks],
    ) -> Result<PullPlan, ShareError> {
        // Get our current bitfield
        let local_bitfield = self.blob_store.get_chunk_bitfield(hash, metadata.total_chunks)?;

        // Find missing chunks
        let missing_chunks: Vec<u32> = local_bitfield
            .iter()
            .enumerate()
            .filter(|&(_, have)| !have)
            .map(|(i, _)| i as u32)
            .collect();

        if missing_chunks.is_empty() {
            return Ok(PullPlan {
                missing_chunks: Vec::new(),
                peer_assignments: Vec::new(),
                total_chunks: metadata.total_chunks,
            });
        }

        // Assign chunks to peers based on who has what
        let mut peer_assignments = Vec::new();

        for peer_info in peers_with_chunks {
            let chunks_from_peer: Vec<u32> = missing_chunks
                .iter()
                .filter(|&&c| c >= peer_info.chunk_start && c < peer_info.chunk_end)
                .copied()
                .collect();

            if !chunks_from_peer.is_empty() {
                peer_assignments.push(PeerAssignment {
                    peer_id: peer_info.endpoint_id,
                    chunks: chunks_from_peer,
                });
            }
        }

        info!(
            hash = hex::encode(&hash[..8]),
            missing = missing_chunks.len(),
            peers = peer_assignments.len(),
            "Created pull plan"
        );

        Ok(PullPlan {
            missing_chunks,
            peer_assignments,
            total_chunks: metadata.total_chunks,
        })
    }

    /// Process a received chunk during pull
    ///
    /// Verifies and stores the chunk, returns whether it was stored and if blob is complete.
    pub fn process_pull_chunk(
        &self,
        hash: &[u8; 32],
        chunk_index: u32,
        data: &[u8],
        total_size: u64,
        total_chunks: u32,
    ) -> Result<ChunkProcessResult, ShareError> {
        // Verify chunk
        let valid = self
            .blob_store
            .verify_chunk(hash, chunk_index, data)
            .unwrap_or(false);

        if !valid {
            return Ok(ChunkProcessResult {
                stored: false,
                blob_complete: false,
            });
        }

        // Store chunk
        self.blob_store
            .write_chunk(hash, chunk_index, data, total_size)?;

        // Check if complete
        let blob_complete = self.blob_store.is_complete(hash, total_chunks)?;

        Ok(ChunkProcessResult {
            stored: true,
            blob_complete,
        })
    }

    /// Mark a blob as complete in the database
    pub async fn mark_complete(&self, hash: &[u8; 32]) -> Result<(), ShareError> {
        let db_lock = self.db.lock().await;
        mark_blob_complete(&db_lock, hash).map_err(|e| ShareError::Database(e.to_string()))
    }

    /// Record that we received a section from a peer
    pub async fn record_section_trace(
        &self,
        hash: &[u8; 32],
        section_id: u8,
        from_peer: &[u8; 32],
    ) -> Result<(), ShareError> {
        let db_lock = self.db.lock().await;
        record_section_received(&db_lock, hash, section_id, from_peer)
            .map_err(|e| ShareError::Database(e.to_string()))
    }

    /// Calculate section ID from chunk index
    pub fn calculate_section_id(&self, chunk_index: u32, total_chunks: u32) -> u8 {
        let num_sections = calculate_num_sections(total_chunks);
        let chunks_per_section = (total_chunks + num_sections as u32 - 1) / num_sections as u32;
        (chunk_index / chunks_per_section) as u8
    }

    /// Request a single chunk from a peer (1:1 RPC)
    pub async fn request_chunk(
        &self,
        peer_id: &[u8; 32],
        hash: &[u8; 32],
        chunk_index: u32,
    ) -> Result<ChunkResponse, ShareError> {
        let node_id =
            EndpointId::from_bytes(peer_id).map_err(|e| ShareError::Connection(e.to_string()))?;

        let conn = self.connect_for_share(node_id).await?;

        let request = ShareMessage::ChunkRequest(ChunkRequest {
            hash: *hash,
            chunk_index,
        });

        // Open bidirectional stream
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        // Send request
        tokio::io::AsyncWriteExt::write_all(&mut send, &request.encode())
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))?;
        send.finish()
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        // Read single response
        let max_size = CHUNK_SIZE as usize + 1024;
        let data = recv
            .read_to_end(max_size)
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        match ShareMessage::decode(&data) {
            Ok(ShareMessage::ChunkResponse(resp)) => Ok(resp),
            _ => Err(ShareError::Protocol("invalid chunk response".to_string())),
        }
    }

    /// Request chunk map from source (1:1 RPC)
    pub async fn request_chunk_map(
        &self,
        source_id: &[u8; 32],
        hash: &[u8; 32],
    ) -> Result<ChunkMapResponse, ShareError> {
        let node_id = EndpointId::from_bytes(source_id)
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        let conn = self.connect_for_share(node_id).await?;

        let request = ShareMessage::ChunkMapRequest(ChunkMapRequest { hash: *hash });

        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        tokio::io::AsyncWriteExt::write_all(&mut send, &request.encode())
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))?;
        send.finish()
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        let data = recv
            .read_to_end(64 * 1024)
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        match ShareMessage::decode(&data) {
            Ok(ShareMessage::ChunkMapResponse(resp)) => Ok(resp),
            _ => Err(ShareError::Protocol(
                "invalid chunk map response".to_string(),
            )),
        }
    }

    /// Execute the blob pull process
    ///
    /// Coordinates chunk map request, chunk fetching, and completion announcement.
    pub async fn execute_blob_pull(
        &self,
        hash: &[u8; 32],
        metadata: &BlobMetadata,
    ) -> Result<(), ShareError> {
        // 1. Query source for chunk map
        let peers_with_chunks = match self.request_chunk_map(&metadata.source_id, hash).await {
            Ok(map) => map.peers,
            Err(e) => {
                debug!(error = %e, "Failed to get chunk map, will try source directly");
                vec![PeerChunks {
                    endpoint_id: metadata.source_id,
                    chunk_start: 0,
                    chunk_end: metadata.total_chunks,
                }]
            }
        };

        info!(
            hash = hex::encode(&hash[..8]),
            peers = peers_with_chunks.len(),
            "Starting pull from {} peer(s)",
            peers_with_chunks.len()
        );

        // 2. Get pull plan
        let plan = self.plan_blob_pull(hash, metadata, &peers_with_chunks)?;

        if plan.missing_chunks.is_empty() {
            info!(hash = hex::encode(&hash[..8]), "Already have all chunks");
            return Ok(());
        }

        info!(
            hash = hex::encode(&hash[..8]),
            missing = plan.missing_chunks.len(),
            total = metadata.total_chunks,
            "Need to fetch {} chunks",
            plan.missing_chunks.len()
        );

        // 3. Request chunks from peers (1:1 RPC per chunk)
        for assignment in &plan.peer_assignments {
            for &chunk_index in &assignment.chunks {
                match self
                    .request_chunk(&assignment.peer_id, hash, chunk_index)
                    .await
                {
                    Ok(resp) => {
                        let result = self.process_pull_chunk(
                            hash,
                            resp.chunk_index,
                            &resp.data,
                            metadata.total_size,
                            metadata.total_chunks,
                        )?;

                        if result.stored {
                            debug!(
                                chunk = resp.chunk_index,
                                from = hex::encode(&assignment.peer_id[..8]),
                                "Received chunk"
                            );
                        } else {
                            warn!(chunk = resp.chunk_index, "Chunk verification failed");
                        }
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            peer = hex::encode(&assignment.peer_id[..8]),
                            chunk = chunk_index,
                            "Failed to request chunk"
                        );
                    }
                }
            }

            // Record section trace
            if !assignment.chunks.is_empty() {
                let section_id =
                    self.calculate_section_id(assignment.chunks[0], metadata.total_chunks);
                let _ = self
                    .record_section_trace(hash, section_id, &assignment.peer_id)
                    .await;
            }
        }

        // 4. Check completion
        if self
            .blob_store
            .is_complete(hash, metadata.total_chunks)
            .unwrap_or(false)
        {
            self.mark_complete(hash).await?;

            info!(
                hash = hex::encode(&hash[..8]),
                name = metadata.display_name,
                "Blob pull complete!"
            );

            // Announce we can seed
            match self.get_can_seed_recipients(&metadata.scope_id).await {
                Ok(recipients) => {
                    if let Err(e) = self.announce_can_seed(&metadata.scope_id, hash, &recipients).await
                    {
                        warn!(error = %e, "Failed to announce can seed");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to get can seed recipients");
                }
            }
        } else {
            let bitfield = self
                .blob_store
                .get_chunk_bitfield(hash, metadata.total_chunks)
                .unwrap_or_default();
            let have = bitfield.iter().filter(|&&b| b).count();
            info!(
                hash = hex::encode(&hash[..8]),
                have = have,
                total = metadata.total_chunks,
                "Pull incomplete, will retry later"
            );
        }

        Ok(())
    }

    /// Receive and store a chunk (used by acquire flow)
    pub async fn receive_chunk(
        &self,
        hash: &[u8; 32],
        chunk_index: u32,
        data: &[u8],
        total_size: u64,
        from_peer: &[u8; 32],
    ) -> Result<bool, ShareError> {
        // Verify chunk (simplified - would use bao-tree for full verification)
        // For now just verify it's not empty and not too large
        if data.is_empty() || data.len() > CHUNK_SIZE as usize {
            return Err(ShareError::Protocol("invalid chunk size".to_string()));
        }

        // Write to storage
        self.blob_store
            .write_chunk(hash, chunk_index, data, total_size)?;

        debug!(
            hash = %hex::encode(&hash[..8]),
            chunk = chunk_index,
            from = %hex::encode(&from_peer[..8]),
            "received chunk"
        );

        // Check if this completes a section (for trace recording)
        let total_chunks = ((total_size + CHUNK_SIZE - 1) / CHUNK_SIZE) as u32;
        let bitfield = self.blob_store.get_chunk_bitfield(hash, total_chunks)?;

        // Check which sections are complete
        {
            let db_lock = self.db.lock().await;
            if let Ok(traces) = get_section_traces(&db_lock, hash) {
                for trace in traces {
                    // Check if all chunks in section are present
                    let section_complete = (trace.chunk_start..trace.chunk_end)
                        .all(|i| bitfield.get(i as usize).copied().unwrap_or(false));

                    if section_complete && trace.received_from.is_none() {
                        // Record trace
                        let _ = record_section_received(&db_lock, hash, trace.section_id, from_peer);
                        debug!(
                            hash = %hex::encode(&hash[..8]),
                            section = trace.section_id,
                            from = %hex::encode(&from_peer[..8]),
                            "section complete, recorded trace"
                        );
                    }
                }
            }
        }

        // Check if blob is complete
        let is_complete = self.blob_store.is_complete(hash, total_chunks)?;

        if is_complete {
            let db_lock = self.db.lock().await;
            mark_blob_complete(&db_lock, hash).map_err(|e| ShareError::Database(e.to_string()))?;

            info!(
                hash = %hex::encode(&hash[..8]),
                "blob complete"
            );
        }

        Ok(is_complete)
    }

    /// Get chunk map for a blob (for responding to ChunkMapRequest)
    pub async fn get_chunk_map(&self, hash: &[u8; 32]) -> Result<ChunkMapResponse, ShareError> {
        let db_lock = self.db.lock().await;

        // Get section traces (who has what)
        let traces =
            get_section_traces(&db_lock, hash).map_err(|e| ShareError::Database(e.to_string()))?;

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

        // Add ourselves if we're the source
        if let Ok(Some(blob)) = get_blob(&db_lock, hash) {
            if blob.source_id == self.identity.public_key && blob.state == BlobState::Complete {
                peers.push(PeerChunks {
                    endpoint_id: self.identity.public_key,
                    chunk_start: 0,
                    chunk_end: blob.total_chunks,
                });
            }
        }

        Ok(ChunkMapResponse {
            hash: *hash,
            peers,
        })
    }

    /// Get peer suggestion for a section (when we're busy)
    pub async fn get_peer_suggestion(
        &self,
        hash: &[u8; 32],
        section_id: u8,
    ) -> Result<Option<PeerSuggestion>, ShareError> {
        let db_lock = self.db.lock().await;

        if let Some(suggested_peer) = get_section_peer_suggestion(&db_lock, hash, section_id)
            .map_err(|e| ShareError::Database(e.to_string()))?
        {
            Ok(Some(PeerSuggestion {
                hash: *hash,
                section_id,
                suggested_peer,
            }))
        } else {
            Ok(None)
        }
    }

    /// Connect to a peer for Share protocol
    pub async fn connect_for_share(
        &self,
        node_id: EndpointId,
    ) -> Result<iroh::endpoint::Connection, ShareError> {
        let endpoint_id_bytes: [u8; 32] = *node_id.as_bytes();
        let relay_info = {
            let db = self.db.lock().await;
            get_peer_relay_info(&db, &endpoint_id_bytes).unwrap_or(None)
        };
        let (relay_url_str, relay_last_success) = match &relay_info {
            Some((url, ts)) => (Some(url.as_str()), *ts),
            None => (None, None),
        };
        let parsed_relay: Option<iroh::RelayUrl> =
            relay_url_str.and_then(|u| u.parse().ok());

        let result = connect::connect(
            &self.endpoint,
            node_id,
            parsed_relay.as_ref(),
            relay_last_success,
            SHARE_ALPN,
            self.config.connect_timeout,
        )
        .await
        .map_err(|e| ShareError::Connection(e.to_string()))?;

        if result.relay_url_confirmed {
            if let Some(url) = &parsed_relay {
                let db = self.db.lock().await;
                let _ = update_peer_relay_url(
                    &db,
                    &endpoint_id_bytes,
                    &url.to_string(),
                    current_timestamp(),
                );
            }
        }

        Ok(result.connection)
    }
}
