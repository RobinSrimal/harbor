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
use crate::security::PacketId;

use super::protocol::{
    DeliveryAck, HarborMessage, PacketInfo, PullRequest, StoreRequest, SyncRequest, SyncResponse,
};
use super::service::HarborService;

fn unexpected_response(expected: &str, actual: &HarborMessage) -> ProtocolError {
    ProtocolError::Network(format!(
        "unexpected response (expected {}): {:?}",
        expected,
        std::mem::discriminant(actual)
    ))
}

fn parse_store_response(
    request_packet_id: &PacketId,
    response: HarborMessage,
) -> Result<bool, ProtocolError> {
    match response {
        HarborMessage::StoreResponse(resp) => {
            if &resp.packet_id != request_packet_id {
                return Err(ProtocolError::Network(format!(
                    "store response packet_id mismatch: expected {}, got {}",
                    hex::encode(request_packet_id),
                    hex::encode(resp.packet_id),
                )));
            }
            Ok(resp.success)
        }
        other => Err(unexpected_response("StoreResponse", &other)),
    }
}

fn parse_pull_response(
    request_harbor_id: &[u8; 32],
    response: HarborMessage,
) -> Result<Vec<PacketInfo>, ProtocolError> {
    match response {
        HarborMessage::PullResponse(resp) => {
            if &resp.harbor_id != request_harbor_id {
                return Err(ProtocolError::Network(format!(
                    "pull response harbor_id mismatch: expected {}, got {}",
                    hex::encode(request_harbor_id),
                    hex::encode(resp.harbor_id),
                )));
            }
            Ok(resp.packets)
        }
        other => Err(unexpected_response("PullResponse", &other)),
    }
}

fn parse_sync_response(
    request_harbor_id: &[u8; 32],
    response: HarborMessage,
) -> Result<SyncResponse, ProtocolError> {
    match response {
        HarborMessage::SyncResponse(resp) => {
            if &resp.harbor_id != request_harbor_id {
                return Err(ProtocolError::Network(format!(
                    "sync response harbor_id mismatch: expected {}, got {}",
                    hex::encode(request_harbor_id),
                    hex::encode(resp.harbor_id),
                )));
            }
            Ok(resp)
        }
        other => Err(unexpected_response("SyncResponse", &other)),
    }
}

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
        let encoded = message
            .try_encode()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

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

        let response_bytes = tokio::time::timeout(
            self.response_timeout,
            recv.read_to_end(Self::STORE_RESPONSE_MAX_SIZE),
        )
        .await
        .map_err(|_| ProtocolError::Network("response read timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let response = HarborMessage::decode(&response_bytes)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        parse_store_response(&request.packet_id, response)
    }

    /// Send a PullRequest to a Harbor Node
    pub async fn send_harbor_pull(
        &self,
        harbor_node: &[u8; 32],
        request: &PullRequest,
    ) -> Result<Vec<PacketInfo>, ProtocolError> {
        let conn = self.connect_to_harbor_node(harbor_node).await?;

        let message = HarborMessage::Pull(request.clone());
        let encoded = message
            .try_encode()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

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

        let response_bytes = tokio::time::timeout(
            self.response_timeout,
            recv.read_to_end(Self::PULL_RESPONSE_MAX_SIZE),
        )
        .await
        .map_err(|_| ProtocolError::Network("response read timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let response = HarborMessage::decode(&response_bytes)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        parse_pull_response(&request.harbor_id, response)
    }

    /// Send a DeliveryAck to a Harbor Node (fire-and-forget)
    pub async fn send_harbor_ack(
        &self,
        harbor_node: &[u8; 32],
        ack: &DeliveryAck,
    ) -> Result<(), ProtocolError> {
        let conn = self.connect_to_harbor_node(harbor_node).await?;

        let message = HarborMessage::Ack(ack.clone());
        let encoded = message
            .try_encode()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

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

        let conn = self
            .connect_to_harbor_node(harbor_node)
            .await
            .map_err(|e| {
                debug!(partner = %node_hex, error = %e, "Harbor sync: connection failed");
                e
            })?;

        debug!(partner = %node_hex, "Harbor sync: connection established, opening request stream");

        let mut send = conn.open_uni().await.map_err(|e| {
            debug!(partner = %node_hex, error = %e, "Harbor sync: failed to open request stream");
            ProtocolError::Network(e.to_string())
        })?;

        // Send request
        let msg = HarborMessage::SyncRequest(request.clone());
        let data = msg
            .try_encode()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        debug!(
            partner = %node_hex,
            request_size = data.len(),
            packets_we_have = request.have_packets.len(),
            "Harbor sync: sending request"
        );

        tokio::io::AsyncWriteExt::write_all(&mut send, &data)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let mut recv = tokio::time::timeout(self.response_timeout, conn.accept_uni())
            .await
            .map_err(|_| ProtocolError::Network("response timeout".to_string()))?
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Read response
        let response_data = tokio::time::timeout(
            self.response_timeout,
            recv.read_to_end(Self::SYNC_RESPONSE_MAX_SIZE),
        )
        .await
        .map_err(|_| ProtocolError::Network("response read timeout".to_string()))?
        .map_err(|e| {
            debug!(partner = %node_hex, error = %e, "Harbor sync: failed to read response");
            ProtocolError::Network(e.to_string())
        })?;

        debug!(
            partner = %node_hex,
            response_size = response_data.len(),
            "Harbor sync: received response"
        );

        let response = HarborMessage::decode(&response_data)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let parsed = parse_sync_response(&request.harbor_id, response)?;
        debug!(
            partner = %node_hex,
            missing_packets = parsed.missing_packets.len(),
            delivery_updates = parsed.delivery_updates.len(),
            "Harbor sync: parsed response"
        );
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::harbor::protocol::{PullResponse, StoreResponse};

    #[test]
    fn test_parse_store_response_requires_matching_packet_id() {
        let req_packet_id = [1u8; 16];
        let resp = HarborMessage::StoreResponse(StoreResponse {
            packet_id: [2u8; 16],
            success: true,
            error: None,
            required_difficulty: None,
        });

        let err = parse_store_response(&req_packet_id, resp).unwrap_err();
        assert!(err.to_string().contains("packet_id mismatch"));
    }

    #[test]
    fn test_parse_pull_response_requires_matching_harbor_id() {
        let req_harbor_id = [1u8; 32];
        let resp = HarborMessage::PullResponse(PullResponse {
            harbor_id: [2u8; 32],
            packets: vec![],
        });

        let err = parse_pull_response(&req_harbor_id, resp).unwrap_err();
        assert!(err.to_string().contains("harbor_id mismatch"));
    }

    #[test]
    fn test_parse_sync_response_requires_matching_harbor_id() {
        let req_harbor_id = [1u8; 32];
        let resp = HarborMessage::SyncResponse(SyncResponse {
            harbor_id: [2u8; 32],
            missing_packets: vec![],
            delivery_updates: vec![],
            members: vec![],
        });

        let err = parse_sync_response(&req_harbor_id, resp).unwrap_err();
        assert!(err.to_string().contains("harbor_id mismatch"));
    }

    #[test]
    fn test_parse_sync_response_rejects_wrong_variant() {
        let req_harbor_id = [1u8; 32];
        let wrong = HarborMessage::StoreResponse(StoreResponse {
            packet_id: [1u8; 16],
            success: true,
            error: None,
            required_difficulty: None,
        });

        let err = parse_sync_response(&req_harbor_id, wrong).unwrap_err();
        assert!(err.to_string().contains("unexpected response"));
    }
}
