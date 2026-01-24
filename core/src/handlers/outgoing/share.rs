//! Share outgoing operations for the Protocol
//!
//! This module provides methods for Share protocol requests:
//! - request_chunks: Request chunks from a peer
//! - request_chunk_map: Ask source who has what
//! - announce_can_seed: Announce seeding capability
//! - push_section_to_peer: Push a section of chunks to a peer

use std::time::Duration;

use iroh::{NodeAddr, NodeId};
use tracing::{debug, info};

use crate::data::{BlobStore, CHUNK_SIZE};
use crate::network::share::protocol::{
    ChunkMapRequest, ChunkMapResponse, ChunkRequest, ChunkResponse,
    ShareMessage, SHARE_ALPN,
};
use crate::network::send::topic_messages::{TopicMessage, CanSeedMessage};
use crate::protocol::{Protocol, ProtocolError};

impl Protocol {
    /// Request chunks from a peer
    pub async fn request_chunks(
        &self,
        peer_id: &[u8; 32],
        hash: &[u8; 32],
        chunk_indices: &[u32],
    ) -> Result<Vec<ChunkResponse>, ProtocolError> {
        let node_id = NodeId::from_bytes(peer_id)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let conn = self.connect_for_share(node_id).await?;

        let request = ShareMessage::ChunkRequest(ChunkRequest {
            hash: *hash,
            chunks: chunk_indices.to_vec(),
        });

        // Open bidirectional stream
        let (mut send, mut recv) = conn.open_bi().await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Send request
        tokio::io::AsyncWriteExt::write_all(&mut send, &request.encode()).await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Read responses
        let mut responses = Vec::new();
        let max_size = CHUNK_SIZE as usize + 1024;

        loop {
            match recv.read_to_end(max_size).await {
                Ok(data) if !data.is_empty() => {
                    if let Ok(ShareMessage::ChunkResponse(resp)) = ShareMessage::decode(&data) {
                        responses.push(resp);
                    }
                }
                _ => break,
            }
        }

        Ok(responses)
    }

    /// Request chunk map from source
    pub async fn request_chunk_map(
        &self,
        source_id: &[u8; 32],
        hash: &[u8; 32],
    ) -> Result<ChunkMapResponse, ProtocolError> {
        let node_id = NodeId::from_bytes(source_id)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let conn = self.connect_for_share(node_id).await?;

        let request = ShareMessage::ChunkMapRequest(ChunkMapRequest {
            hash: *hash,
        });

        // Open bidirectional stream
        let (mut send, mut recv) = conn.open_bi().await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Send request
        tokio::io::AsyncWriteExt::write_all(&mut send, &request.encode()).await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Read response
        let data = recv.read_to_end(64 * 1024).await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        match ShareMessage::decode(&data) {
            Ok(ShareMessage::ChunkMapResponse(resp)) => Ok(resp),
            _ => Err(ProtocolError::Network("invalid response".to_string())),
        }
    }

    /// Announce that we can seed (have 100% of file)
    ///
    /// This uses the Send protocol to broadcast to topic members.
    pub async fn announce_can_seed(
        &self,
        topic_id: &[u8; 32],
        hash: &[u8; 32],
        recipient_ids: &[[u8; 32]],
    ) -> Result<(), ProtocolError> {
        let topic_msg = TopicMessage::CanSeed(CanSeedMessage::new(
            *hash,
            self.identity.public_key,
        ));

        self.send_raw(
            topic_id,
            &topic_msg.encode(),
            recipient_ids,
            crate::network::harbor::protocol::HarborPacketType::Content,
        ).await?;

        info!(
            hash = %hex::encode(&hash[..8]),
            "announced can seed"
        );

        Ok(())
    }

    /// Connect to a peer for Share protocol
    async fn connect_for_share(&self, node_id: NodeId) -> Result<iroh::endpoint::Connection, ProtocolError> {
        let node_addr: NodeAddr = node_id.into();

        tokio::time::timeout(
            Duration::from_secs(10),
            self.endpoint.connect(node_addr, SHARE_ALPN),
        )
        .await
        .map_err(|_| ProtocolError::Network("connect timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Push a section of chunks to a peer
    ///
    /// Opens a connection and sends all chunks in the range [chunk_start, chunk_end).
    pub async fn push_section_to_peer(
        &self,
        peer_id: &[u8; 32],
        hash: &[u8; 32],
        chunk_start: u32,
        chunk_end: u32,
    ) -> Result<(), ProtocolError> {
        let node_id = NodeId::from_bytes(peer_id)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        let node_addr: NodeAddr = node_id.into();

        // Connect to peer
        let conn = tokio::time::timeout(
            Duration::from_secs(15),
            self.endpoint.connect(node_addr, SHARE_ALPN),
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

        let blob_store = self.share_service.blob_store();

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

            tracing::debug!(
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
}

/// Push a section of chunks to a peer (standalone version for spawned tasks)
///
/// This function doesn't require a Protocol reference, making it suitable for
/// use in spawned tasks where we can't easily pass `&self`.
pub async fn push_section_to_peer_standalone(
    endpoint: &iroh::Endpoint,
    blob_store: &BlobStore,
    peer_id: &[u8; 32],
    hash: &[u8; 32],
    chunk_start: u32,
    chunk_end: u32,
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
