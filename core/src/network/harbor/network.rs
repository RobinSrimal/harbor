//! Harbor network operations
//!
//! Handles outgoing QUIC connections to Harbor Nodes:
//! - Store requests (replicating packets)
//! - Pull requests (retrieving missed packets)
//! - Acknowledgments (fire-and-forget)
//! - Sync requests (Harbor-to-Harbor)
//!
//! All send methods use a shared connection helper to avoid duplicating
//! relay lookup, connection establishment, and relay URL confirmation.

use tracing::debug;

use crate::protocol::ProtocolError;

use super::protocol::{
    DeliveryAck, HarborMessage, PacketInfo, StoreRequest, PullRequest, SyncRequest, SyncResponse,
};
use super::service::HarborService;

impl HarborService {
    /// Maximum buffer size for store responses (small, just status)
    pub const STORE_RESPONSE_MAX_SIZE: usize = 1024;

    /// Maximum buffer size for pull responses (can contain many packets)
    pub const PULL_RESPONSE_MAX_SIZE: usize = 10 * 1024 * 1024; // 10MB

    /// Maximum buffer size for sync responses
    pub const SYNC_RESPONSE_MAX_SIZE: usize = 1024 * 1024; // 1MB

    /// Connect to a harbor node, handling relay lookup and URL confirmation.
    ///
    /// This is the shared connection helper used by all `send_harbor_*` methods.
    /// It handles:
    /// 1. Relay info lookup from DB
    /// 2. QUIC connection via Connector
    /// 3. Relay URL confirmation update
    async fn connect_to_harbor_node(
        &self,
        harbor_node: &[u8; 32],
    ) -> Result<iroh::endpoint::Connection, ProtocolError> {
        let connector = self.connector();
        connector
            .connect_with_timeout(harbor_node, self.connect_timeout)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Send a StoreRequest to a Harbor Node
    pub async fn send_harbor_store(
        &self,
        harbor_node: &[u8; 32],
        request: &StoreRequest,
    ) -> Result<bool, ProtocolError> {
        let conn = self.connect_to_harbor_node(harbor_node).await?;

        let message = HarborMessage::Store(request.clone());
        let encoded = message.encode();

        // Send request
        let mut send = conn
            .open_uni()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        tokio::io::AsyncWriteExt::write_all(&mut send, &encoded)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Wait for response with timeout
        let mut recv = tokio::time::timeout(self.response_timeout, conn.accept_uni())
            .await
            .map_err(|_| ProtocolError::Network("response timeout".to_string()))?
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let response_bytes = recv
            .read_to_end(Self::STORE_RESPONSE_MAX_SIZE)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let response = HarborMessage::decode(&response_bytes)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        match response {
            HarborMessage::StoreResponse(resp) => Ok(resp.success),
            _ => Err(ProtocolError::Network("unexpected response".to_string())),
        }
    }

    /// Send a PullRequest to a Harbor Node
    pub async fn send_harbor_pull(
        &self,
        harbor_node: &[u8; 32],
        request: &PullRequest,
    ) -> Result<Vec<PacketInfo>, ProtocolError> {
        let conn = self.connect_to_harbor_node(harbor_node).await?;

        let message = HarborMessage::Pull(request.clone());
        let encoded = message.encode();

        // Send request
        let mut send = conn
            .open_uni()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        tokio::io::AsyncWriteExt::write_all(&mut send, &encoded)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Wait for response with timeout (larger buffer for packets)
        let mut recv = tokio::time::timeout(self.response_timeout, conn.accept_uni())
            .await
            .map_err(|_| ProtocolError::Network("response timeout".to_string()))?
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let response_bytes = recv
            .read_to_end(Self::PULL_RESPONSE_MAX_SIZE)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let response = HarborMessage::decode(&response_bytes)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        match response {
            HarborMessage::PullResponse(resp) => Ok(resp.packets),
            _ => Err(ProtocolError::Network("unexpected response".to_string())),
        }
    }

    /// Send a DeliveryAck to a Harbor Node (fire-and-forget)
    pub async fn send_harbor_ack(
        &self,
        harbor_node: &[u8; 32],
        ack: &DeliveryAck,
    ) -> Result<(), ProtocolError> {
        let conn = self.connect_to_harbor_node(harbor_node).await?;

        let message = HarborMessage::Ack(ack.clone());
        let encoded = message.encode();

        // Send ack (no response expected)
        let mut send = conn
            .open_uni()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        tokio::io::AsyncWriteExt::write_all(&mut send, &encoded)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        Ok(())
    }

    /// Send a SyncRequest to another Harbor Node
    pub async fn send_harbor_sync(
        &self,
        harbor_node: &[u8; 32],
        request: &SyncRequest,
    ) -> Result<SyncResponse, ProtocolError> {
        let node_hex = hex::encode(harbor_node);
        debug!(partner = %node_hex, "Harbor sync: connecting to partner");

        let conn = self.connect_to_harbor_node(harbor_node).await.map_err(|e| {
            debug!(partner = %node_hex, error = %e, "Harbor sync: connection failed");
            e
        })?;

        debug!(partner = %node_hex, "Harbor sync: connection established, opening stream");

        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| {
            debug!(partner = %node_hex, error = %e, "Harbor sync: failed to open stream");
            ProtocolError::Network(e.to_string())
        })?;

        // Send request
        let msg = HarborMessage::SyncRequest(request.clone());
        let data =
            postcard::to_allocvec(&msg).map_err(|e| ProtocolError::Network(e.to_string()))?;

        debug!(
            partner = %node_hex,
            request_size = data.len(),
            packets_we_have = request.have_packets.len(),
            "Harbor sync: sending request"
        );

        send.write_all(&data)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Read response
        let response_data = recv
            .read_to_end(Self::SYNC_RESPONSE_MAX_SIZE)
            .await
            .map_err(|e| {
                debug!(partner = %node_hex, error = %e, "Harbor sync: failed to read response");
                ProtocolError::Network(e.to_string())
            })?;

        debug!(
            partner = %node_hex,
            response_size = response_data.len(),
            "Harbor sync: received response"
        );

        let response: HarborMessage = postcard::from_bytes(&response_data)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        match response {
            HarborMessage::SyncResponse(resp) => {
                debug!(
                    partner = %node_hex,
                    missing_packets = resp.missing_packets.len(),
                    delivery_updates = resp.delivery_updates.len(),
                    "Harbor sync: parsed response"
                );
                Ok(resp)
            }
            other => {
                debug!(partner = %node_hex, "Harbor sync: unexpected response type");
                Err(ProtocolError::Network(format!(
                    "unexpected response: {:?}",
                    std::mem::discriminant(&other)
                )))
            }
        }
    }
}
