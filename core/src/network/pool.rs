//! Generic connection pool for QUIC protocols
//!
//! Provides connection pooling and reuse for any ALPN protocol.
//! Features:
//! - Connection caching with idle timeout
//! - LRU eviction when at capacity
//! - Connection liveness checking
//! - Configurable timeouts and limits
//! - Thread-safe (Clone + Send + Sync)

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, EndpointId, RelayUrl};
use iroh::endpoint::Connection;
use tokio::sync::{oneshot, RwLock};
use tracing::{debug, trace, warn};

use crate::network::connect;

/// Configuration for a connection pool
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

/// A reference to a pooled connection
#[derive(Debug)]
pub struct ConnectionRef {
    /// The underlying connection
    connection: Connection,
    /// Node ID this connection is for
    node_id: EndpointId,
    /// Sender to notify pool when dropped
    _drop_notify: Option<oneshot::Sender<EndpointId>>,
}

impl ConnectionRef {
    /// Get the underlying connection
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Get the node ID
    pub fn node_id(&self) -> EndpointId {
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

/// Generic connection pool for any ALPN protocol
#[derive(Clone)]
pub struct ConnectionPool {
    /// Iroh endpoint
    endpoint: Endpoint,
    /// ALPN identifier for this pool
    alpn: &'static [u8],
    /// Configuration
    config: PoolConfig,
    /// Cached connections
    connections: Arc<RwLock<HashMap<EndpointId, CachedConnection>>>,
    /// Order of connections for LRU eviction
    lru_order: Arc<RwLock<VecDeque<EndpointId>>>,
}

impl ConnectionPool {
    /// Create a new connection pool for the given ALPN
    pub fn new(endpoint: Endpoint, alpn: &'static [u8], config: PoolConfig) -> Self {
        Self {
            endpoint,
            alpn,
            config,
            connections: Arc::new(RwLock::new(HashMap::new())),
            lru_order: Arc::new(RwLock::new(VecDeque::new())),
        }
    }

    /// Get the ALPN identifier for this pool
    pub fn alpn(&self) -> &'static [u8] {
        self.alpn
    }

    /// Get or create a connection to a peer.
    ///
    /// Uses smart connection logic: if `relay_url` is fresh (based on
    /// `relay_url_last_success`), connects via relay first, falling back
    /// to DNS discovery. Returns whether the relay URL was confirmed working.
    pub async fn get_connection(
        &self,
        node_id: EndpointId,
        relay_url: Option<&RelayUrl>,
        relay_url_last_success: Option<i64>,
    ) -> Result<(ConnectionRef, bool), PoolError> {
        // Check for cached connection
        {
            let connections = self.connections.read().await;
            if let Some(cached) = connections.get(&node_id) {
                // Check if connection is still alive
                if cached.connection.close_reason().is_none() {
                    trace!(
                        alpn = ?std::str::from_utf8(self.alpn),
                        target = %node_id,
                        "reusing cached connection"
                    );
                    return Ok((ConnectionRef {
                        connection: cached.connection.clone(),
                        node_id,
                        _drop_notify: None,
                    }, false));
                }
            }
        }

        // Need to establish new connection
        self.connect_smart(node_id, relay_url, relay_url_last_success).await
    }

    /// Establish a new connection using smart relay/DNS logic.
    async fn connect_smart(
        &self,
        node_id: EndpointId,
        relay_url: Option<&RelayUrl>,
        relay_url_last_success: Option<i64>,
    ) -> Result<(ConnectionRef, bool), PoolError> {
        // Check connection limit
        {
            let connections = self.connections.read().await;
            if connections.len() >= self.config.max_connections {
                // Try to evict idle connection
                drop(connections);
                self.evict_idle().await;

                let connections = self.connections.read().await;
                if connections.len() >= self.config.max_connections {
                    warn!(
                        alpn = ?std::str::from_utf8(self.alpn),
                        "connection pool full"
                    );
                    return Err(PoolError::TooManyConnections);
                }
            }
        }

        debug!(
            alpn = ?std::str::from_utf8(self.alpn),
            target = %node_id,
            "establishing new connection"
        );

        let result = connect::connect(
            &self.endpoint,
            node_id,
            relay_url,
            relay_url_last_success,
            self.alpn,
            self.config.connect_timeout,
        )
        .await
        .map_err(|e| match e {
            connect::ConnectError::Timeout => PoolError::Timeout,
            connect::ConnectError::Failed(msg) => PoolError::ConnectionFailed(msg),
        })?;

        let relay_url_confirmed = result.relay_url_confirmed;

        // Cache the connection
        {
            let mut connections = self.connections.write().await;
            let mut lru = self.lru_order.write().await;

            connections.insert(node_id, CachedConnection {
                connection: result.connection.clone(),
                last_used: std::time::Instant::now(),
            });

            // Update LRU order
            lru.retain(|id| *id != node_id);
            lru.push_back(node_id);
        }

        Ok((ConnectionRef {
            connection: result.connection,
            node_id,
            _drop_notify: None,
        }, relay_url_confirmed))
    }

    /// Evict the least recently used idle connection
    async fn evict_idle(&self) {
        let mut connections = self.connections.write().await;
        let mut lru = self.lru_order.write().await;

        let now = std::time::Instant::now();

        // Find oldest idle connection
        if let Some(oldest_id) = lru.front().copied() {
            if let Some(cached) = connections.get(&oldest_id) {
                // Check if idle
                if now.duration_since(cached.last_used) > self.config.idle_timeout {
                    trace!(
                        alpn = ?std::str::from_utf8(self.alpn),
                        target = %oldest_id,
                        "evicting idle connection"
                    );
                    cached.connection.close(0u32.into(), b"idle");
                    connections.remove(&oldest_id);
                    lru.pop_front();
                }
                // If oldest isn't idle, none will be
            } else {
                // Connection not found, remove from LRU
                lru.pop_front();
            }
        }
    }

    /// Close a specific connection
    pub async fn close(&self, node_id: EndpointId) {
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

/// Statistics about a connection pool
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
}
