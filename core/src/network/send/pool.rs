//! Connection pool for Send protocol
//!
//! Provides connection pooling for the Send protocol (messages, receipts, sync updates/requests).
//! Uses the generic ConnectionPool with SEND_ALPN.

use iroh::{Endpoint, EndpointId, RelayUrl};

use crate::network::pool::{ConnectionPool, PoolConfig, PoolError, ConnectionRef};

// Re-export ALPN
pub use super::protocol::SEND_ALPN;

// Re-export common types from generic pool
pub use crate::network::pool::{
    PoolConfig as SendPoolConfig,
    PoolError as SendPoolError,
    ConnectionRef as SendConnectionRef,
    PoolStats as SendPoolStats,
};

/// Send protocol connection pool
#[derive(Clone)]
pub struct SendPool {
    /// Generic connection pool
    pool: ConnectionPool,
}

impl SendPool {
    /// Create a new Send connection pool
    pub fn new(endpoint: Endpoint, config: PoolConfig) -> Self {
        Self {
            pool: ConnectionPool::new(endpoint, SEND_ALPN, config),
        }
    }

    /// Get or create a connection to a peer
    pub async fn get_connection(
        &self,
        node_id: EndpointId,
        relay_url: Option<&RelayUrl>,
        relay_url_last_success: Option<i64>,
    ) -> Result<(ConnectionRef, bool), PoolError> {
        self.pool.get_connection(node_id, relay_url, relay_url_last_success).await
    }

    /// Close a specific connection
    pub async fn close(&self, node_id: EndpointId) {
        self.pool.close(node_id).await;
    }

    /// Get statistics about the pool
    pub async fn stats(&self) -> SendPoolStats {
        self.pool.stats().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
