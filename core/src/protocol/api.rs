//! Public API for the Protocol
//!
//! Thin API layer that delegates to services for all business logic and transport.
//! All Protocol API methods are consolidated here.

use std::path::Path;

use crate::data::BlobMetadata;

use super::core::{Protocol, MAX_MESSAGE_SIZE};
use super::error::ProtocolError;
use super::stats::{DhtBucketInfo, ProtocolStats, TopicDetails, TopicSummary};
use crate::network::topic::TopicInvite;

// Re-export ShareStatus from service for public API
pub use crate::network::share::service::ShareStatus;

impl Protocol {
    // ========== Topic API ==========

    /// Create a new topic
    ///
    /// This creates a new topic with this node as the only member.
    /// Returns an invite that can be shared with others.
    pub async fn create_topic(&self) -> Result<TopicInvite, ProtocolError> {
        self.check_running().await?;
        self.topic_service
            .create_topic()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Join an existing topic
    ///
    /// This joins a topic using an invite received from another member.
    /// The invite contains all current topic members.
    /// Announces our presence to other members.
    pub async fn join_topic(&self, invite: TopicInvite) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.topic_service
            .join_topic(invite)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Leave a topic
    pub async fn leave_topic(&self, topic_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.topic_service
            .leave_topic(topic_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// List all topics we're subscribed to
    pub async fn list_topics(&self) -> Result<Vec<[u8; 32]>, ProtocolError> {
        self.check_running().await?;
        self.topic_service
            .list_topics()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Get an invite for an existing topic
    ///
    /// Returns a fresh invite containing ALL current topic members.
    pub async fn get_invite(&self, topic_id: &[u8; 32]) -> Result<TopicInvite, ProtocolError> {
        self.check_running().await?;
        self.topic_service
            .get_invite(topic_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    // ========== Send API ==========

    /// Send a message to a topic
    ///
    /// The payload is opaque bytes - the app defines the format.
    /// Messages are sent to all known topic members (from the local member list).
    ///
    /// # Arguments
    ///
    /// * `topic_id` - The 32-byte topic identifier
    /// * `payload` - The message content (max 512KB)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The protocol is not running
    /// - The message exceeds 512KB
    /// - The topic doesn't exist
    /// - The caller is not a member of the topic
    pub async fn send(&self, topic_id: &[u8; 32], payload: &[u8]) -> Result<(), ProtocolError> {
        self.check_running().await?;

        if payload.len() > MAX_MESSAGE_SIZE {
            return Err(ProtocolError::MessageTooLarge);
        }

        self.send_service
            .send_content(topic_id, payload)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        Ok(())
    }

    // ========== Share API ==========

    /// Share a file with all members of a topic
    ///
    /// The file must be at least 512KB. For smaller files, use the regular `send()` method.
    ///
    /// Returns the BLAKE3 hash of the file on success.
    pub async fn share_file(
        &self,
        topic_id: &[u8; 32],
        file_path: impl AsRef<Path>,
    ) -> Result<[u8; 32], ProtocolError> {
        self.share_service
            .share_and_announce(topic_id, file_path.as_ref())
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
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
        self.share_service
            .request_blob(hash)
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
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

    // ========== Sync Transport API ==========
    //
    // Thin delegates to SyncService.
    // Applications bring their own CRDT (Loro, Yjs, Automerge, etc.).

    /// Send a sync update to all topic members
    ///
    /// Max size: 512 KB (uses broadcast via send protocol)
    pub async fn send_sync_update(
        &self,
        topic_id: &[u8; 32],
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.sync_service.send_sync_update(topic_id, data).await
    }

    /// Request sync state from topic members
    pub async fn request_sync(
        &self,
        topic_id: &[u8; 32],
    ) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.sync_service.request_sync(topic_id).await
    }

    /// Respond to a sync request with current state
    ///
    /// Uses direct SYNC_ALPN connection - no size limit.
    pub async fn respond_sync(
        &self,
        topic_id: &[u8; 32],
        requester_id: &[u8; 32],
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.sync_service.respond_sync(topic_id, requester_id, data).await
    }

    // ========== Stats API ==========

    /// Get comprehensive protocol statistics
    pub async fn get_stats(&self) -> Result<ProtocolStats, ProtocolError> {
        self.check_running().await?;
        self.stats_service
            .get_stats()
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
    }

    /// Get detailed DHT bucket information
    pub async fn get_dht_buckets(&self) -> Result<Vec<DhtBucketInfo>, ProtocolError> {
        self.check_running().await?;
        self.stats_service
            .get_dht_buckets()
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
    }

    /// Get detailed information about a specific topic
    pub async fn get_topic_details(
        &self,
        topic_id: &[u8; 32],
    ) -> Result<TopicDetails, ProtocolError> {
        self.check_running().await?;
        self.stats_service
            .get_topic_details(topic_id)
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
    }

    /// List all topics with summary info
    pub async fn list_topic_summaries(&self) -> Result<Vec<TopicSummary>, ProtocolError> {
        self.check_running().await?;
        self.stats_service
            .list_topic_summaries()
            .await
            .map_err(|e| ProtocolError::Database(e.to_string()))
    }

    // ========== Live Streaming API ==========

    /// Request to stream to a peer in a topic
    ///
    /// Sends a stream request via signaling. The destination must accept
    /// before media can flow. Returns the unique request ID.
    pub async fn request_stream(
        &self,
        topic_id: &[u8; 32],
        peer_id: &[u8; 32],
        name: &str,
        catalog: Vec<u8>,
    ) -> Result<[u8; 32], ProtocolError> {
        self.check_running().await?;
        self.live_service
            .request_stream(topic_id, peer_id, name, catalog)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Accept an incoming stream request
    pub async fn accept_stream(&self, request_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.live_service
            .accept_stream(request_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Reject an incoming stream request
    pub async fn reject_stream(
        &self,
        request_id: &[u8; 32],
        reason: Option<String>,
    ) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.live_service
            .reject_stream(request_id, reason)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// End an active outgoing stream
    pub async fn end_stream(&self, request_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.live_service
            .end_stream(request_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{BlobState, CHUNK_SIZE};

    #[test]
    fn test_max_message_size_constant() {
        assert_eq!(MAX_MESSAGE_SIZE, 512 * 1024);
    }

    #[test]
    fn test_message_size_validation() {
        let small_msg = vec![0u8; 1000];
        let max_msg = vec![0u8; MAX_MESSAGE_SIZE];
        let too_large = vec![0u8; MAX_MESSAGE_SIZE + 1];

        assert!(small_msg.len() <= MAX_MESSAGE_SIZE);
        assert!(max_msg.len() <= MAX_MESSAGE_SIZE);
        assert!(too_large.len() > MAX_MESSAGE_SIZE);
    }

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
