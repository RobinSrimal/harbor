//! Harbor Node client operations
//!
//! This module contains methods for communicating with Harbor Nodes:
//! - Storing packets on Harbor Nodes
//! - Pulling missed packets from Harbor Nodes
//! - Sending delivery acknowledgements
//!
//! Note: Finding Harbor Nodes uses `dht_lookup()` from outgoing/dht.rs

use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, EndpointId};
use tracing::debug;

use crate::network::dht::DhtService;
use crate::network::harbor::protocol::{
    HARBOR_ALPN, HarborMessage, StoreRequest, PullRequest, DeliveryAck, PacketInfo,
};

use crate::protocol::{Protocol, ProtocolError};

/// Maximum buffer size for store responses (small, just status)
const STORE_RESPONSE_MAX_SIZE: usize = 1024;

/// Maximum buffer size for pull responses (can contain many packets)
const PULL_RESPONSE_MAX_SIZE: usize = 10 * 1024 * 1024; // 10MB

impl Protocol {
    /// Find Harbor Nodes for a given HarborID using DHT lookup
    ///
    /// This is a convenience wrapper that handles the Option<Arc<DhtService>>.
    /// For direct DHT access, use `dht_lookup()`.
    pub(crate) async fn find_harbor_nodes(
        dht_service: &Option<Arc<DhtService>>,
        harbor_id: &[u8; 32],
    ) -> Vec<[u8; 32]> {
        let Some(client) = dht_service else {
            debug!("DHT client not available, cannot find Harbor Nodes");
            return vec![];
        };

        use crate::network::dht::Id as DhtId;
        let target = DhtId::new(*harbor_id);
        let initial: Option<Vec<[u8; 32]>> = None;
        match client.lookup(target, initial).await {
            Ok(nodes) => {
                debug!(
                    harbor_id = hex::encode(harbor_id),
                    count = nodes.len(),
                    "found Harbor Nodes via DHT"
                );
                nodes
            }
            Err(e) => {
                debug!(error = %e, "DHT lookup failed");
                vec![]
            }
        }
    }

    /// Send a StoreRequest to a Harbor Node
    pub(crate) async fn send_harbor_store(
        endpoint: &Endpoint,
        harbor_node: &[u8; 32],
        request: &StoreRequest,
        connect_timeout: Duration,
        response_timeout: Duration,
    ) -> Result<bool, ProtocolError> {
        let node_id = EndpointId::from_bytes(harbor_node)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let conn = tokio::time::timeout(
            connect_timeout,
            endpoint.connect(node_id, HARBOR_ALPN),
        )
        .await
        .map_err(|_| ProtocolError::Network("connect timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let message = HarborMessage::Store(request.clone());
        let encoded = message.encode();

        // Send request
        let mut send = conn.open_uni().await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        tokio::io::AsyncWriteExt::write_all(&mut send, &encoded).await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Wait for response with timeout
        let mut recv = tokio::time::timeout(
            response_timeout,
            conn.accept_uni(),
        )
        .await
        .map_err(|_| ProtocolError::Network("response timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))?;
        
        let response_bytes = recv.read_to_end(STORE_RESPONSE_MAX_SIZE).await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let response = HarborMessage::decode(&response_bytes)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        match response {
            HarborMessage::StoreResponse(resp) => Ok(resp.success),
            _ => Err(ProtocolError::Network("unexpected response".to_string())),
        }
    }

    /// Send a PullRequest to a Harbor Node
    pub(crate) async fn send_harbor_pull(
        endpoint: &Endpoint,
        harbor_node: &[u8; 32],
        request: &PullRequest,
        connect_timeout: Duration,
        response_timeout: Duration,
    ) -> Result<Vec<PacketInfo>, ProtocolError> {
        let node_id = EndpointId::from_bytes(harbor_node)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let conn = tokio::time::timeout(
            connect_timeout,
            endpoint.connect(node_id, HARBOR_ALPN),
        )
        .await
        .map_err(|_| ProtocolError::Network("connect timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let message = HarborMessage::Pull(request.clone());
        let encoded = message.encode();

        // Send request
        let mut send = conn.open_uni().await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        tokio::io::AsyncWriteExt::write_all(&mut send, &encoded).await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Wait for response with timeout (larger buffer for packets)
        let mut recv = tokio::time::timeout(
            response_timeout,
            conn.accept_uni(),
        )
        .await
        .map_err(|_| ProtocolError::Network("response timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))?;
        
        let response_bytes = recv.read_to_end(PULL_RESPONSE_MAX_SIZE).await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let response = HarborMessage::decode(&response_bytes)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        match response {
            HarborMessage::PullResponse(resp) => Ok(resp.packets),
            _ => Err(ProtocolError::Network("unexpected response".to_string())),
        }
    }

    /// Send a DeliveryAck to a Harbor Node
    pub(crate) async fn send_harbor_ack(
        endpoint: &Endpoint,
        harbor_node: &[u8; 32],
        ack: &DeliveryAck,
        connect_timeout: Duration,
    ) -> Result<(), ProtocolError> {
        let node_id = EndpointId::from_bytes(harbor_node)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let conn = tokio::time::timeout(
            connect_timeout,
            endpoint.connect(node_id, HARBOR_ALPN),
        )
        .await
        .map_err(|_| ProtocolError::Network("connect timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let message = HarborMessage::Ack(ack.clone());
        let encoded = message.encode();

        // Send ack (no response expected)
        let mut send = conn.open_uni().await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        tokio::io::AsyncWriteExt::write_all(&mut send, &encoded).await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ProtocolConfig;

    fn make_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_harbor_connect_timeout_default_is_reasonable() {
        let config = ProtocolConfig::default();
        assert!(config.harbor_connect_timeout_secs >= 3);
        assert!(config.harbor_connect_timeout_secs <= 30);
    }

    #[test]
    fn test_harbor_response_timeout_default_is_reasonable() {
        let config = ProtocolConfig::default();
        // Response might take longer due to data transfer
        assert!(config.harbor_response_timeout_secs >= 10);
        assert!(config.harbor_response_timeout_secs <= 120);
    }

    #[test]
    fn test_response_timeout_longer_than_connect() {
        let config = ProtocolConfig::default();
        // Response timeout should be longer than connect timeout
        assert!(config.harbor_response_timeout_secs > config.harbor_connect_timeout_secs);
    }

    #[test]
    fn test_store_response_max_size() {
        // Store responses are small (just status)
        assert!(STORE_RESPONSE_MAX_SIZE <= 4096);
        assert!(STORE_RESPONSE_MAX_SIZE >= 256);
    }

    #[test]
    fn test_pull_response_max_size() {
        // Pull responses can be large (multiple packets)
        assert!(PULL_RESPONSE_MAX_SIZE >= 1024 * 1024); // At least 1MB
        assert!(PULL_RESPONSE_MAX_SIZE <= 100 * 1024 * 1024); // At most 100MB
    }

    #[test]
    fn test_pull_response_larger_than_store() {
        // Pull responses contain packets, store responses are just status
        assert!(PULL_RESPONSE_MAX_SIZE > STORE_RESPONSE_MAX_SIZE);
    }

    #[test]
    fn test_node_id_from_harbor_node() {
        let harbor_node = make_id(42);
        let result = iroh::EndpointId::from_bytes(&harbor_node);
        assert!(result.is_ok());
    }

    #[test]
    fn test_dht_id_from_harbor_id() {
        use crate::network::dht::Id as DhtId;
        let harbor_id = make_id(123);
        let target = DhtId::new(harbor_id);
        assert_eq!(*target.as_bytes(), harbor_id);
    }
}

