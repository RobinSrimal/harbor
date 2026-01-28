//! Smart connection primitive with relay URL freshness logic.
//!
//! Decides whether to use a stored relay URL or let iroh's DNS discovery
//! resolve the peer's address, based on how recently the relay URL was
//! confirmed working.

use iroh::endpoint::{Connection, Endpoint};
use iroh::{EndpointAddr, EndpointId, RelayUrl};
use std::fmt;
use std::time::Duration;

use crate::data::dht::peer::current_timestamp;

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
