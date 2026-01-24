//! File sharing operations for the Protocol
//!
//! Provides methods for sharing large files (>512KB) with topic members.
//! Uses the BLAKE3-based chunking protocol for efficient P2P distribution.
//!
//! This module is a thin wrapper around ShareService - all business logic
//! lives in `network/share/service.rs`.

use std::path::Path;
use std::time::Duration;

use iroh::{NodeAddr, NodeId};
use tracing::{debug, info, warn};

use crate::data::{
    get_blob, get_topic_members_with_info, mark_blob_complete,
    record_section_received, BlobMetadata, BlobState, BlobStore,
};
use crate::network::share::protocol::{
    ChunkResponse, ShareMessage, SHARE_ALPN,
};

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
            .share_file(file_path, topic_id, &self.db)
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))?;

        info!(
            hash = hex::encode(&hash[..8]),
            size = total_size,
            chunks = total_chunks,
            sections = num_sections,
            "File imported, announcing to topic"
        );

        // Announce to topic members (transport operation)
        self.announce_file_to_topic(topic_id, &hash, total_size, total_chunks, num_sections, &display_name)
            .await?;

        Ok(hash)
    }

    /// Get the status of a shared file
    pub async fn get_share_status(&self, hash: &[u8; 32]) -> Result<ShareStatus, ProtocolError> {
        self.share_service
            .get_status(hash, &self.db)
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
    }

    /// List all shared files for a topic
    pub async fn list_topic_blobs(&self, topic_id: &[u8; 32]) -> Result<Vec<BlobMetadata>, ProtocolError> {
        self.share_service
            .list_blobs(topic_id, &self.db)
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
            get_blob(&db, hash)
                .map_err(|e| ProtocolError::Database(e.to_string()))?
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

        // Start the pull process (transport operation)
        self.start_blob_pull(hash, &metadata).await
    }

    /// Export a completed blob to a file
    pub async fn export_blob(
        &self,
        hash: &[u8; 32],
        dest_path: impl AsRef<Path>,
    ) -> Result<(), ProtocolError> {
        self.share_service
            .export_blob_async(hash, dest_path.as_ref(), &self.db)
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
    }

    // ========================================================================
    // Transport operations - these need Protocol access for endpoint/connections
    // ========================================================================

    /// Announce a file to topic members
    async fn announce_file_to_topic(
        &self,
        topic_id: &[u8; 32],
        hash: &[u8; 32],
        total_size: u64,
        total_chunks: u32,
        num_sections: u8,
        display_name: &str,
    ) -> Result<(), ProtocolError> {
        use crate::network::send::topic_messages::{TopicMessage, FileAnnouncementMessage};

        // Get topic members
        let members = {
            let db = self.db.lock().await;
            get_topic_members_with_info(&db, topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        // Filter out self
        let other_members: Vec<_> = members
            .iter()
            .filter(|m| m.endpoint_id != self.identity.public_key)
            .collect();

        if other_members.is_empty() {
            warn!("No other members in topic to share with");
            return Ok(());
        }

        // Create the FileAnnouncement as a proper TopicMessage control type
        let topic_msg = TopicMessage::FileAnnouncement(FileAnnouncementMessage::new(
            *hash,
            self.identity.public_key,
            total_size,
            total_chunks,
            num_sections,
            display_name.to_string(),
        ));

        // Encode and send via the Send protocol
        let recipient_ids: Vec<[u8; 32]> = members.iter().map(|m| m.endpoint_id).collect();

        self.send_raw(
            topic_id,
            &topic_msg.encode(),
            &recipient_ids,
            crate::network::harbor::protocol::HarborPacketType::Content,
        ).await?;

        info!(
            hash = hex::encode(&hash[..8]),
            members = other_members.len(),
            "File announced to topic"
        );

        // Start pushing chunks to initial recipients (up to 5)
        let initial_count = std::cmp::min(5, other_members.len());
        let chunks_per_section = (total_chunks + num_sections as u32 - 1) / num_sections as u32;

        for (i, member) in other_members.iter().take(initial_count).enumerate() {
            let section_id = i as u8 % num_sections;
            let chunk_start = section_id as u32 * chunks_per_section;
            let chunk_end = std::cmp::min(chunk_start + chunks_per_section, total_chunks);

            // Spawn a task to push this section to the peer
            let endpoint = self.endpoint.clone();
            let peer_id = member.endpoint_id;
            let hash_copy = *hash;
            let blob_store = self.share_service.blob_store().clone();

            tokio::spawn(async move {
                if let Err(e) = Self::push_section_to_peer(
                    &endpoint,
                    &peer_id,
                    &hash_copy,
                    chunk_start,
                    chunk_end,
                    &blob_store,
                ).await {
                    warn!(
                        error = %e,
                        peer = hex::encode(&peer_id[..8]),
                        section = section_id,
                        "Failed to push section"
                    );
                }
            });
        }

        Ok(())
    }

    /// Push a section of chunks to a peer
    async fn push_section_to_peer(
        endpoint: &iroh::Endpoint,
        peer_id: &[u8; 32],
        hash: &[u8; 32],
        chunk_start: u32,
        chunk_end: u32,
        blob_store: &BlobStore,
    ) -> Result<(), ProtocolError> {
        let node_id = NodeId::from_bytes(peer_id)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        let node_addr: NodeAddr = node_id.into();

        // Connect to peer
        let conn = tokio::time::timeout(
            Duration::from_secs(15),
            endpoint.connect(node_addr, SHARE_ALPN),
        )
        .await
        .map_err(|_| ProtocolError::Network("connect timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))?;

        info!(
            peer = hex::encode(&peer_id[..8]),
            hash = hex::encode(&hash[..8]),
            chunks = format!("{}-{}", chunk_start, chunk_end),
            "Pushing section to peer"
        );

        // Send each chunk in the section
        for chunk_index in chunk_start..chunk_end {
            // Read chunk from storage
            let data = blob_store.read_chunk(hash, chunk_index)
                .map_err(|e| ProtocolError::Database(format!("Failed to read chunk: {}", e)))?;

            // Create response message (we're pushing, so we send as response)
            let msg = ShareMessage::ChunkResponse(ChunkResponse {
                hash: *hash,
                chunk_index,
                data,
            });

            // Open stream and send
            let mut send = conn.open_uni().await
                .map_err(|e| ProtocolError::Network(e.to_string()))?;

            tokio::io::AsyncWriteExt::write_all(&mut send, &msg.encode()).await
                .map_err(|e| ProtocolError::Network(e.to_string()))?;
            send.finish()
                .map_err(|e| ProtocolError::Network(e.to_string()))?;

            debug!(
                chunk = chunk_index,
                peer = hex::encode(&peer_id[..8]),
                "Pushed chunk"
            );
        }

        info!(
            peer = hex::encode(&peer_id[..8]),
            hash = hex::encode(&hash[..8]),
            chunks = chunk_end - chunk_start,
            "Section push complete"
        );

        Ok(())
    }

    /// Start pulling a blob from peers
    async fn start_blob_pull(
        &self,
        hash: &[u8; 32],
        metadata: &BlobMetadata,
    ) -> Result<(), ProtocolError> {
        let blob_store = self.share_service.blob_store();

        // 1. Query source for chunk map to know who has what
        let chunk_map = self.request_chunk_map(&metadata.source_id, hash).await;

        let peers_with_chunks = match chunk_map {
            Ok(map) => map.peers,
            Err(e) => {
                debug!(error = %e, "Failed to get chunk map, will try source directly");
                // Fall back to source only
                vec![crate::network::share::protocol::PeerChunks {
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

        // 2. Determine which chunks we need
        let local_bitfield = blob_store.get_chunk_bitfield(hash, metadata.total_chunks)
            .map_err(|e| ProtocolError::Database(format!("Failed to get bitfield: {}", e)))?;

        let missing_chunks: Vec<u32> = local_bitfield.iter()
            .enumerate()
            .filter(|&(_, have)| !have)
            .map(|(i, _)| i as u32)
            .collect();

        if missing_chunks.is_empty() {
            info!(hash = hex::encode(&hash[..8]), "Already have all chunks");
            return Ok(());
        }

        info!(
            hash = hex::encode(&hash[..8]),
            missing = missing_chunks.len(),
            total = metadata.total_chunks,
            "Need to fetch {} chunks",
            missing_chunks.len()
        );

        // 3. Request chunks from appropriate peers
        for peer_info in &peers_with_chunks {
            // Find chunks this peer has that we need
            let chunks_to_request: Vec<u32> = missing_chunks.iter()
                .filter(|&&c| c >= peer_info.chunk_start && c < peer_info.chunk_end)
                .copied()
                .collect();

            if chunks_to_request.is_empty() {
                continue;
            }

            // Request in batches of 10 chunks
            for batch in chunks_to_request.chunks(10) {
                match self.request_chunks(&peer_info.endpoint_id, hash, batch).await {
                    Ok(responses) => {
                        for resp in responses {
                            // Verify and store chunk
                            if blob_store.verify_chunk(hash, resp.chunk_index, &resp.data)
                                .unwrap_or(false)
                            {
                                if let Err(e) = blob_store.write_chunk(
                                    hash,
                                    resp.chunk_index,
                                    &resp.data,
                                    metadata.total_size,
                                ) {
                                    warn!(error = %e, chunk = resp.chunk_index, "Failed to write chunk");
                                } else {
                                    debug!(
                                        chunk = resp.chunk_index,
                                        from = hex::encode(&peer_info.endpoint_id[..8]),
                                        "Received chunk"
                                    );
                                }
                            } else {
                                warn!(chunk = resp.chunk_index, "Chunk verification failed");
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            peer = hex::encode(&peer_info.endpoint_id[..8]),
                            "Failed to request chunks"
                        );
                    }
                }
            }

            // Record section trace
            let section_id = (peer_info.chunk_start / ((metadata.total_chunks + 4) / 5)) as u8;
            let db = self.db.lock().await;
            let _ = record_section_received(&db, hash, section_id, &peer_info.endpoint_id);
        }

        // 4. Check if we're now complete
        if blob_store.is_complete(hash, metadata.total_chunks).unwrap_or(false) {
            let db = self.db.lock().await;
            mark_blob_complete(&db, hash)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;

            info!(
                hash = hex::encode(&hash[..8]),
                name = metadata.display_name,
                "Blob pull complete!"
            );

            // Announce we can seed
            drop(db); // Release lock before async call
            match self.share_service.get_can_seed_recipients(&metadata.topic_id, &self.db).await {
                Ok(recipients) => {
                    if let Err(e) = self.announce_can_seed(&metadata.topic_id, hash, &recipients).await {
                        warn!(error = %e, "Failed to announce can seed");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to get can seed recipients");
                }
            }
        } else {
            let new_bitfield = blob_store.get_chunk_bitfield(hash, metadata.total_chunks)
                .unwrap_or_default();
            let have = new_bitfield.iter().filter(|&&b| b).count();
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
    use crate::network::share::service::ShareStatus;
    use crate::data::{BlobState, CHUNK_SIZE};

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
        // Verify section math is correct
        let total_chunks = 100u32;
        let num_sections = 5u8;
        let chunks_per_section = (total_chunks + num_sections as u32 - 1) / num_sections as u32;

        assert_eq!(chunks_per_section, 20); // 100 / 5 = 20 chunks per section

        for section_id in 0..num_sections {
            let chunk_start = section_id as u32 * chunks_per_section;
            let chunk_end = std::cmp::min(chunk_start + chunks_per_section, total_chunks);

            assert_eq!(chunk_end - chunk_start, 20);
        }
    }

    #[test]
    fn test_section_calculation_uneven() {
        // When chunks don't divide evenly
        let total_chunks = 23u32;
        let num_sections = 5u8;
        let chunks_per_section = (total_chunks + num_sections as u32 - 1) / num_sections as u32;

        assert_eq!(chunks_per_section, 5); // ceil(23 / 5) = 5

        // Verify last section doesn't exceed total
        let last_section_start = 4 * chunks_per_section;
        let last_section_end = std::cmp::min(last_section_start + chunks_per_section, total_chunks);

        assert_eq!(last_section_start, 20);
        assert_eq!(last_section_end, 23); // Only 3 chunks in last section
    }

    #[test]
    fn test_missing_chunks_detection() {
        let local_chunks = vec![true, false, true, false, true];

        let missing: Vec<u32> = local_chunks.iter()
            .enumerate()
            .filter(|&(_, have)| !have)
            .map(|(i, _)| i as u32)
            .collect();

        assert_eq!(missing, vec![1, 3]);
    }

    #[test]
    fn test_all_chunks_present() {
        let local_chunks = vec![true, true, true, true];

        let missing: Vec<u32> = local_chunks.iter()
            .enumerate()
            .filter(|&(_, have)| !have)
            .map(|(i, _)| i as u32)
            .collect();

        assert!(missing.is_empty());
    }

    #[test]
    fn test_chunk_batching() {
        // Test that chunks are batched correctly for requests
        let chunks: Vec<u32> = (0..25).collect();
        let batch_size = 10;

        let batches: Vec<&[u32]> = chunks.chunks(batch_size).collect();

        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 10);
        assert_eq!(batches[1].len(), 10);
        assert_eq!(batches[2].len(), 5);
    }
}
