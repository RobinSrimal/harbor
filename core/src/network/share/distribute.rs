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
    CHUNK_SIZE, add_blob_recipient, get_topic_members, init_blob_sections, insert_blob,
    mark_blob_complete,
};
use crate::network::send::SendOptions;
use crate::protocol::MemberInfo;

use super::protocol::{ChunkResponse, FileAnnouncement, InitialRecipient, ShareMessage};
use super::service::{AnnouncementPlan, SectionPush, ShareError, ShareService};

/// Helper to calculate number of sections based on chunk count
fn calculate_num_sections(total_chunks: u32, max_connections: usize) -> u8 {
    max_connections.min(total_chunks as usize).max(1) as u8
}

fn total_chunks_from_size(total_size: u64) -> Result<u32, ShareError> {
    let total_chunks_u64 = total_size.div_ceil(CHUNK_SIZE);
    u32::try_from(total_chunks_u64)
        .map_err(|_| ShareError::Protocol("file too large for share chunk index space".to_string()))
}

fn section_bounds(total_chunks: u32, num_sections: u8, section_id: u8) -> Option<(u32, u32)> {
    if num_sections == 0 || section_id >= num_sections {
        return None;
    }

    let chunks_per_section = total_chunks.div_ceil(num_sections as u32);
    let chunk_start = section_id as u32 * chunks_per_section;
    let chunk_end = ((section_id as u32 + 1) * chunks_per_section).min(total_chunks);
    Some((chunk_start, chunk_end))
}

fn persist_shared_blob_metadata(
    conn: &rusqlite::Connection,
    hash: &[u8; 32],
    scope_id: &[u8; 32],
    source_id: &[u8; 32],
    display_name: &str,
    total_size: u64,
    num_sections: u8,
) -> Result<u32, ShareError> {
    if num_sections == 0 {
        return Err(ShareError::Protocol("num_sections must be > 0".to_string()));
    }

    let total_chunks = total_chunks_from_size(total_size)?;

    insert_blob(
        conn,
        hash,
        scope_id,
        source_id,
        display_name,
        total_size,
        num_sections,
    )
    .map_err(|e| ShareError::Database(e.to_string()))?;

    init_blob_sections(conn, hash, num_sections, total_chunks)
        .map_err(|e| ShareError::Database(e.to_string()))?;

    mark_blob_complete(conn, hash).map_err(|e| ShareError::Database(e.to_string()))?;

    Ok(total_chunks)
}

#[derive(Debug, Clone)]
struct ImportedBlobInfo {
    hash: [u8; 32],
    total_size: u64,
    total_chunks: u32,
    num_sections: u8,
    display_name: String,
}

impl ShareService {
    async fn import_and_register_file(
        &self,
        file_path: &Path,
        scope_id: &[u8; 32],
    ) -> Result<ImportedBlobInfo, ShareError> {
        let file_metadata = std::fs::metadata(file_path)
            .map_err(|_| ShareError::FileNotFound(file_path.display().to_string()))?;

        if file_metadata.len() < CHUNK_SIZE {
            return Err(ShareError::FileTooSmall(file_metadata.len()));
        }

        let (hash, total_size) = self.blob_store.import_file(file_path)?;
        let num_sections = calculate_num_sections(
            total_chunks_from_size(total_size)?,
            self.config.max_connections,
        );
        let display_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let total_chunks = {
            let db_lock = self.db.lock().await;
            persist_shared_blob_metadata(
                &db_lock,
                &hash,
                scope_id,
                &self.identity.public_key,
                &display_name,
                total_size,
                num_sections,
            )?
        };

        Ok(ImportedBlobInfo {
            hash,
            total_size,
            total_chunks,
            num_sections,
            display_name,
        })
    }

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
        let imported = self.import_and_register_file(source_path, scope_id).await?;

        // Create announcement (initial_recipients will be filled when connections are made)
        let announcement = FileAnnouncement {
            hash: imported.hash,
            source_id: self.identity.public_key,
            total_size: imported.total_size,
            total_chunks: imported.total_chunks,
            num_sections: imported.num_sections,
            display_name: imported.display_name,
            merkle_root: imported.hash, // For now, use the file hash as merkle root
            initial_recipients: Vec::new(), // Will be updated after connections
        };

        info!(
            hash = %hex::encode(&imported.hash[..8]),
            size = imported.total_size,
            chunks = imported.total_chunks,
            sections = imported.num_sections,
            "file imported for sharing"
        );

        Ok((imported.hash, announcement))
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
        let num_sections = announcement.num_sections.max(1);

        let mut initial_recipients = Vec::new();

        for (i, peer_id) in peers.iter().take(num_peers).enumerate() {
            let section_id = (i % usize::from(num_sections)) as u8;
            let Some((chunk_start, chunk_end)) =
                section_bounds(total_chunks, num_sections, section_id)
            else {
                continue;
            };

            // Try to connect to peer
            let node_id = EndpointId::from_bytes(peer_id)
                .map_err(|e| ShareError::Connection(e.to_string()))?;

            match self.connect_for_share(node_id).await {
                Ok(_) => {
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

        let can_seed_msg =
            TopicMessage::CanSeed(CanSeedMessage::new(*hash, self.identity.public_key));

        send_service
            .send_to_topic(
                topic_id,
                &can_seed_msg.encode(),
                recipients,
                SendOptions::content(),
            )
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))?;

        info!(
            hash = %hex::encode(&hash[..8]),
            topic = %hex::encode(&topic_id[..8]),
            "announced can seed to topic"
        );

        Ok(())
    }

    /// Import and register a file for sharing
    ///
    /// Used internally by share_and_announce and share_dm_file.
    pub async fn share_file(
        &self,
        file_path: &Path,
        scope_id: &[u8; 32],
    ) -> Result<([u8; 32], u64, u32, u8, String), ShareError> {
        let imported = self.import_and_register_file(file_path, scope_id).await?;

        info!(
            file = %file_path.display(),
            size = imported.total_size,
            scope = hex::encode(&scope_id[..8]),
            "Starting file share"
        );

        info!(
            hash = hex::encode(&imported.hash[..8]),
            size = imported.total_size,
            chunks = imported.total_chunks,
            sections = imported.num_sections,
            "File imported for sharing"
        );

        Ok((
            imported.hash,
            imported.total_size,
            imported.total_chunks,
            imported.num_sections,
            imported.display_name,
        ))
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
            let connector = self.connector.clone();
            let hash_copy = hash;
            let connect_timeout = self.config.connect_timeout;

            tokio::spawn(async move {
                let conn = match connector
                    .connect_with_timeout(&push.peer_id, connect_timeout)
                    .await
                {
                    Ok(conn) => conn,
                    Err(e) => {
                        warn!(
                            peer = hex::encode(&push.peer_id[..8]),
                            section = push.section_id,
                            error = %e,
                            "Failed to connect for section push"
                        );
                        return;
                    }
                };

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
                    if let Err(e) = send.finish() {
                        warn!(error = %e, "Failed to finish stream for push");
                        return;
                    }
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

        if num_sections == 0 {
            return Err(ShareError::Protocol("num_sections must be > 0".to_string()));
        }

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

        // Calculate section pushes (up to max_connections initial recipients)
        let mut section_pushes = Vec::new();
        let initial_count = other_members
            .len()
            .min(self.config.max_connections)
            .min(num_sections as usize);

        for (i, member) in other_members.iter().take(initial_count).enumerate() {
            let section_id = i as u8;
            let Some((chunk_start, chunk_end)) =
                section_bounds(total_chunks, num_sections, section_id)
            else {
                continue;
            };

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

#[cfg(test)]
mod tests {
    use super::*;

    use rusqlite::Connection;

    use crate::data::{BlobState, create_all_tables, get_blob, get_section_traces};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB should open");
        conn.execute("PRAGMA foreign_keys = ON", [])
            .expect("foreign keys should be enabled");
        create_all_tables(&conn).expect("schema should initialize");
        conn
    }

    #[test]
    fn test_calculate_num_sections_respects_max_connections() {
        assert_eq!(calculate_num_sections(10, 5), 5);
        assert_eq!(calculate_num_sections(3, 5), 3);
        assert_eq!(calculate_num_sections(10, 0), 1);
        assert_eq!(calculate_num_sections(0, 5), 1);
    }

    #[test]
    fn test_section_bounds_split_ranges() {
        assert_eq!(section_bounds(10, 3, 0), Some((0, 4)));
        assert_eq!(section_bounds(10, 3, 1), Some((4, 8)));
        assert_eq!(section_bounds(10, 3, 2), Some((8, 10)));
        assert_eq!(section_bounds(10, 3, 3), None);
        assert_eq!(section_bounds(10, 0, 0), None);
    }

    #[test]
    fn test_total_chunks_from_size_rejects_overflow() {
        let too_large = (u64::from(u32::MAX) + 1) * CHUNK_SIZE;
        let err = total_chunks_from_size(too_large).expect_err("overflow size should fail");
        assert!(matches!(err, ShareError::Protocol(_)));
    }

    #[test]
    fn test_persist_shared_blob_metadata_inits_sections_and_marks_complete() {
        let conn = setup_db();
        let hash = [11u8; 32];
        let scope_id = [1u8; 32];
        let source_id = [2u8; 32];
        let total_size = CHUNK_SIZE * 3;

        let total_chunks = persist_shared_blob_metadata(
            &conn, &hash, &scope_id, &source_id, "test.bin", total_size, 3,
        )
        .expect("blob metadata should persist");

        assert_eq!(total_chunks, 3);

        let blob = get_blob(&conn, &hash)
            .expect("blob lookup should succeed")
            .expect("blob should exist");
        assert_eq!(blob.state, BlobState::Complete);
        assert_eq!(blob.num_sections, 3);

        let traces = get_section_traces(&conn, &hash).expect("section traces should load");
        assert_eq!(traces.len(), 3);
        assert_eq!(traces[0].chunk_start, 0);
        assert_eq!(traces[0].chunk_end, 1);
        assert_eq!(traces[1].chunk_start, 1);
        assert_eq!(traces[1].chunk_end, 2);
        assert_eq!(traces[2].chunk_start, 2);
        assert_eq!(traces[2].chunk_end, 3);
    }
}
