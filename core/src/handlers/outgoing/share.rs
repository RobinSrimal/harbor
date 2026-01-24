//! Share outgoing operations for the Protocol
//!
//! This module provides methods for Share protocol requests:
//! - request_chunks: Request chunks from a peer
//! - request_chunk_map: Ask source who has what
//! - announce_can_seed: Announce seeding capability

use std::time::Duration;

use iroh::{NodeAddr, NodeId};
use tracing::info;

use crate::data::CHUNK_SIZE;
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
}
