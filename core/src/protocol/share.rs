//! File sharing operations for the Protocol
//!
//! Provides methods for sharing large files (>512KB) with topic members.
//! Uses the BLAKE3-based chunking protocol for efficient P2P distribution.
//!
//! This module is a thin wrapper that coordinates between:
//! - ShareService (business logic in `network/share/service.rs`)
//! - Outgoing handlers (transport in `handlers/outgoing/share.rs`)

use std::path::Path;

use tracing::{debug, info, warn};

use crate::data::{get_blob, BlobMetadata, BlobState};
use crate::handlers::outgoing::push_section_to_peer_standalone;
use crate::network::share::protocol::PeerChunks;

use super::core::Protocol;
use super::error::ProtocolError;

// Re-export ShareStatus from service for public API
pub use crate::network::share::service::ShareStatus;

impl Protocol {
    /// Share a file with all members of a topic
    ///
    /// The file must be at least 512KB. For smaller files, use the regular `send()` method.
    ///
    /// Returns the BLAKE3 hash of the file on success.
    ///
    /// # Example
    /// ```ignore
    /// let hash = protocol.share_file(&topic_id, "/path/to/large_video.mp4").await?;
    /// println!("Shared file with hash: {}", hex::encode(hash));
    /// ```
    pub async fn share_file(
        &self,
        topic_id: &[u8; 32],
        file_path: impl AsRef<Path>,
    ) -> Result<[u8; 32], ProtocolError> {
        let file_path = file_path.as_ref();

        // Delegate to share service for import and metadata storage
        let (hash, total_size, total_chunks, num_sections, display_name) = self
            .share_service
            .share_file(file_path, topic_id)
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))?;

        info!(
            hash = hex::encode(&hash[..8]),
            size = total_size,
            chunks = total_chunks,
            sections = num_sections,
            "File imported, announcing to topic"
        );

        // Get announcement plan from service (business logic)
        let plan = self
            .share_service
            .prepare_announcement(
                topic_id,
                &hash,
                total_size,
                total_chunks,
                num_sections,
                &display_name,
            )
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))?;

        // Send announcement via SendService
        self.send_service
            .send_to_topic(
                topic_id,
                &plan.message_bytes,
                &plan.recipients,
                crate::network::harbor::protocol::HarborPacketType::Content,
            )
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        info!(
            hash = hex::encode(&hash[..8]),
            members = plan.recipients.len(),
            "File announced to topic"
        );

        // Start pushing sections to initial recipients (transport via handler)
        for push in plan.section_pushes {
            let endpoint = self.endpoint.clone();
            let blob_store = self.share_service.blob_store().clone();
            let hash_copy = hash;

            tokio::spawn(async move {
                if let Err(e) = push_section_to_peer_standalone(
                    &endpoint,
                    &blob_store,
                    &push.peer_id,
                    &hash_copy,
                    push.chunk_start,
                    push.chunk_end,
                )
                .await
                {
                    warn!(
                        error = %e,
                        peer = hex::encode(&push.peer_id[..8]),
                        section = push.section_id,
                        "Failed to push section"
                    );
                }
            });
        }

        Ok(hash)
    }

    /// Get the status of a shared file
    pub async fn get_share_status(&self, hash: &[u8; 32]) -> Result<ShareStatus, ProtocolError> {
        self.share_service
            .get_status(hash)
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
    }

    /// List all shared files for a topic
    pub async fn list_topic_blobs(
        &self,
        topic_id: &[u8; 32],
    ) -> Result<Vec<BlobMetadata>, ProtocolError> {
        self.share_service
            .list_blobs(topic_id)
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
    }

    /// Request to download a file by hash
    ///
    /// This starts the pull process to download a file that was announced by another peer.
    pub async fn request_blob(&self, hash: &[u8; 32]) -> Result<(), ProtocolError> {
        // Check if we already have this blob
        let metadata = {
            let db = self.db.lock().await;
            get_blob(&db, hash).map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        let metadata = metadata.ok_or_else(|| {
            ProtocolError::NotFound("Blob not found. Wait for announcement first.".to_string())
        })?;

        if metadata.state == BlobState::Complete {
            debug!(hash = hex::encode(&hash[..8]), "Blob already complete");
            return Ok(());
        }

        info!(
            hash = hex::encode(&hash[..8]),
            name = metadata.display_name,
            "Starting blob pull"
        );

        // Execute the pull process
        self.execute_blob_pull(hash, &metadata).await
    }

    /// Export a completed blob to a file
    pub async fn export_blob(
        &self,
        hash: &[u8; 32],
        dest_path: impl AsRef<Path>,
    ) -> Result<(), ProtocolError> {
        self.share_service
            .export_blob_async(hash, dest_path.as_ref())
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
    }

    // ========================================================================
    // Pull coordination - orchestrates service (business logic) and handlers (transport)
    // ========================================================================

    /// Execute the blob pull process
    ///
    /// Coordinates between service layer (planning) and outgoing handlers (transport).
    async fn execute_blob_pull(
        &self,
        hash: &[u8; 32],
        metadata: &BlobMetadata,
    ) -> Result<(), ProtocolError> {
        // 1. Query source for chunk map (transport operation)
        let peers_with_chunks = match self.request_chunk_map(&metadata.source_id, hash).await {
            Ok(map) => map.peers,
            Err(e) => {
                debug!(error = %e, "Failed to get chunk map, will try source directly");
                // Fall back to source only
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

        // 2. Get pull plan from service (business logic)
        let plan = self
            .share_service
            .plan_blob_pull(hash, metadata, &peers_with_chunks)
            .map_err(|e| ProtocolError::Database(e.to_string()))?;

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

        // 3. Request chunks from peers (transport) and process results (service)
        for assignment in &plan.peer_assignments {
            // Request in batches of 10 chunks
            for batch in assignment.chunks.chunks(10) {
                match self.request_chunks(&assignment.peer_id, hash, batch).await {
                    Ok(responses) => {
                        for resp in responses {
                            // Process chunk through service layer
                            let result = self
                                .share_service
                                .process_pull_chunk(
                                    hash,
                                    resp.chunk_index,
                                    &resp.data,
                                    metadata.total_size,
                                    metadata.total_chunks,
                                )
                                .map_err(|e| ProtocolError::Database(e.to_string()))?;

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
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            peer = hex::encode(&assignment.peer_id[..8]),
                            "Failed to request chunks"
                        );
                    }
                }
            }

            // Record section trace via service
            if !assignment.chunks.is_empty() {
                let section_id = self
                    .share_service
                    .calculate_section_id(assignment.chunks[0], metadata.total_chunks);
                let _ = self
                    .share_service
                    .record_section_trace(hash, section_id, &assignment.peer_id)
                    .await;
            }
        }

        // 4. Check completion via service
        let blob_store = self.share_service.blob_store();
        if blob_store
            .is_complete(hash, metadata.total_chunks)
            .unwrap_or(false)
        {
            self.share_service
                .mark_complete(hash)
                .await
                .map_err(|e| ProtocolError::Database(e.to_string()))?;

            info!(
                hash = hex::encode(&hash[..8]),
                name = metadata.display_name,
                "Blob pull complete!"
            );

            // Announce we can seed (transport operation)
            match self
                .share_service
                .get_can_seed_recipients(&metadata.topic_id)
                .await
            {
                Ok(recipients) => {
                    if let Err(e) = self
                        .announce_can_seed(&metadata.topic_id, hash, &recipients)
                        .await
                    {
                        warn!(error = %e, "Failed to announce can seed");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to get can seed recipients");
                }
            }
        } else {
            let bitfield = blob_store
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{BlobState, CHUNK_SIZE};
    use crate::network::share::service::ShareStatus;

    #[test]
    fn test_share_status_fields() {
        let status = ShareStatus {
            hash: [1u8; 32],
            display_name: "test.bin".to_string(),
            total_size: 1024 * 1024,
            total_chunks: 2,
            num_sections: 2,
            state: BlobState::Partial,
            local_chunks: vec![true, false],
            chunks_complete: 1,
            section_traces: vec![],
        };

        assert_eq!(status.display_name, "test.bin");
        assert_eq!(status.total_chunks, 2);
        assert_eq!(status.chunks_complete, 1);
        assert_eq!(status.local_chunks.len(), 2);
    }

    #[test]
    fn test_share_status_complete() {
        let status = ShareStatus {
            hash: [2u8; 32],
            display_name: "complete.bin".to_string(),
            total_size: CHUNK_SIZE * 3,
            total_chunks: 3,
            num_sections: 3,
            state: BlobState::Complete,
            local_chunks: vec![true, true, true],
            chunks_complete: 3,
            section_traces: vec![],
        };

        assert!(matches!(status.state, BlobState::Complete));
        assert_eq!(status.chunks_complete, status.total_chunks);
        assert!(status.local_chunks.iter().all(|&b| b));
    }

    #[test]
    fn test_section_calculation_math() {
        let total_chunks = 100u32;
        let num_sections = 5u8;
        let chunks_per_section = (total_chunks + num_sections as u32 - 1) / num_sections as u32;

        assert_eq!(chunks_per_section, 20);

        for section_id in 0..num_sections {
            let chunk_start = section_id as u32 * chunks_per_section;
            let chunk_end = std::cmp::min(chunk_start + chunks_per_section, total_chunks);

            assert_eq!(chunk_end - chunk_start, 20);
        }
    }

    #[test]
    fn test_section_calculation_uneven() {
        let total_chunks = 23u32;
        let num_sections = 5u8;
        let chunks_per_section = (total_chunks + num_sections as u32 - 1) / num_sections as u32;

        assert_eq!(chunks_per_section, 5);

        let last_section_start = 4 * chunks_per_section;
        let last_section_end = std::cmp::min(last_section_start + chunks_per_section, total_chunks);

        assert_eq!(last_section_start, 20);
        assert_eq!(last_section_end, 23);
    }

    #[test]
    fn test_missing_chunks_detection() {
        let local_chunks = vec![true, false, true, false, true];

        let missing: Vec<u32> = local_chunks
            .iter()
            .enumerate()
            .filter(|&(_, have)| !have)
            .map(|(i, _)| i as u32)
            .collect();

        assert_eq!(missing, vec![1, 3]);
    }

    #[test]
    fn test_all_chunks_present() {
        let local_chunks = vec![true, true, true, true];

        let missing: Vec<u32> = local_chunks
            .iter()
            .enumerate()
            .filter(|&(_, have)| !have)
            .map(|(i, _)| i as u32)
            .collect();

        assert!(missing.is_empty());
    }

    #[test]
    fn test_chunk_batching() {
        let chunks: Vec<u32> = (0..25).collect();
        let batch_size = 10;

        let batches: Vec<&[u32]> = chunks.chunks(batch_size).collect();

        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 10);
        assert_eq!(batches[1].len(), 10);
        assert_eq!(batches[2].len(), 5);
    }
}
