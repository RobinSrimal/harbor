//! File distribution methods for ShareService
//!
//! Handles the "push" side of file sharing:
//! - Importing files for distribution
//! - Announcing to topic members or DM recipients
//! - Initial section assignment and pushing
//! - CanSeed broadcasts

use std::path::Path;

use iroh::EndpointId;
use tracing::{debug, info, warn};

use crate::data::{
    add_blob_recipient, get_topic_members, init_blob_sections, insert_blob, mark_blob_complete,
    CHUNK_SIZE,
};
use crate::data::dht::peer::{current_timestamp, get_peer_relay_info, update_peer_relay_url};
use crate::network::connect;
use crate::network::send::SendOptions;
use crate::protocol::MemberInfo;

use super::protocol::{ChunkResponse, FileAnnouncement, InitialRecipient, ShareMessage, SHARE_ALPN};
use super::service::{ActiveTransfer, AnnouncementPlan, SectionPush, ShareError, ShareService};

/// Helper to calculate number of sections based on chunk count
fn calculate_num_sections(total_chunks: u32, max_connections: usize) -> u8 {
    max_connections.min(total_chunks as usize).max(1) as u8
}

/// Standalone helper for simpler flow
fn calc_num_sections_simple(total_chunks: u32) -> u8 {
    std::cmp::min(5, std::cmp::max(1, total_chunks)) as u8
}

impl ShareService {
    /// Import a file into the blob store and register it for sharing
    ///
    /// # Arguments
    /// * `source_path` - Path to the file to import
    /// * `scope_id` - Topic ID or recipient endpoint ID (for DMs)
    ///
    /// # Returns
    /// The file hash and initial announcement metadata (before section assignment)
    pub async fn import_file(
        &self,
        source_path: impl AsRef<Path>,
        scope_id: &[u8; 32],
    ) -> Result<([u8; 32], FileAnnouncement), ShareError> {
        let source_path = source_path.as_ref();

        // Check file exists and is large enough
        let metadata = std::fs::metadata(source_path)
            .map_err(|_| ShareError::FileNotFound(source_path.display().to_string()))?;

        if metadata.len() < CHUNK_SIZE {
            return Err(ShareError::FileTooSmall(metadata.len()));
        }

        // Import to blob store
        let (hash, total_size) = self.blob_store.import_file(source_path)?;

        let total_chunks = ((total_size + CHUNK_SIZE - 1) / CHUNK_SIZE) as u32;
        let display_name = source_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Store metadata in database
        let num_sections = calculate_num_sections(total_chunks, self.config.max_connections);

        {
            let db_lock = self.db.lock().await;
            insert_blob(
                &db_lock,
                &hash,
                scope_id,
                &self.identity.public_key,
                &display_name,
                total_size,
                num_sections,
            )
            .map_err(|e| ShareError::Database(e.to_string()))?;

            // Initialize sections
            init_blob_sections(&db_lock, &hash, num_sections, total_chunks)
                .map_err(|e| ShareError::Database(e.to_string()))?;

            // Mark ourselves as complete
            mark_blob_complete(&db_lock, &hash)
                .map_err(|e| ShareError::Database(e.to_string()))?;
        }

        // Create announcement (initial_recipients will be filled when connections are made)
        let announcement = FileAnnouncement {
            hash,
            source_id: self.identity.public_key,
            total_size,
            total_chunks,
            num_sections,
            display_name,
            merkle_root: hash, // For now, use the file hash as merkle root
            initial_recipients: Vec::new(), // Will be updated after connections
        };

        info!(
            hash = %hex::encode(&hash[..8]),
            size = total_size,
            chunks = total_chunks,
            sections = num_sections,
            "file imported for sharing"
        );

        Ok((hash, announcement))
    }

    /// Start initial distribution to peers
    ///
    /// Connects to up to max_connections peers and assigns sections.
    /// Returns updated FileAnnouncement with initial_recipients.
    pub async fn start_distribution(
        &self,
        hash: &[u8; 32],
        announcement: &mut FileAnnouncement,
        peers: &[[u8; 32]],
    ) -> Result<(), ShareError> {
        let num_peers = peers.len().min(self.config.max_connections);
        if num_peers == 0 {
            return Ok(());
        }

        let total_chunks = announcement.total_chunks;
        let num_sections = num_peers as u8;
        let chunks_per_section = (total_chunks + num_sections as u32 - 1) / num_sections as u32;

        let mut initial_recipients = Vec::new();

        for (i, peer_id) in peers.iter().take(num_peers).enumerate() {
            let section_id = i as u8;
            let chunk_start = section_id as u32 * chunks_per_section;
            let chunk_end = ((section_id as u32 + 1) * chunks_per_section).min(total_chunks);

            // Try to connect to peer
            let node_id = EndpointId::from_bytes(peer_id)
                .map_err(|e| ShareError::Connection(e.to_string()))?;

            match self.connect_for_share(node_id).await {
                Ok(conn) => {
                    // Add to initial recipients
                    initial_recipients.push(InitialRecipient {
                        endpoint_id: *peer_id,
                        section_id,
                        chunk_start,
                        chunk_end,
                    });

                    // Track recipient in database
                    {
                        let db_lock = self.db.lock().await;
                        add_blob_recipient(&db_lock, hash, peer_id)
                            .map_err(|e| ShareError::Database(e.to_string()))?;
                    }

                    // Store active transfer
                    {
                        let mut transfers = self.outgoing_transfers.write().await;
                        let entry = transfers.entry(*hash).or_insert_with(Vec::new);
                        entry.push(ActiveTransfer {
                            peer_id: *peer_id,
                            connection: conn,
                            chunks_in_progress: (chunk_start..chunk_end).collect(),
                        });
                    }

                    debug!(
                        peer = %hex::encode(&peer_id[..8]),
                        section = section_id,
                        chunks = ?chunk_start..chunk_end,
                        "assigned section to peer"
                    );
                }
                Err(e) => {
                    warn!(
                        peer = %hex::encode(&peer_id[..8]),
                        error = %e,
                        "failed to connect to peer for distribution"
                    );
                }
            }
        }

        announcement.initial_recipients = initial_recipients;
        announcement.num_sections = num_sections;

        Ok(())
    }

    /// Send chunks to a peer over an existing connection
    pub async fn send_chunks(
        &self,
        hash: &[u8; 32],
        peer_id: &[u8; 32],
        chunk_indices: &[u32],
        conn: &iroh::endpoint::Connection,
    ) -> Result<(), ShareError> {
        use super::protocol::{ChunkResponse, ShareMessage};
        use tracing::trace;

        for &chunk_index in chunk_indices {
            // Read chunk from storage
            let chunk_data = self.blob_store.read_chunk(hash, chunk_index)?;

            // Create response
            let response = ShareMessage::ChunkResponse(ChunkResponse {
                hash: *hash,
                chunk_index,
                data: chunk_data,
            });

            // Send via stream
            let mut send_stream = conn
                .open_uni()
                .await
                .map_err(|e| ShareError::Connection(e.to_string()))?;

            tokio::io::AsyncWriteExt::write_all(&mut send_stream, &response.encode())
                .await
                .map_err(|e| ShareError::Connection(e.to_string()))?;

            send_stream
                .finish()
                .map_err(|e| ShareError::Connection(e.to_string()))?;

            trace!(
                hash = %hex::encode(&hash[..8]),
                chunk = chunk_index,
                peer = %hex::encode(&peer_id[..8]),
                "sent chunk"
            );
        }

        Ok(())
    }

    /// Push a section of chunks to a specific peer
    ///
    /// Opens a connection and pushes all chunks in the section range.
    pub async fn push_section_to_peer(
        &self,
        hash: &[u8; 32],
        peer_id: &[u8; 32],
        chunk_start: u32,
        chunk_end: u32,
    ) -> Result<(), ShareError> {
        let node_id =
            EndpointId::from_bytes(peer_id).map_err(|e| ShareError::Connection(e.to_string()))?;

        let conn = self.connect_for_share(node_id).await?;

        let chunks: Vec<u32> = (chunk_start..chunk_end).collect();
        self.send_chunks(hash, peer_id, &chunks, &conn).await?;

        info!(
            hash = %hex::encode(&hash[..8]),
            peer = %hex::encode(&peer_id[..8]),
            chunks = ?chunk_start..chunk_end,
            "pushed section to peer"
        );

        Ok(())
    }

    /// Get recipient IDs for a can-seed announcement
    ///
    /// Returns the endpoint IDs of all topic members to broadcast the announcement to.
    pub async fn get_can_seed_recipients(
        &self,
        topic_id: &[u8; 32],
    ) -> Result<Vec<MemberInfo>, ShareError> {
        let db_lock = self.db.lock().await;
        let members = get_topic_members(&db_lock, topic_id)
            .map_err(|e| ShareError::Database(e.to_string()))?;

        Ok(members
            .into_iter()
            .map(|endpoint_id| MemberInfo::new(endpoint_id))
            .collect())
    }

    /// Announce ability to seed a file (topic only)
    ///
    /// Broadcasts a CanSeed message to all topic members, letting them know
    /// this peer has a complete copy and can serve chunks.
    pub async fn announce_can_seed(
        &self,
        topic_id: &[u8; 32],
        hash: &[u8; 32],
        recipients: &[MemberInfo],
    ) -> Result<(), ShareError> {
        use crate::network::packet::{CanSeedMessage, TopicMessage};

        let send_service = self
            .send_service
            .read()
            .await
            .as_ref()
            .ok_or_else(|| ShareError::Protocol("SendService not initialized".to_string()))?
            .clone();

        let can_seed_msg = TopicMessage::CanSeed(CanSeedMessage::new(*hash, self.identity.public_key));

        send_service
            .send_to_topic(topic_id, &can_seed_msg.encode(), recipients, SendOptions::content())
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        info!(
            hash = %hex::encode(&hash[..8]),
            topic = %hex::encode(&topic_id[..8]),
            "announced can seed to topic"
        );

        Ok(())
    }

    /// Import and register a file for sharing (simpler version)
    ///
    /// Used internally by share_and_announce and share_dm_file.
    pub async fn share_file(
        &self,
        file_path: &Path,
        scope_id: &[u8; 32],
    ) -> Result<([u8; 32], u64, u32, u8, String), ShareError> {
        // Check file exists and is large enough
        let file_metadata = std::fs::metadata(file_path)
            .map_err(|_| ShareError::FileNotFound(file_path.display().to_string()))?;

        if file_metadata.len() < CHUNK_SIZE {
            return Err(ShareError::FileTooSmall(file_metadata.len()));
        }

        info!(
            file = %file_path.display(),
            size = file_metadata.len(),
            scope = hex::encode(&scope_id[..8]),
            "Starting file share"
        );

        // Import the file
        let (hash, size) = self.blob_store.import_file(file_path)?;

        let total_chunks = ((size + CHUNK_SIZE - 1) / CHUNK_SIZE) as u32;
        let num_sections = calc_num_sections_simple(total_chunks);

        let display_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string();

        // Store metadata in database
        {
            let db_lock = self.db.lock().await;
            insert_blob(
                &db_lock,
                &hash,
                scope_id,
                &self.identity.public_key,
                &display_name,
                size,
                num_sections,
            )
            .map_err(|e| ShareError::Database(e.to_string()))?;

            // Mark as complete since we have the full file
            mark_blob_complete(&db_lock, &hash)
                .map_err(|e| ShareError::Database(e.to_string()))?;
        }

        info!(
            hash = hex::encode(&hash[..8]),
            size = size,
            chunks = total_chunks,
            sections = num_sections,
            "File imported for sharing"
        );

        Ok((hash, size, total_chunks, num_sections, display_name))
    }

    /// Share a file with topic members: import, announce, and push sections
    ///
    /// This is the full share flow for topics. Protocol delegates here.
    pub async fn share_and_announce(
        &self,
        topic_id: &[u8; 32],
        file_path: &Path,
    ) -> Result<[u8; 32], ShareError> {
        use crate::network::send::SendOptions;

        // 1. Import file and store metadata
        let (hash, total_size, total_chunks, num_sections, display_name) =
            self.share_file(file_path, topic_id).await?;

        info!(
            hash = hex::encode(&hash[..8]),
            size = total_size,
            chunks = total_chunks,
            sections = num_sections,
            "File imported, announcing to topic"
        );

        // 2. Prepare announcement plan
        let plan = self
            .prepare_announcement(
                topic_id,
                &hash,
                total_size,
                total_chunks,
                num_sections,
                &display_name,
            )
            .await?;

        // 3. Broadcast announcement via SendService
        let send_service = self
            .send_service
            .read()
            .await
            .as_ref()
            .ok_or_else(|| ShareError::Protocol("send service not available".to_string()))?
            .clone();

        send_service
            .send_to_topic(
                topic_id,
                &plan.message_bytes,
                &plan.recipients,
                SendOptions::content(),
            )
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        info!(
            hash = hex::encode(&hash[..8]),
            members = plan.recipients.len(),
            "File announced to topic"
        );

        // 4. Spawn section pushes to initial recipients
        for push in plan.section_pushes {
            let service = self.blob_store.clone();
            let endpoint = self.endpoint.clone();
            let db = self.db.clone();
            let hash_copy = hash;

            tokio::spawn(async move {
                let node_id = match EndpointId::from_bytes(&push.peer_id) {
                    Ok(id) => id,
                    Err(e) => {
                        warn!(error = %e, "Invalid peer ID for section push");
                        return;
                    }
                };

                let relay_info = {
                    let db = db.lock().await;
                    get_peer_relay_info(&db, &push.peer_id).unwrap_or(None)
                };
                let (relay_url_str, relay_last_success) = match &relay_info {
                    Some((url, ts)) => (Some(url.clone()), *ts),
                    None => (None, None),
                };
                let parsed_relay: Option<iroh::RelayUrl> =
                    relay_url_str.as_deref().and_then(|u| u.parse().ok());

                let result = match connect::connect(
                    &endpoint,
                    node_id,
                    parsed_relay.as_ref(),
                    relay_last_success,
                    SHARE_ALPN,
                    std::time::Duration::from_secs(15),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        warn!(
                            peer = hex::encode(&push.peer_id[..8]),
                            section = push.section_id,
                            "Failed to connect for section push"
                        );
                        return;
                    }
                };

                let conn = result.connection;
                if result.relay_url_confirmed {
                    if let Some(url) = &parsed_relay {
                        let db = db.lock().await;
                        let _ = update_peer_relay_url(
                            &db,
                            &push.peer_id,
                            &url.to_string(),
                            current_timestamp(),
                        );
                    }
                }

                for chunk_index in push.chunk_start..push.chunk_end {
                    let data = match service.read_chunk(&hash_copy, chunk_index) {
                        Ok(d) => d,
                        Err(e) => {
                            warn!(error = %e, chunk = chunk_index, "Failed to read chunk for push");
                            return;
                        }
                    };

                    let msg = ShareMessage::ChunkResponse(ChunkResponse {
                        hash: hash_copy,
                        chunk_index,
                        data,
                    });

                    let mut send = match conn.open_uni().await {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(error = %e, "Failed to open stream for push");
                            return;
                        }
                    };

                    if let Err(e) =
                        tokio::io::AsyncWriteExt::write_all(&mut send, &msg.encode()).await
                    {
                        warn!(error = %e, "Failed to write chunk for push");
                        return;
                    }
                    let _ = send.finish();
                }

                info!(
                    peer = hex::encode(&push.peer_id[..8]),
                    hash = hex::encode(&hash_copy[..8]),
                    chunks = push.chunk_end - push.chunk_start,
                    "Section push complete"
                );
            });
        }

        Ok(hash)
    }

    /// Share a file via DM (direct message to a single peer)
    ///
    /// Imports the file and sends a FileAnnouncement via the DM channel.
    /// The sender is the only seeder — no CanSeed announcement needed.
    pub async fn share_dm_file(
        &self,
        recipient_id: &[u8; 32],
        file_path: &Path,
    ) -> Result<[u8; 32], ShareError> {
        use crate::network::packet::{DmMessage, FileAnnouncementMessage};

        // 1. Import file (use recipient_id as scope)
        let (hash, total_size, total_chunks, num_sections, display_name) =
            self.share_file(file_path, recipient_id).await?;

        info!(
            hash = hex::encode(&hash[..8]),
            size = total_size,
            recipient = hex::encode(&recipient_id[..8]),
            "File imported, announcing via DM"
        );

        // 2. Encode DM FileAnnouncement and send
        let dm_msg = DmMessage::FileAnnouncement(FileAnnouncementMessage::new(
            hash,
            self.identity.public_key,
            total_size,
            total_chunks,
            num_sections,
            display_name,
        ));

        let send_service = self
            .send_service
            .read()
            .await
            .as_ref()
            .ok_or_else(|| ShareError::Protocol("send service not available".to_string()))?
            .clone();

        send_service
            .send_to_dm(recipient_id, &dm_msg.encode())
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        info!(
            hash = hex::encode(&hash[..8]),
            recipient = hex::encode(&recipient_id[..8]),
            "DM file announcement sent"
        );

        Ok(hash)
    }

    /// Prepare an announcement plan for a topic
    ///
    /// Generates the message bytes and calculates section assignments for initial pushes.
    pub async fn prepare_announcement(
        &self,
        topic_id: &[u8; 32],
        hash: &[u8; 32],
        total_size: u64,
        total_chunks: u32,
        num_sections: u8,
        display_name: &str,
    ) -> Result<AnnouncementPlan, ShareError> {
        use crate::network::packet::{FileAnnouncementMessage, TopicMessage};

        // Get topic members
        let members = {
            let db_lock = self.db.lock().await;
            get_topic_members(&db_lock, topic_id)
                .map_err(|e| ShareError::Database(e.to_string()))?
        };

        // Filter out self
        let other_members: Vec<_> = members
            .iter()
            .filter(|m| **m != self.identity.public_key)
            .collect();

        // Create the FileAnnouncement message
        let topic_msg = TopicMessage::FileAnnouncement(FileAnnouncementMessage::new(
            *hash,
            self.identity.public_key,
            total_size,
            total_chunks,
            num_sections,
            display_name.to_string(),
        ));

        let message_bytes = topic_msg.encode();

        // Convert to MemberInfo
        let recipients: Vec<MemberInfo> = members
            .iter()
            .map(|endpoint_id| MemberInfo::new(*endpoint_id))
            .collect();

        // Calculate section pushes (up to 5 initial recipients)
        let mut section_pushes = Vec::new();
        let initial_count = std::cmp::min(5, other_members.len());
        let chunks_per_section = (total_chunks + num_sections as u32 - 1) / num_sections as u32;

        for (i, member) in other_members.iter().take(initial_count).enumerate() {
            let section_id = i as u8 % num_sections;
            let chunk_start = section_id as u32 * chunks_per_section;
            let chunk_end = std::cmp::min(chunk_start + chunks_per_section, total_chunks);

            section_pushes.push(SectionPush {
                peer_id: **member,
                section_id,
                chunk_start,
                chunk_end,
            });
        }

        if other_members.is_empty() {
            warn!("No other members in topic to share with");
        }

        info!(
            hash = hex::encode(&hash[..8]),
            members = other_members.len(),
            pushes = section_pushes.len(),
            "Prepared announcement plan"
        );

        Ok(AnnouncementPlan {
            message_bytes,
            recipients,
            section_pushes,
        })
    }
}
