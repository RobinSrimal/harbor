//! Public API for the Protocol
//!
//! Thin API layer that delegates to services for all business logic and transport.
//! All Protocol API methods are consolidated here.

use std::path::Path;

use crate::data::BlobMetadata;

use super::core::{MAX_MESSAGE_SIZE, Protocol};
use super::error::ProtocolError;
use super::stats::{DhtBucketInfo, ProtocolStats, TopicDetails, TopicSummary};
use crate::network::control::TopicInvite;

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
        self.control_service
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
        self.control_service
            .join_topic(invite)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Leave a topic
    pub async fn leave_topic(&self, topic_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.control_service
            .leave_topic(topic_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// List all topics we're subscribed to
    pub async fn list_topics(&self) -> Result<Vec<[u8; 32]>, ProtocolError> {
        self.check_running().await?;
        self.control_service
            .list_topics()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Get an invite for an existing topic
    ///
    /// Returns a fresh invite containing ALL current topic members.
    pub async fn get_invite(&self, topic_id: &[u8; 32]) -> Result<TopicInvite, ProtocolError> {
        self.check_running().await?;
        self.control_service
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
    pub async fn send(&self, target: super::Target, payload: &[u8]) -> Result<(), ProtocolError> {
        self.check_running().await?;

        if payload.len() > MAX_MESSAGE_SIZE {
            return Err(ProtocolError::MessageTooLarge);
        }

        match target {
            super::Target::Topic(topic_id) => {
                self.send_service
                    .send_topic(&topic_id, payload)
                    .await
                    .map_err(|e| ProtocolError::Network(e.to_string()))?;
            }
            super::Target::Dm(recipient_id) => {
                self.send_service
                    .send_dm(&recipient_id, payload)
                    .await
                    .map_err(|e| ProtocolError::Network(e.to_string()))?;
            }
        }

        Ok(())
    }

    // ========== Share API ==========

    /// Share a file
    ///
    /// - `Target::Topic(id)` — share with all topic members
    /// - `Target::Dm(id)` — share directly with a single peer
    ///
    /// The file must be at least 512KB. For smaller files, use the regular `send()` method.
    ///
    /// Returns the BLAKE3 hash of the file on success.
    pub async fn share_file(
        &self,
        target: super::Target,
        file_path: impl AsRef<Path>,
    ) -> Result<[u8; 32], ProtocolError> {
        self.check_running().await?;
        match target {
            super::Target::Topic(id) => self
                .share_service
                .share_and_announce(&id, file_path.as_ref())
                .await
                .map_err(|e| ProtocolError::Database(e.to_string())),
            super::Target::Dm(id) => self
                .share_service
                .share_dm_file(&id, file_path.as_ref())
                .await
                .map_err(|e| ProtocolError::Database(e.to_string())),
        }
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

    /// Send a sync update
    ///
    /// - `Target::Topic(id)` — broadcast to all topic members (max 512 KB)
    /// - `Target::Dm(id)` — send to a single peer via DM (max 512 KB)
    pub async fn send_sync_update(
        &self,
        target: super::Target,
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        self.check_running().await?;
        match target {
            super::Target::Topic(id) => self.sync_service.send_sync_update(&id, data).await,
            super::Target::Dm(id) => self.sync_service.send_dm_sync_update(&id, data).await,
        }
    }

    /// Request sync state
    ///
    /// - `Target::Topic(id)` — request from all topic members
    /// - `Target::Dm(id)` — request from a single peer
    pub async fn request_sync(&self, target: super::Target) -> Result<(), ProtocolError> {
        self.check_running().await?;
        match target {
            super::Target::Topic(id) => self.sync_service.request_sync(&id).await,
            super::Target::Dm(id) => self.sync_service.request_dm_sync(&id).await,
        }
    }

    /// Respond to a sync request with current state
    ///
    /// Uses direct SYNC_ALPN connection - no size limit.
    ///
    /// - `Target::Topic(id)` — respond to a topic sync request from `requester_id`
    /// - `Target::Dm(id)` — respond to a DM sync request (target peer is the recipient)
    pub async fn respond_sync(
        &self,
        target: super::Target,
        requester_id: &[u8; 32],
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        self.check_running().await?;
        match target {
            super::Target::Topic(id) => {
                self.sync_service
                    .respond_sync(&id, requester_id, data)
                    .await
            }
            super::Target::Dm(id) => self.sync_service.respond_dm_sync(&id, data).await,
        }
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

    // ========== Stream API ==========

    /// Request to stream to a peer in a topic
    ///
    /// Sends stream request signaling. Topic fanout uses one request per recipient
    /// grouped under a shared broadcast ID.
    /// Returns request_id for single-target calls, or broadcast_id for fanout.
    pub async fn request_stream(
        &self,
        topic_id: &[u8; 32],
        peer_id: &[u8; 32],
        name: &str,
        catalog: Vec<u8>,
    ) -> Result<[u8; 32], ProtocolError> {
        self.check_running().await?;
        self.stream_service
            .request_stream(topic_id, peer_id, name, catalog)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Request a DM stream to a peer (peer-to-peer, no topic)
    ///
    /// Sends a stream request directly via DM. The destination must accept
    /// before media can flow. Returns the request_id.
    pub async fn dm_stream_request(
        &self,
        peer_id: &[u8; 32],
        name: &str,
    ) -> Result<[u8; 32], ProtocolError> {
        self.check_running().await?;
        self.stream_service
            .dm_request_stream(peer_id, name)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Accept an incoming stream request
    pub async fn accept_stream(&self, request_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.stream_service
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
        self.stream_service
            .reject_stream(request_id, reason)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// End an active outgoing stream
    pub async fn end_stream(&self, request_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.stream_service
            .end_stream(request_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Get an active MOQ session for a stream
    ///
    /// Returns the session after `StreamConnected` has been emitted.
    /// Use `create_broadcast` / `consume_broadcast` on the returned session.
    pub async fn get_stream_session(
        &self,
        request_id: &[u8; 32],
    ) -> Result<std::sync::Arc<crate::network::stream::session::StreamSession>, ProtocolError> {
        self.check_running().await?;
        self.stream_service
            .get_session(request_id)
            .await
            .ok_or_else(|| ProtocolError::NotFound("stream session not found".into()))
    }

    /// Create a broadcast producer on an active MOQ session (source side)
    ///
    /// Call after receiving `StreamConnected` with `is_source: true`.
    /// Returns a `BroadcastProducer` that the app feeds media data into.
    pub async fn publish_to_stream(
        &self,
        request_id: &[u8; 32],
        broadcast_name: &str,
    ) -> Result<moq_lite::BroadcastProducer, ProtocolError> {
        self.check_running().await?;
        let session = self
            .stream_service
            .get_session(request_id)
            .await
            .ok_or_else(|| ProtocolError::NotFound("stream session not found".into()))?;
        session
            .create_broadcast(broadcast_name)
            .ok_or_else(|| ProtocolError::Network("failed to create broadcast".into()))
    }

    /// Consume a broadcast from an active MOQ session (destination side)
    ///
    /// Call after receiving `StreamConnected` with `is_source: false`.
    /// Returns a `BroadcastConsumer` that the app reads media data from.
    pub async fn consume_stream(
        &self,
        request_id: &[u8; 32],
        broadcast_name: &str,
    ) -> Result<moq_lite::BroadcastConsumer, ProtocolError> {
        self.check_running().await?;
        let session = self
            .stream_service
            .get_session(request_id)
            .await
            .ok_or_else(|| ProtocolError::NotFound("stream session not found".into()))?;
        session
            .consume_broadcast(broadcast_name)
            .ok_or_else(|| ProtocolError::Network("broadcast not available".into()))
    }

    // ========== Control API ==========

    /// Request a connection to a peer
    ///
    /// Sends a ConnectRequest to the peer. If a token is provided, it's for the
    /// QR code / invite string flow where the recipient auto-accepts valid tokens.
    /// Returns the request ID for tracking.
    pub async fn request_connection(
        &self,
        peer_id: &[u8; 32],
        relay_url: Option<&str>,
        display_name: Option<&str>,
        token: Option<[u8; 32]>,
    ) -> Result<[u8; 32], ProtocolError> {
        self.check_running().await?;
        self.control_service
            .request_connection(peer_id, relay_url, display_name, token)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Accept a pending connection request
    pub async fn accept_connection(&self, request_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.control_service
            .accept_connection(request_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Decline a pending connection request
    pub async fn decline_connection(
        &self,
        request_id: &[u8; 32],
        reason: Option<&str>,
    ) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.control_service
            .decline_connection(request_id, reason)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Generate a connect invite (for QR code / invite string)
    ///
    /// Returns a ConnectInvite that can be encoded to a string and shared.
    pub async fn generate_connect_invite(
        &self,
    ) -> Result<crate::network::control::ConnectInvite, ProtocolError> {
        self.check_running().await?;
        self.control_service
            .generate_connect_token()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Connect using an invite string (decode and request connection)
    ///
    /// Decodes the invite and requests connection with the embedded token.
    pub async fn connect_with_invite(&self, invite: &str) -> Result<[u8; 32], ProtocolError> {
        self.check_running().await?;
        let decoded = crate::network::control::ConnectInvite::decode(invite)
            .ok_or_else(|| ProtocolError::InvalidInput("invalid invite string".into()))?;
        self.control_service
            .request_connection(
                &decoded.endpoint_id,
                decoded.relay_url.as_deref(),
                None,
                Some(decoded.token),
            )
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Block a peer
    pub async fn block_peer(&self, peer_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.control_service
            .block_peer(peer_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Unblock a peer
    pub async fn unblock_peer(&self, peer_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.control_service
            .unblock_peer(peer_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// List all connections
    pub async fn list_connections(
        &self,
    ) -> Result<Vec<crate::data::ConnectionInfo>, ProtocolError> {
        self.check_running().await?;
        let db = self.db.lock().await;
        crate::data::list_all_connections(&db).map_err(|e| ProtocolError::Database(e.to_string()))
    }

    /// Invite a peer to a topic
    ///
    /// Returns the message ID for tracking.
    pub async fn invite_to_topic(
        &self,
        peer_id: &[u8; 32],
        topic_id: &[u8; 32],
    ) -> Result<[u8; 32], ProtocolError> {
        self.check_running().await?;
        self.control_service
            .invite_to_topic(peer_id, topic_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Accept a topic invitation
    pub async fn accept_topic_invite(&self, message_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.control_service
            .accept_topic_invite(message_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Decline a topic invitation
    pub async fn decline_topic_invite(&self, message_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.control_service
            .decline_topic_invite(message_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Remove a member from a topic (admin only)
    ///
    /// Generates a new epoch key and sends it to all remaining members.
    pub async fn remove_topic_member(
        &self,
        topic_id: &[u8; 32],
        member_id: &[u8; 32],
    ) -> Result<(), ProtocolError> {
        self.check_running().await?;
        self.control_service
            .remove_topic_member(topic_id, member_id)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Suggest a peer to another peer (introduction)
    ///
    /// Returns the message ID for tracking.
    pub async fn suggest_peer(
        &self,
        to_peer: &[u8; 32],
        suggested_peer: &[u8; 32],
        note: Option<&str>,
    ) -> Result<[u8; 32], ProtocolError> {
        self.check_running().await?;
        self.control_service
            .suggest_peer(to_peer, suggested_peer, note)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// List pending topic invites
    pub async fn list_pending_invites(
        &self,
    ) -> Result<Vec<crate::data::PendingTopicInvite>, ProtocolError> {
        self.check_running().await?;
        let db = self.db.lock().await;
        crate::data::list_pending_invites(&db).map_err(|e| ProtocolError::Database(e.to_string()))
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
