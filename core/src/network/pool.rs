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

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointId, RelayUrl};
use tokio::sync::{RwLock, oneshot};
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

fn touch_lru(lru: &mut VecDeque<EndpointId>, node_id: EndpointId) {
    lru.retain(|id| *id != node_id);
    lru.push_back(node_id);
}

fn remove_lru(lru: &mut VecDeque<EndpointId>, node_id: EndpointId) {
    lru.retain(|id| *id != node_id);
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
        self.get_connection_with_timeout(
            node_id,
            relay_url,
            relay_url_last_success,
            self.config.connect_timeout,
        )
        .await
    }

    /// Get or create a connection to a peer with a caller-provided timeout.
    pub async fn get_connection_with_timeout(
        &self,
        node_id: EndpointId,
        relay_url: Option<&RelayUrl>,
        relay_url_last_success: Option<i64>,
        connect_timeout: Duration,
    ) -> Result<(ConnectionRef, bool), PoolError> {
        if let Some(cached) = self.try_get_cached(node_id).await {
            return Ok((cached, false));
        }

        // Keep pool capacity accurate by dropping already-closed cached entries.
        let _ = self.prune_closed_connections().await;

        // Need to establish new connection
        self.connect_smart(node_id, relay_url, relay_url_last_success, connect_timeout)
            .await
    }

    /// Establish a new connection using smart relay/DNS logic.
    async fn connect_smart(
        &self,
        node_id: EndpointId,
        relay_url: Option<&RelayUrl>,
        relay_url_last_success: Option<i64>,
        connect_timeout: Duration,
    ) -> Result<(ConnectionRef, bool), PoolError> {
        let _ = self.prune_closed_connections().await;

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
            connect_timeout,
        )
        .await
        .map_err(|e| match e {
            connect::ConnectError::Timeout => PoolError::Timeout,
            connect::ConnectError::Failed(msg) => PoolError::ConnectionFailed(msg),
        })?;

        let relay_url_confirmed = result.relay_url_confirmed;

        let conn_ref = self.cache_connection(node_id, result.connection).await?;
        Ok((conn_ref, relay_url_confirmed))
    }

    /// Try to get a cached connection without creating a new one.
    pub async fn try_get_cached(&self, node_id: EndpointId) -> Option<ConnectionRef> {
        let mut connections = self.connections.write().await;
        let mut lru = self.lru_order.write().await;

        if let Some(cached) = connections.get_mut(&node_id) {
            if cached.connection.close_reason().is_none() {
                cached.last_used = std::time::Instant::now();
                touch_lru(&mut lru, node_id);
                trace!(
                    alpn = ?std::str::from_utf8(self.alpn),
                    target = %node_id,
                    "reusing cached connection"
                );
                return Some(ConnectionRef {
                    connection: cached.connection.clone(),
                    node_id,
                    _drop_notify: None,
                });
            }
        }

        if connections.remove(&node_id).is_some() {
            remove_lru(&mut lru, node_id);
            trace!(
                alpn = ?std::str::from_utf8(self.alpn),
                target = %node_id,
                "removed closed cached connection"
            );
        }

        None
    }

    /// Cache an established connection.
    pub async fn cache_connection(
        &self,
        node_id: EndpointId,
        connection: Connection,
    ) -> Result<ConnectionRef, PoolError> {
        let _ = self.prune_closed_connections().await;

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

        {
            let mut connections = self.connections.write().await;
            let mut lru = self.lru_order.write().await;

            connections.insert(
                node_id,
                CachedConnection {
                    connection: connection.clone(),
                    last_used: std::time::Instant::now(),
                },
            );

            // Update LRU order
            touch_lru(&mut lru, node_id);
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

        // Find the oldest evictable connection.
        while let Some(oldest_id) = lru.pop_front() {
            let action = if let Some(cached) = connections.get(&oldest_id) {
                if cached.connection.close_reason().is_some() {
                    0u8
                } else if now.duration_since(cached.last_used) > self.config.idle_timeout {
                    1u8
                } else {
                    2u8
                }
            } else {
                3u8
            };

            match action {
                0 => {
                    connections.remove(&oldest_id);
                    trace!(
                        alpn = ?std::str::from_utf8(self.alpn),
                        target = %oldest_id,
                        "removed closed connection during eviction"
                    );
                }
                1 => {
                    if let Some(cached) = connections.remove(&oldest_id) {
                        trace!(
                            alpn = ?std::str::from_utf8(self.alpn),
                            target = %oldest_id,
                            "evicting idle connection"
                        );
                        cached.connection.close(0u32.into(), b"idle");
                    }
                    return;
                }
                2 => {
                    // Oldest connection is active; no idle connections remain.
                    lru.push_front(oldest_id);
                    return;
                }
                _ => {
                    // Stale LRU entry, continue scanning.
                }
            }
        }
    }

    async fn prune_closed_connections(&self) -> usize {
        let mut connections = self.connections.write().await;
        let mut lru = self.lru_order.write().await;

        let before = connections.len();
        connections.retain(|_, cached| cached.connection.close_reason().is_none());
        let removed = before.saturating_sub(connections.len());

        if removed > 0 {
            lru.retain(|id| connections.contains_key(id));
            trace!(
                alpn = ?std::str::from_utf8(self.alpn),
                removed,
                "pruned closed cached connections"
            );
        }

        removed
    }

    /// Close a specific connection
    pub async fn close(&self, node_id: EndpointId) {
        let mut connections = self.connections.write().await;
        let mut lru = self.lru_order.write().await;

        if let Some(cached) = connections.remove(&node_id) {
            cached.connection.close(0u32.into(), b"close");
        }
        remove_lru(&mut lru, node_id);
    }

    /// Get statistics about the pool
    pub async fn stats(&self) -> PoolStats {
        let connections = self.connections.read().await;
        let live_connections = connections
            .values()
            .filter(|cached| cached.connection.close_reason().is_none())
            .count();
        PoolStats {
            active_connections: live_connections,
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
    fn test_touch_lru_moves_existing_id_to_back() {
        let a = iroh::SecretKey::from_bytes(&[1u8; 32]).public();
        let b = iroh::SecretKey::from_bytes(&[2u8; 32]).public();
        let c = iroh::SecretKey::from_bytes(&[3u8; 32]).public();

        let mut lru = VecDeque::from([a, b, c]);
        touch_lru(&mut lru, b);

        let ordered: Vec<_> = lru.into_iter().collect();
        assert_eq!(ordered, vec![a, c, b]);
    }

    #[test]
    fn test_remove_lru_drops_all_matching_entries() {
        let a = iroh::SecretKey::from_bytes(&[1u8; 32]).public();
        let b = iroh::SecretKey::from_bytes(&[2u8; 32]).public();

        let mut lru = VecDeque::from([a, b, a]);
        remove_lru(&mut lru, a);

        let ordered: Vec<_> = lru.into_iter().collect();
        assert_eq!(ordered, vec![b]);
    }
}
