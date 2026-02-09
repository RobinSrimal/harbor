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

use crate::data::dht::peer::{current_timestamp, get_peer_relay_info, update_peer_relay_url};

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
/// just call `connect_to_peer(peer_id, alpn, timeout)`.
#[derive(Clone)]
pub struct Connector {
    endpoint: Endpoint,
    db: Arc<Mutex<DbConnection>>,
}

impl Connector {
    /// Create a new Connector.
    pub fn new(endpoint: Endpoint, db: Arc<Mutex<DbConnection>>) -> Self {
        Self { endpoint, db }
    }

    /// Get the underlying endpoint for non-connect operations.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Connect to a peer by ID.
    ///
    /// Looks up the peer's relay URL from the database, uses it if fresh,
    /// otherwise falls back to DNS discovery. On success with a relay URL,
    /// updates the relay timestamp in the database.
    pub async fn connect_to_peer(
        &self,
        peer_id: &[u8; 32],
        alpn: &'static [u8],
        timeout: Duration,
    ) -> Result<Connection, ConnectError> {
        let node_id = EndpointId::from_bytes(peer_id)
            .map_err(|e| ConnectError::Failed(format!("invalid node id: {}", e)))?;

        // Look up relay info from DB
        let relay_info = {
            let db = self.db.lock().await;
            get_peer_relay_info(&db, peer_id).unwrap_or(None)
        };
        let (relay_url_str, relay_last_success) = match &relay_info {
            Some((url, ts)) => (Some(url.as_str()), *ts),
            None => (None, None),
        };
        let parsed_relay: Option<RelayUrl> = relay_url_str.and_then(|u| u.parse().ok());

        // Smart connect: relay-first if fresh, DNS fallback
        let result = connect(
            &self.endpoint,
            node_id,
            parsed_relay.as_ref(),
            relay_last_success,
            alpn,
            timeout,
        )
        .await?;

        // Update relay URL timestamp if confirmed working
        if result.relay_url_confirmed {
            if let Some(url) = &parsed_relay {
                let db = self.db.lock().await;
                let _ = update_peer_relay_url(
                    &db,
                    peer_id,
                    &url.to_string(),
                    current_timestamp(),
                );
            }
        }

        debug!(
            peer = %hex::encode(&peer_id[..8]),
            relay_confirmed = result.relay_url_confirmed,
            "connection established"
        );

        Ok(result.connection)
    }
}
