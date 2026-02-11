//! Smart connection primitive with relay URL freshness logic.
//!
//! Decides whether to use a stored relay URL or let iroh's DNS discovery
//! resolve the peer's address, based on how recently the relay URL was
//! confirmed working.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, Endpoint};
use iroh::{EndpointAddr, EndpointId, RelayUrl};
use rusqlite::Connection as DbConnection;
use tokio::sync::Mutex;
use tracing::debug;

use crate::data::dht::peer::{current_timestamp, get_peer, update_peer_relay_url};
use crate::network::pool::{ConnectionPool, PoolConfig, PoolError};

/// How long a relay URL is considered fresh (1 week).
/// If the relay URL was last confirmed working within this period,
/// we use it directly. Otherwise we skip it and let DNS resolve.
pub const RELAY_URL_FRESHNESS_SECS: i64 = 7 * 24 * 60 * 60;

/// Result of a smart connection attempt.
pub struct ConnectResult {
    /// The established connection.
    pub connection: Connection,
    /// Whether the relay URL was used and confirmed working.
    /// When true, the caller should update `relay_url_last_success` in the peers table.
    pub relay_url_confirmed: bool,
}

/// Error type for connection attempts.
#[derive(Debug)]
pub enum ConnectError {
    /// Connection timed out (all attempts exhausted).
    Timeout,
    /// Connection failed with an error message.
    Failed(String),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Timeout => write!(f, "connection timed out"),
            ConnectError::Failed(msg) => write!(f, "connection failed: {}", msg),
        }
    }
}

impl std::error::Error for ConnectError {}

/// Smart connect: uses relay URL if fresh, falls back to DNS discovery.
///
/// Logic:
/// - If `relay_url` is `Some` and `relay_url_last_success` is within
///   [`RELAY_URL_FRESHNESS_SECS`]: try connecting with the relay URL first
///   (using half the timeout), then fall back to DNS on failure.
/// - If relay URL is stale or missing: connect with just the endpoint ID,
///   letting iroh's DNS discovery resolve the address (full timeout).
pub async fn connect(
    endpoint: &Endpoint,
    node_id: EndpointId,
    relay_url: Option<&RelayUrl>,
    relay_url_last_success: Option<i64>,
    alpn: &'static [u8],
    timeout: Duration,
) -> Result<ConnectResult, ConnectError> {
    let relay_is_fresh = is_relay_fresh(relay_url, relay_url_last_success);

    if relay_is_fresh {
        // Relay URL is fresh — try it first with half the timeout
        let relay = relay_url.expect("relay_url must be Some when fresh");
        let half_timeout = timeout / 2;

        let addr = EndpointAddr::new(node_id).with_relay_url(relay.clone());
        match tokio::time::timeout(half_timeout, endpoint.connect(addr, alpn)).await {
            Ok(Ok(conn)) => {
                return Ok(ConnectResult {
                    connection: conn,
                    relay_url_confirmed: true,
                });
            }
            _ => {
                // Relay attempt failed or timed out — fall back to DNS
                let addr = EndpointAddr::from(node_id);
                let conn = tokio::time::timeout(half_timeout, endpoint.connect(addr, alpn))
                    .await
                    .map_err(|_| ConnectError::Timeout)?
                    .map_err(|e| ConnectError::Failed(e.to_string()))?;

                return Ok(ConnectResult {
                    connection: conn,
                    relay_url_confirmed: false,
                });
            }
        }
    }

    // No relay URL or stale — use DNS discovery with full timeout
    let addr = EndpointAddr::from(node_id);
    let conn = tokio::time::timeout(timeout, endpoint.connect(addr, alpn))
        .await
        .map_err(|_| ConnectError::Timeout)?
        .map_err(|e| ConnectError::Failed(e.to_string()))?;

    Ok(ConnectResult {
        connection: conn,
        relay_url_confirmed: false,
    })
}

/// Check if a relay URL is fresh enough to use directly.
fn is_relay_fresh(relay_url: Option<&RelayUrl>, last_success: Option<i64>) -> bool {
    match (relay_url, last_success) {
        (Some(_), Some(ts)) => {
            let now = current_timestamp();
            now - ts < RELAY_URL_FRESHNESS_SECS
        }
        _ => false,
    }
}

/// Smart connector that handles relay URL lookup and connection establishment.
///
/// Centralizes the relay-lookup → connect → relay-confirm pattern so services
/// just call `connect(peer_id)`. Optionally uses a connection pool.
#[derive(Clone)]
pub struct Connector {
    endpoint: Endpoint,
    db: Arc<Mutex<DbConnection>>,
    alpn: &'static [u8],
    connect_timeout: Duration,
    pool: Option<ConnectionPool>,
}

impl Connector {
    /// Create a new Connector.
    pub fn new(
        endpoint: Endpoint,
        db: Arc<Mutex<DbConnection>>,
        alpn: &'static [u8],
        connect_timeout: Duration,
        pool_config: Option<PoolConfig>,
    ) -> Self {
        let pool = pool_config.map(|config| ConnectionPool::new(endpoint.clone(), alpn, config));
        Self {
            endpoint,
            db,
            alpn,
            connect_timeout,
            pool,
        }
    }

    /// Get the underlying endpoint for non-connect operations.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Get connection pool stats if pooling is enabled.
    pub async fn pool_stats(&self) -> Option<crate::network::pool::PoolStats> {
        match &self.pool {
            Some(pool) => Some(pool.stats().await),
            None => None,
        }
    }

    /// Connect to a peer by ID using the default timeout.
    pub async fn connect(&self, peer_id: &[u8; 32]) -> Result<Connection, ConnectError> {
        self.connect_with_timeout(peer_id, self.connect_timeout).await
    }

    /// Connect to a peer by ID with a custom timeout.
    ///
    /// Looks up the peer's relay URL from the database, uses it if fresh,
    /// otherwise falls back to DNS discovery. On success with a relay URL,
    /// updates the relay timestamp in the database.
    pub async fn connect_with_timeout(
        &self,
        peer_id: &[u8; 32],
        timeout: Duration,
    ) -> Result<Connection, ConnectError> {
        let node_id = EndpointId::from_bytes(peer_id)
            .map_err(|e| ConnectError::Failed(format!("invalid node id: {}", e)))?;

        if let Some(cached) = self.try_get_cached(node_id).await {
            return Ok(cached);
        }

        let (relay_url, relay_last_success) = self.load_peer_relay_info(peer_id).await;

        let result = self
            .connect_internal(node_id, relay_url.as_ref(), relay_last_success, timeout)
            .await?;

        self.update_relay_timestamp(peer_id, relay_url.as_ref(), result.relay_url_confirmed)
            .await;

        debug!(
            peer = %hex::encode(&peer_id[..8]),
            relay_confirmed = result.relay_url_confirmed,
            "connection established"
        );

        Ok(result.connection)
    }

    /// Connect to a peer using an optional relay hint.
    ///
    /// If the hint is `None`, falls back to relay info from the database.
    /// Returns whether the relay URL was confirmed working.
    pub async fn connect_with_relay_hint(
        &self,
        peer_id: &[u8; 32],
        relay_hint: Option<&RelayUrl>,
        relay_hint_last_success: Option<i64>,
    ) -> Result<ConnectResult, ConnectError> {
        self.connect_with_relay_hint_timeout(
            peer_id,
            relay_hint,
            relay_hint_last_success,
            self.connect_timeout,
        )
        .await
    }

    /// Connect to a peer using an optional relay hint with a custom timeout.
    ///
    /// If the hint is `None`, falls back to relay info from the database.
    /// Returns whether the relay URL was confirmed working.
    pub async fn connect_with_relay_hint_timeout(
        &self,
        peer_id: &[u8; 32],
        relay_hint: Option<&RelayUrl>,
        relay_hint_last_success: Option<i64>,
        timeout: Duration,
    ) -> Result<ConnectResult, ConnectError> {
        let node_id = EndpointId::from_bytes(peer_id)
            .map_err(|e| ConnectError::Failed(format!("invalid node id: {}", e)))?;

        if let Some(cached) = self.try_get_cached(node_id).await {
            return Ok(ConnectResult {
                connection: cached,
                relay_url_confirmed: false,
            });
        }

        let (db_relay_url, db_relay_last_success) = self.load_peer_relay_info(peer_id).await;

        let (relay_url, relay_last_success) = match relay_hint {
            Some(hint) => {
                let hint_last_success = relay_hint_last_success.or_else(|| {
                    db_relay_url
                        .as_ref()
                        .and_then(|db_url| if db_url == hint { db_relay_last_success } else { None })
                });
                (Some(hint.clone()), hint_last_success)
            }
            None => (db_relay_url, db_relay_last_success),
        };

        let result = self
            .connect_internal(node_id, relay_url.as_ref(), relay_last_success, timeout)
            .await?;

        self.update_relay_timestamp(peer_id, relay_url.as_ref(), result.relay_url_confirmed)
            .await;

        debug!(
            peer = %hex::encode(&peer_id[..8]),
            relay_confirmed = result.relay_url_confirmed,
            "connection established"
        );

        Ok(result)
    }

    /// Close a pooled connection if present.
    pub async fn close(&self, peer_id: &[u8; 32]) {
        let Some(pool) = &self.pool else {
            return;
        };

        if let Ok(node_id) = EndpointId::from_bytes(peer_id) {
            pool.close(node_id).await;
        }
    }

    async fn try_get_cached(&self, node_id: EndpointId) -> Option<Connection> {
        let pool = self.pool.as_ref()?;
        pool.try_get_cached(node_id)
            .await
            .map(|cached| cached.connection().clone())
    }

    async fn connect_internal(
        &self,
        node_id: EndpointId,
        relay_url: Option<&RelayUrl>,
        relay_url_last_success: Option<i64>,
        timeout: Duration,
    ) -> Result<ConnectResult, ConnectError> {
        if let Some(pool) = &self.pool {
            let (conn_ref, relay_confirmed) = pool
                .get_connection(node_id, relay_url, relay_url_last_success)
                .await
                .map_err(pool_error_to_connect_error)?;
            Ok(ConnectResult {
                connection: conn_ref.connection().clone(),
                relay_url_confirmed: relay_confirmed,
            })
        } else {
            connect(
                &self.endpoint,
                node_id,
                relay_url,
                relay_url_last_success,
                self.alpn,
                timeout,
            )
            .await
        }
    }

    async fn load_peer_relay_info(
        &self,
        peer_id: &[u8; 32],
    ) -> (Option<RelayUrl>, Option<i64>) {
        let db = self.db.lock().await;
        match get_peer(&db, peer_id).unwrap_or(None) {
            Some(peer) => {
                let relay_url = peer.relay_url.as_deref().and_then(|u| u.parse().ok());
                (relay_url, peer.relay_url_last_success)
            }
            None => (None, None),
        }
    }

    async fn update_relay_timestamp(
        &self,
        peer_id: &[u8; 32],
        relay_url: Option<&RelayUrl>,
        relay_confirmed: bool,
    ) {
        if relay_confirmed {
            if let Some(url) = relay_url {
                let db = self.db.lock().await;
                let _ = update_peer_relay_url(
                    &db,
                    peer_id,
                    &url.to_string(),
                    current_timestamp(),
                );
            }
        }
    }
}

fn pool_error_to_connect_error(err: PoolError) -> ConnectError {
    match err {
        PoolError::Timeout => ConnectError::Timeout,
        PoolError::ConnectionFailed(msg) => ConnectError::Failed(msg),
        other => ConnectError::Failed(other.to_string()),
    }
}
