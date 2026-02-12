//! Connection pool for DHT protocol
//!
//! Manages connections to DHT peers with:
//! - Relay URL freshness logic with DNS fallback
//! - Connection caching with idle timeout (via Connector pool)
//! - 0-RTT support for fast DHT queries

use std::collections::HashMap;
use std::sync::Arc;

use iroh::EndpointId;
use iroh::endpoint::Connection;
use tokio::sync::RwLock;

use crate::data::dht::PeerInfo;
use crate::network::connect::{ConnectError, Connector};
use crate::network::pool::PoolError;

// Re-export ALPN from protocol module for consistency
pub use crate::network::dht::protocol::RPC_ALPN as DHT_ALPN;

// Re-export common types from generic pool
pub use crate::network::pool::{
    ConnectionRef as DhtConnectionRef, PoolConfig as DhtPoolConfig, PoolError as DhtPoolError,
    PoolStats,
};

/// Information about a peer for dialing
#[derive(Debug, Clone)]
pub struct DialInfo {
    /// The node ID (required)
    pub node_id: EndpointId,
    /// Optional relay URL for cross-network connectivity
    pub relay_url: Option<String>,
    /// When the relay URL was last confirmed working
    pub relay_url_last_success: Option<i64>,
}

impl DialInfo {
    /// Create dial info from just a EndpointId
    pub fn from_node_id(node_id: EndpointId) -> Self {
        Self {
            node_id,
            relay_url: None,
            relay_url_last_success: None,
        }
    }

    /// Create dial info with relay URL
    pub fn from_node_id_with_relay(node_id: EndpointId, relay_url: String) -> Self {
        Self {
            node_id,
            relay_url: Some(relay_url),
            relay_url_last_success: None,
        }
    }

    /// Create dial info from PeerInfo
    ///
    /// Returns `None` if the peer's endpoint_id is not a valid EndpointId
    pub fn from_peer_info(peer: &PeerInfo) -> Option<Self> {
        let node_id = EndpointId::from_bytes(&peer.endpoint_id).ok()?;
        Some(Self {
            node_id,
            relay_url: peer.relay_url.clone(),
            relay_url_last_success: peer.relay_url_last_success,
        })
    }
}

/// DHT connection pool
#[derive(Clone)]
pub struct DhtPool {
    /// Service-specific connector with pooling enabled
    connector: Arc<Connector>,
    /// Pre-registered dial info (e.g., for bootstrap nodes)
    known_nodes: Arc<RwLock<HashMap<EndpointId, DialInfo>>>,
}

impl DhtPool {
    /// Create a new DHT connection pool
    pub fn new(connector: Arc<Connector>) -> Self {
        Self {
            connector,
            known_nodes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get our home relay URL (if connected to a relay)
    pub fn home_relay_url(&self) -> Option<String> {
        self.connector
            .endpoint()
            .addr()
            .relay_urls()
            .next()
            .map(|url| url.to_string())
    }

    /// Register dial info for a node (used for bootstrap nodes with relay URLs)
    pub async fn register_dial_info(&self, dial_info: DialInfo) {
        let mut known = self.known_nodes.write().await;
        known.insert(dial_info.node_id, dial_info);
    }

    /// Get or create a connection to a peer
    ///
    /// Returns the connection and whether the relay URL was confirmed working.
    pub async fn get_connection(
        &self,
        dial_info: &DialInfo,
    ) -> Result<(Connection, bool), PoolError> {
        let node_id = dial_info.node_id;

        // Merge with known node info (e.g., bootstrap relay URLs)
        let merged_dial_info = {
            let known = self.known_nodes.read().await;
            if let Some(known_info) = known.get(&node_id) {
                // Prefer provided relay_url, fall back to known
                DialInfo {
                    node_id,
                    relay_url: dial_info
                        .relay_url
                        .clone()
                        .or_else(|| known_info.relay_url.clone()),
                    relay_url_last_success: dial_info
                        .relay_url_last_success
                        .or(known_info.relay_url_last_success),
                }
            } else {
                dial_info.clone()
            }
        };

        // Parse relay URL for the smart connect primitive
        let parsed_relay: Option<iroh::RelayUrl> = merged_dial_info
            .relay_url
            .as_ref()
            .and_then(|url| url.parse().ok());

        let result = self
            .connector
            .connect_with_relay_hint(
                node_id.as_bytes(),
                parsed_relay.as_ref(),
                merged_dial_info.relay_url_last_success,
            )
            .await
            .map_err(connect_error_to_pool_error)?;

        Ok((result.connection, result.relay_url_confirmed))
    }

    /// Close a specific connection
    pub async fn close(&self, node_id: EndpointId) {
        self.connector.close(node_id.as_bytes()).await;
    }

    /// Get statistics about the pool
    pub async fn stats(&self) -> PoolStats {
        self.connector.pool_stats().await.unwrap_or(PoolStats {
            active_connections: 0,
            max_connections: 0,
        })
    }
}

fn connect_error_to_pool_error(err: ConnectError) -> PoolError {
    match err {
        ConnectError::Timeout => PoolError::Timeout,
        ConnectError::Failed(msg) => PoolError::ConnectionFailed(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_dial_info_from_node_id() {
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let node_id = secret.public();
        let info = DialInfo::from_node_id(node_id);

        assert_eq!(info.node_id, node_id);
        assert!(info.relay_url.is_none());
        assert!(info.relay_url_last_success.is_none());
    }

    #[test]
    fn test_pool_config_default() {
        let config = DhtPoolConfig::default();
        assert_eq!(config.idle_timeout, Duration::from_secs(30));
        assert_eq!(config.connect_timeout, Duration::from_secs(5));
        assert_eq!(config.max_connections, 256);
    }

    #[test]
    fn test_pool_error_display() {
        assert_eq!(
            PoolError::Shutdown.to_string(),
            "connection pool is shut down"
        );
        assert_eq!(PoolError::Timeout.to_string(), "connection timed out");
        assert_eq!(
            PoolError::TooManyConnections.to_string(),
            "too many connections"
        );
        assert_eq!(
            PoolError::ConnectionFailed("test".to_string()).to_string(),
            "connection failed: test"
        );
    }

    #[test]
    fn test_dial_info_from_node_id_with_relay() {
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let node_id = secret.public();
        let relay_url = "https://relay.example.com/".to_string();

        let info = DialInfo::from_node_id_with_relay(node_id, relay_url.clone());

        assert_eq!(info.node_id, node_id);
        assert_eq!(info.relay_url, Some(relay_url));
    }

    #[test]
    fn test_dial_info_from_peer_info() {
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let node_id = secret.public();

        let peer = PeerInfo {
            endpoint_id: *node_id.as_bytes(),
            last_latency_ms: Some(50),
            latency_timestamp: Some(12345),
            last_seen: 67890,
            relay_url: Some("https://relay.example.com/".to_string()),
            relay_url_last_success: Some(12345),
        };

        let info = DialInfo::from_peer_info(&peer);
        assert!(info.is_some());

        let info = info.unwrap();
        assert_eq!(info.node_id, node_id);
        assert_eq!(
            info.relay_url,
            Some("https://relay.example.com/".to_string())
        );
        assert_eq!(info.relay_url_last_success, Some(12345));
    }

    // Note: We cannot reliably test invalid EndpointId bytes because iroh's
    // EndpointId::from_bytes() is permissive and accepts many byte patterns
    // that are not valid Ed25519 public keys (e.g., all zeros).
    // See DEPENDENCIES.md "Iroh EndpointId Validation" note.
    // The from_peer_info() returns Option<Self> to handle the case where
    // from_bytes() does fail, but this is defensive coding rather than
    // something we can easily test.

    // Note: Integration tests that actually create connections require
    // a running Iroh network, which is tested separately.
}
