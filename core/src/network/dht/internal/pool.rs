//! Connection pool for DHT protocol
//!
//! Manages connections to DHT peers with:
//! - Direct dial when fresh address is available (< 24 hours old)
//! - Fallback to dial via EndpointID using Iroh DNS
//! - Connection caching with idle timeout
//! - 0-RTT support for fast DHT queries

use std::collections::HashMap;
use std::sync::Arc;

use iroh::{Endpoint, EndpointId, EndpointAddr};
use tokio::sync::RwLock;

use crate::data::dht::{PeerInfo, ADDRESS_FRESHNESS_THRESHOLD_SECS};
use crate::network::pool::{ConnectionPool, PoolConfig, PoolError, ConnectionRef};

// Re-export ALPN from protocol module for consistency
pub use crate::network::dht::protocol::RPC_ALPN as DHT_ALPN;

// Re-export common types from generic pool
pub use crate::network::pool::{PoolConfig as DhtPoolConfig, PoolError as DhtPoolError, ConnectionRef as DhtConnectionRef, PoolStats};

/// Information about a peer for dialing
#[derive(Debug, Clone)]
pub struct DialInfo {
    /// The node ID (required)
    pub node_id: EndpointId,
    /// Optional direct address (IP:port)
    pub address: Option<String>,
    /// When the address was last updated
    pub address_timestamp: Option<i64>,
    /// Optional relay URL for cross-network connectivity
    pub relay_url: Option<String>,
}

impl DialInfo {
    /// Create dial info from just a EndpointId
    pub fn from_node_id(node_id: EndpointId) -> Self {
        Self {
            node_id,
            address: None,
            address_timestamp: None,
            relay_url: None,
        }
    }

    /// Create dial info with relay URL
    pub fn from_node_id_with_relay(node_id: EndpointId, relay_url: String) -> Self {
        Self {
            node_id,
            address: None,
            address_timestamp: None,
            relay_url: Some(relay_url),
        }
    }

    /// Create dial info from PeerInfo
    ///
    /// Returns `None` if the peer's endpoint_id is not a valid EndpointId
    pub fn from_peer_info(peer: &PeerInfo) -> Option<Self> {
        let node_id = EndpointId::from_bytes(&peer.endpoint_id).ok()?;
        Some(Self {
            node_id,
            address: peer.endpoint_address.clone(),
            address_timestamp: peer.address_timestamp,
            relay_url: None,
        })
    }

    /// Check if we have a fresh direct address
    pub fn has_fresh_address(&self) -> bool {
        if let (Some(addr), Some(timestamp)) = (&self.address, self.address_timestamp) {
            if addr.is_empty() {
                return false;
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            now - timestamp < ADDRESS_FRESHNESS_THRESHOLD_SECS
        } else {
            false
        }
    }

    /// Convert to EndpointAddr for iroh dialing
    pub fn to_node_addr(&self) -> EndpointAddr {
        let mut addr: EndpointAddr = self.node_id.into();

        // Add relay URL if available
        if let Some(ref relay_url) = self.relay_url {
            if let Ok(relay) = relay_url.parse() {
                addr = addr.with_relay_url(relay);
            }
        }

        // Add direct address if fresh
        if self.has_fresh_address() {
            if let Some(address) = &self.address {
                if let Ok(socket_addr) = address.parse() {
                    addr = addr.with_ip_addr(socket_addr);
                }
            }
        }

        addr
    }
}

/// DHT connection pool
#[derive(Clone)]
pub struct DhtPool {
    /// Generic connection pool
    pool: ConnectionPool,
    /// Iroh endpoint (needed for helper methods)
    endpoint: Endpoint,
    /// Pre-registered dial info (e.g., for bootstrap nodes)
    known_nodes: Arc<RwLock<HashMap<EndpointId, DialInfo>>>,
}

impl DhtPool {
    /// Create a new DHT connection pool
    pub fn new(endpoint: Endpoint, config: PoolConfig) -> Self {
        Self {
            pool: ConnectionPool::new(endpoint.clone(), DHT_ALPN, config),
            endpoint,
            known_nodes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get our home relay URL (if connected to a relay)
    pub fn home_relay_url(&self) -> Option<String> {
        self.endpoint.addr().relay_urls().next().map(|url| url.to_string())
    }

    /// Register dial info for a node (used for bootstrap nodes with relay URLs)
    pub async fn register_dial_info(&self, dial_info: DialInfo) {
        let mut known = self.known_nodes.write().await;
        known.insert(dial_info.node_id, dial_info);
    }

    /// Get or create a connection to a peer
    pub async fn get_connection(&self, dial_info: &DialInfo) -> Result<ConnectionRef, PoolError> {
        let node_id = dial_info.node_id;

        // Merge with known node info (e.g., bootstrap relay URLs)
        let merged_dial_info = {
            let known = self.known_nodes.read().await;
            if let Some(known_info) = known.get(&node_id) {
                // Prefer provided relay_url, fall back to known
                DialInfo {
                    node_id,
                    address: dial_info.address.clone().or_else(|| known_info.address.clone()),
                    address_timestamp: dial_info.address_timestamp.or(known_info.address_timestamp),
                    relay_url: dial_info.relay_url.clone().or_else(|| known_info.relay_url.clone()),
                }
            } else {
                dial_info.clone()
            }
        };

        // Build node address and delegate to generic pool
        let node_addr = merged_dial_info.to_node_addr();
        self.pool.get_connection(node_addr).await
    }

    /// Close a specific connection
    pub async fn close(&self, node_id: EndpointId) {
        self.pool.close(node_id).await;
    }

    /// Get statistics about the pool
    pub async fn stats(&self) -> PoolStats {
        self.pool.stats().await
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
        assert!(info.address.is_none());
        assert!(!info.has_fresh_address());
    }

    #[test]
    fn test_dial_info_fresh_address() {
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let node_id = secret.public();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let fresh = DialInfo {
            node_id,
            address: Some("192.168.1.1:4433".to_string()),
            address_timestamp: Some(now - 3600), // 1 hour ago
            relay_url: None,
        };
        assert!(fresh.has_fresh_address());

        let stale = DialInfo {
            node_id,
            address: Some("192.168.1.1:4433".to_string()),
            address_timestamp: Some(now - 100000), // > 24 hours ago
            relay_url: None,
        };
        assert!(!stale.has_fresh_address());
    }

    #[test]
    fn test_dial_info_to_node_addr() {
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let node_id = secret.public();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Without address
        let info = DialInfo::from_node_id(node_id);
        let addr = info.to_node_addr();
        assert_eq!(addr.id, node_id);
        assert!(addr.ip_addrs().next().is_none());

        // With fresh address
        let info_with_addr = DialInfo {
            node_id,
            address: Some("192.168.1.1:4433".to_string()),
            address_timestamp: Some(now),
            relay_url: None,
        };
        let addr = info_with_addr.to_node_addr();
        assert_eq!(addr.id, node_id);
        assert!(!addr.ip_addrs().next().is_none());
    }

    #[test]
    fn test_pool_config_default() {
        let config = PoolConfig::default();
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
        assert_eq!(
            PoolError::Timeout.to_string(),
            "connection timed out"
        );
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
        assert!(info.address.is_none());
        assert_eq!(info.relay_url, Some(relay_url));
    }

    #[test]
    fn test_dial_info_from_peer_info() {
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let node_id = secret.public();

        let peer = PeerInfo {
            endpoint_id: *node_id.as_bytes(),
            endpoint_address: Some("192.168.1.1:4433".to_string()),
            address_timestamp: Some(12345),
            last_latency_ms: Some(50),
            latency_timestamp: Some(12345),
            last_seen: 67890,
        };

        let info = DialInfo::from_peer_info(&peer);
        assert!(info.is_some());

        let info = info.unwrap();
        assert_eq!(info.node_id, node_id);
        assert_eq!(info.address, Some("192.168.1.1:4433".to_string()));
        assert_eq!(info.address_timestamp, Some(12345));
    }

    // Note: We cannot reliably test invalid EndpointId bytes because iroh's
    // EndpointId::from_bytes() is permissive and accepts many byte patterns
    // that are not valid Ed25519 public keys (e.g., all zeros).
    // See DEPENDENCIES.md "Iroh EndpointId Validation" note.
    // The from_peer_info() returns Option<Self> to handle the case where
    // from_bytes() does fail, but this is defensive coding rather than
    // something we can easily test.

    #[test]
    fn test_dial_info_empty_address() {
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let node_id = secret.public();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Empty address string should not count as fresh
        let info = DialInfo {
            node_id,
            address: Some("".to_string()),
            address_timestamp: Some(now),
            relay_url: None,
        };
        assert!(!info.has_fresh_address());
    }

    #[test]
    fn test_dial_info_to_node_addr_with_relay() {
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let node_id = secret.public();

        let info = DialInfo {
            node_id,
            address: None,
            address_timestamp: None,
            relay_url: Some("https://relay.example.com/".to_string()),
        };

        let addr = info.to_node_addr();
        assert_eq!(addr.id, node_id);
        // Relay URL should be set (we can't easily inspect it, but it shouldn't panic)
    }

    #[test]
    fn test_dial_info_to_node_addr_invalid_relay_url() {
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let node_id = secret.public();

        // Invalid relay URL should be silently ignored
        let info = DialInfo {
            node_id,
            address: None,
            address_timestamp: None,
            relay_url: Some("not a valid url".to_string()),
        };

        let addr = info.to_node_addr();
        assert_eq!(addr.id, node_id);
        // Should not panic even with invalid URL
    }

    #[test]
    fn test_dial_info_to_node_addr_invalid_socket_addr() {
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let node_id = secret.public();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Invalid socket address should be silently ignored
        let info = DialInfo {
            node_id,
            address: Some("not.a.valid.address".to_string()),
            address_timestamp: Some(now),
            relay_url: None,
        };

        let addr = info.to_node_addr();
        assert_eq!(addr.id, node_id);
        assert!(addr.ip_addrs().next().is_none()); // Invalid address not added
    }

    // Note: Integration tests that actually create connections require
    // a running Iroh network, which is tested separately.
}
