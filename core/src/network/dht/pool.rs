//! Connection pool for DHT protocol
//!
//! Manages connections to DHT peers with:
//! - Direct dial when fresh address is available (< 24 hours old)
//! - Fallback to dial via EndpointID using Iroh DNS
//! - Connection caching with idle timeout
//! - 0-RTT support for fast DHT queries

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, NodeId, NodeAddr};
use iroh::endpoint::Connection;
use tokio::sync::{oneshot, RwLock};
use tracing::{debug, trace, warn};

use crate::data::dht::{PeerInfo, ADDRESS_FRESHNESS_THRESHOLD_SECS};

// Re-export ALPN from rpc module for consistency
pub use super::rpc::RPC_ALPN as DHT_ALPN;

/// Configuration for the DHT connection pool
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// How long to keep idle connections around (default: 30 seconds)
    pub idle_timeout: Duration,
    /// Timeout for establishing a connection (default: 5 seconds)
    pub connect_timeout: Duration,
    /// Maximum number of concurrent connections (default: 256)
    pub max_connections: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(5),
            max_connections: 256,
        }
    }
}

/// Error when acquiring a connection from the pool
#[derive(Debug, Clone)]
pub enum PoolError {
    /// Connection pool is shut down
    Shutdown,
    /// Connection timed out
    Timeout,
    /// Too many connections
    TooManyConnections,
    /// Connection failed
    ConnectionFailed(String),
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoolError::Shutdown => write!(f, "connection pool is shut down"),
            PoolError::Timeout => write!(f, "connection timed out"),
            PoolError::TooManyConnections => write!(f, "too many connections"),
            PoolError::ConnectionFailed(e) => write!(f, "connection failed: {}", e),
        }
    }
}

impl std::error::Error for PoolError {}

/// Information about a peer for dialing
#[derive(Debug, Clone)]
pub struct DialInfo {
    /// The node ID (required)
    pub node_id: NodeId,
    /// Optional direct address (IP:port)
    pub address: Option<String>,
    /// When the address was last updated
    pub address_timestamp: Option<i64>,
    /// Optional relay URL for cross-network connectivity
    pub relay_url: Option<String>,
}

impl DialInfo {
    /// Create dial info from just a NodeId
    pub fn from_node_id(node_id: NodeId) -> Self {
        Self {
            node_id,
            address: None,
            address_timestamp: None,
            relay_url: None,
        }
    }

    /// Create dial info with relay URL
    pub fn from_node_id_with_relay(node_id: NodeId, relay_url: String) -> Self {
        Self {
            node_id,
            address: None,
            address_timestamp: None,
            relay_url: Some(relay_url),
        }
    }

    /// Create dial info from PeerInfo
    /// 
    /// Returns `None` if the peer's endpoint_id is not a valid NodeId
    pub fn from_peer_info(peer: &PeerInfo) -> Option<Self> {
        let node_id = NodeId::from_bytes(&peer.endpoint_id).ok()?;
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

    /// Convert to NodeAddr for iroh dialing
    pub fn to_node_addr(&self) -> NodeAddr {
        let mut addr: NodeAddr = self.node_id.into();
        
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
                    addr = addr.with_direct_addresses([socket_addr]);
                }
            }
        }
        
        addr
    }
}

/// A reference to a pooled connection
#[derive(Debug)]
pub struct ConnectionRef {
    /// The underlying connection
    connection: Connection,
    /// Node ID this connection is for
    node_id: NodeId,
    /// Sender to notify pool when dropped
    _drop_notify: Option<oneshot::Sender<NodeId>>,
}

impl ConnectionRef {
    /// Get the underlying connection
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Get the node ID
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }
}

impl std::ops::Deref for ConnectionRef {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

/// Internal state for a cached connection
struct CachedConnection {
    connection: Connection,
    last_used: std::time::Instant,
}

/// DHT connection pool
#[derive(Clone)]
pub struct DhtPool {
    /// Iroh endpoint
    endpoint: Endpoint,
    /// Configuration
    config: PoolConfig,
    /// Cached connections
    connections: Arc<RwLock<HashMap<NodeId, CachedConnection>>>,
    /// Order of connections for LRU eviction
    lru_order: Arc<RwLock<VecDeque<NodeId>>>,
    /// Pre-registered dial info (e.g., for bootstrap nodes)
    known_nodes: Arc<RwLock<HashMap<NodeId, DialInfo>>>,
}

impl DhtPool {
    /// Create a new DHT connection pool
    pub fn new(endpoint: Endpoint, config: PoolConfig) -> Self {
        Self {
            endpoint,
            config,
            connections: Arc::new(RwLock::new(HashMap::new())),
            lru_order: Arc::new(RwLock::new(VecDeque::new())),
            known_nodes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register dial info for a node (used for bootstrap nodes with relay URLs)
    pub async fn register_dial_info(&self, dial_info: DialInfo) {
        let mut known = self.known_nodes.write().await;
        known.insert(dial_info.node_id, dial_info);
    }

    /// Get our own node ID
    pub fn local_node_id(&self) -> NodeId {
        self.endpoint.node_id()
    }

    /// Get our home relay URL (if connected to a relay)
    pub fn home_relay_url(&self) -> Option<String> {
        self.endpoint.node_addr().relay_url.map(|url| url.to_string())
    }

    /// Get or create a connection to a peer
    pub async fn get_connection(&self, dial_info: &DialInfo) -> Result<ConnectionRef, PoolError> {
        let node_id = dial_info.node_id;

        // Check for cached connection
        {
            let connections = self.connections.read().await;
            if let Some(cached) = connections.get(&node_id) {
                // Check if connection is still alive
                if cached.connection.close_reason().is_none() {
                    trace!(target = %node_id, "reusing cached connection");
                    return Ok(ConnectionRef {
                        connection: cached.connection.clone(),
                        node_id,
                        _drop_notify: None,
                    });
                }
            }
        }

        // Need to establish new connection
        self.connect(dial_info).await
    }

    /// Establish a new connection
    async fn connect(&self, dial_info: &DialInfo) -> Result<ConnectionRef, PoolError> {
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

        // Check connection limit
        {
            let connections = self.connections.read().await;
            if connections.len() >= self.config.max_connections {
                // Try to evict idle connection
                drop(connections);
                self.evict_idle().await;
                
                let connections = self.connections.read().await;
                if connections.len() >= self.config.max_connections {
                    warn!("connection pool full");
                    return Err(PoolError::TooManyConnections);
                }
            }
        }

        // Build node address
        let node_addr = merged_dial_info.to_node_addr();
        
        // Log at INFO level if no relay URL - this is likely a discovery issue
        if merged_dial_info.relay_url.is_none() {
            warn!(
                target = %node_id,
                "connecting to peer WITHOUT relay URL - may fail"
            );
        } else {
            debug!(
                target = %node_id,
                has_direct = merged_dial_info.has_fresh_address(),
                relay = ?merged_dial_info.relay_url,
                "connecting to peer"
            );
        }

        // Connect with timeout
        let connect_fut = self.endpoint.connect(node_addr, DHT_ALPN);
        let connection = tokio::time::timeout(self.config.connect_timeout, connect_fut)
            .await
            .map_err(|_| PoolError::Timeout)?
            .map_err(|e| PoolError::ConnectionFailed(e.to_string()))?;

        // Cache the connection
        {
            let mut connections = self.connections.write().await;
            let mut lru = self.lru_order.write().await;
            
            connections.insert(node_id, CachedConnection {
                connection: connection.clone(),
                last_used: std::time::Instant::now(),
            });
            
            // Update LRU order
            lru.retain(|id| *id != node_id);
            lru.push_back(node_id);
        }

        Ok(ConnectionRef {
            connection,
            node_id,
            _drop_notify: None,
        })
    }

    /// Evict the least recently used idle connection
    async fn evict_idle(&self) {
        let mut connections = self.connections.write().await;
        let mut lru = self.lru_order.write().await;

        let now = std::time::Instant::now();
        
        // Find oldest idle connection
        while let Some(oldest_id) = lru.front().copied() {
            if let Some(cached) = connections.get(&oldest_id) {
                // Check if idle
                if now.duration_since(cached.last_used) > self.config.idle_timeout {
                    trace!(target = %oldest_id, "evicting idle connection");
                    cached.connection.close(0u32.into(), b"idle");
                    connections.remove(&oldest_id);
                    lru.pop_front();
                    return;
                }
            } else {
                // Connection not found, remove from LRU
                lru.pop_front();
            }
            break; // If oldest isn't idle, none will be
        }
    }

    /// Close a specific connection
    pub async fn close(&self, node_id: NodeId) {
        let mut connections = self.connections.write().await;
        let mut lru = self.lru_order.write().await;

        if let Some(cached) = connections.remove(&node_id) {
            cached.connection.close(0u32.into(), b"close");
        }
        lru.retain(|id| *id != node_id);
    }

    /// Get statistics about the pool
    pub async fn stats(&self) -> PoolStats {
        let connections = self.connections.read().await;
        PoolStats {
            active_connections: connections.len(),
            max_connections: self.config.max_connections,
        }
    }
}

/// Statistics about the connection pool
#[derive(Debug, Clone)]
pub struct PoolStats {
    /// Number of active connections
    pub active_connections: usize,
    /// Maximum allowed connections
    pub max_connections: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(addr.node_id, node_id);
        assert!(addr.direct_addresses.is_empty());
        
        // With fresh address
        let info_with_addr = DialInfo {
            node_id,
            address: Some("192.168.1.1:4433".to_string()),
            address_timestamp: Some(now),
            relay_url: None,
        };
        let addr = info_with_addr.to_node_addr();
        assert_eq!(addr.node_id, node_id);
        assert!(!addr.direct_addresses.is_empty());
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

    // Note: We cannot reliably test invalid NodeId bytes because iroh's
    // NodeId::from_bytes() is permissive and accepts many byte patterns
    // that are not valid Ed25519 public keys (e.g., all zeros).
    // See DEPENDENCIES.md "Iroh NodeId Validation" note.
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
        assert_eq!(addr.node_id, node_id);
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
        assert_eq!(addr.node_id, node_id);
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
        assert_eq!(addr.node_id, node_id);
        assert!(addr.direct_addresses.is_empty()); // Invalid address not added
    }

    // Note: Integration tests that actually create connections require
    // a running Iroh network, which is tested separately.
}

