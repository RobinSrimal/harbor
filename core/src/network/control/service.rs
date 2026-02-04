//! Control Service - Lifecycle and relationship management
//!
//! Handles peer connections, topic invitations, and membership management.
//! One-off exchanges: open connection, send message, get response, close.
//! Does not maintain a connection pool.

use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, EndpointId};
use rusqlite::Connection as DbConnection;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use crate::data::dht::peer::get_peer_relay_info;
use crate::data::LocalIdentity;
use crate::network::connect;
use crate::protocol::ProtocolEvent;

use super::protocol::{ControlRpcProtocol, CONTROL_ALPN};

/// Error type for control operations
#[derive(Debug)]
pub enum ControlError {
    Connection(String),
    Rpc(String),
    Database(String),
    InvalidToken,
    PeerNotFound(String),
    NotAuthorized(String),
    RequestNotFound,
    InvalidState(String),
    TopicNotFound,
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::Connection(e) => write!(f, "connection failed: {}", e),
            ControlError::Rpc(e) => write!(f, "RPC error: {}", e),
            ControlError::Database(e) => write!(f, "database error: {}", e),
            ControlError::InvalidToken => write!(f, "invalid token"),
            ControlError::PeerNotFound(e) => write!(f, "peer not found: {}", e),
            ControlError::NotAuthorized(e) => write!(f, "not authorized: {}", e),
            ControlError::RequestNotFound => write!(f, "request not found"),
            ControlError::InvalidState(e) => write!(f, "invalid state: {}", e),
            ControlError::TopicNotFound => write!(f, "topic not found"),
        }
    }
}

impl std::error::Error for ControlError {}

/// Result type for control operations
pub type ControlResult<T> = Result<T, ControlError>;

/// Control service for lifecycle and relationship management
///
/// Handles:
/// - Peer connection requests (connect/accept/decline)
/// - Topic invitations and membership
/// - Key rotation (member removal)
/// - Peer introductions (suggest)
pub struct ControlService {
    /// Iroh endpoint for QUIC connections
    endpoint: Endpoint,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// Event sender for protocol events
    event_tx: mpsc::Sender<ProtocolEvent>,
}

impl std::fmt::Debug for ControlService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlService").finish_non_exhaustive()
    }
}

impl ControlService {
    /// Create a new ControlService
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
    ) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            identity,
            db,
            event_tx,
        })
    }

    /// Get our local endpoint ID
    pub fn local_id(&self) -> [u8; 32] {
        self.identity.public_key
    }

    /// Get the event sender
    pub fn event_tx(&self) -> &mpsc::Sender<ProtocolEvent> {
        &self.event_tx
    }

    /// Get a reference to the database
    pub fn db(&self) -> &Arc<Mutex<DbConnection>> {
        &self.db
    }

    /// Get a reference to the identity
    pub fn identity(&self) -> &Arc<LocalIdentity> {
        &self.identity
    }

    /// Get a reference to the endpoint
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Dial a peer on the control ALPN
    ///
    /// Opens a one-off connection for a control message exchange.
    pub(crate) async fn dial_peer(
        &self,
        peer_id: &[u8; 32],
        relay_url: Option<&str>,
    ) -> ControlResult<irpc::Client<ControlRpcProtocol>> {
        // Try to get relay info from DB if not provided
        let (parsed_relay, relay_last_success) = if let Some(url) = relay_url {
            (url.parse::<iroh::RelayUrl>().ok(), None)
        } else {
            let relay_info = {
                let db = self.db.lock().await;
                get_peer_relay_info(&db, peer_id)
                    .map_err(|e| ControlError::Database(e.to_string()))?
            };
            match relay_info {
                Some((url, last_success)) => (url.parse::<iroh::RelayUrl>().ok(), last_success),
                None => (None, None),
            }
        };

        let node_id = EndpointId::from_bytes(peer_id)
            .map_err(|e| ControlError::Connection(e.to_string()))?;

        let connect_result = connect::connect(
            &self.endpoint,
            node_id,
            parsed_relay.as_ref(),
            relay_last_success,
            CONTROL_ALPN,
            Duration::from_secs(10),
        )
        .await
        .map_err(|e| ControlError::Connection(e.to_string()))?;

        debug!(
            peer = %hex::encode(&peer_id[..8]),
            relay_confirmed = connect_result.relay_url_confirmed,
            "control connection established"
        );

        // Wrap connection for irpc client
        let rpc_client = irpc::Client::<ControlRpcProtocol>::boxed(
            crate::network::rpc::ExistingConnection::new(&connect_result.connection),
        );
        Ok(rpc_client)
    }

    // =========================================================================
    // Connection operations (implemented in outgoing.rs)
    // =========================================================================

    /// Request a connection to a peer
    ///
    /// Sends a ConnectRequest message. If a token is provided, it's for the
    /// QR code / invite string flow where the recipient auto-accepts valid tokens.
    pub async fn request_connection(
        &self,
        peer_id: &[u8; 32],
        relay_url: Option<&str>,
        display_name: Option<&str>,
        token: Option<[u8; 32]>,
    ) -> ControlResult<[u8; 32]> {
        super::outgoing::request_connection(self, peer_id, relay_url, display_name, token).await
    }

    /// Accept a connection request
    pub async fn accept_connection(
        &self,
        request_id: &[u8; 32],
    ) -> ControlResult<()> {
        super::outgoing::accept_connection(self, request_id).await
    }

    /// Decline a connection request
    pub async fn decline_connection(
        &self,
        request_id: &[u8; 32],
        reason: Option<&str>,
    ) -> ControlResult<()> {
        super::outgoing::decline_connection(self, request_id, reason).await
    }

    /// Generate a connect token (for QR code / invite string)
    pub async fn generate_connect_token(&self) -> ControlResult<ConnectInvite> {
        super::outgoing::generate_connect_token(self).await
    }

    /// Block a peer
    pub async fn block_peer(&self, peer_id: &[u8; 32]) -> ControlResult<()> {
        super::outgoing::block_peer(self, peer_id).await
    }

    /// Unblock a peer
    pub async fn unblock_peer(&self, peer_id: &[u8; 32]) -> ControlResult<()> {
        super::outgoing::unblock_peer(self, peer_id).await
    }

    // =========================================================================
    // Topic operations (implemented in outgoing.rs)
    // =========================================================================

    /// Invite a peer to a topic
    pub async fn invite_to_topic(
        &self,
        peer_id: &[u8; 32],
        topic_id: &[u8; 32],
    ) -> ControlResult<[u8; 32]> {
        super::outgoing::invite_to_topic(self, peer_id, topic_id).await
    }

    /// Accept a topic invite
    pub async fn accept_topic_invite(
        &self,
        message_id: &[u8; 32],
    ) -> ControlResult<()> {
        super::outgoing::accept_topic_invite(self, message_id).await
    }

    /// Decline a topic invite
    pub async fn decline_topic_invite(
        &self,
        message_id: &[u8; 32],
    ) -> ControlResult<()> {
        super::outgoing::decline_topic_invite(self, message_id).await
    }

    /// Leave a topic
    pub async fn leave_topic(&self, topic_id: &[u8; 32]) -> ControlResult<()> {
        super::outgoing::leave_topic(self, topic_id).await
    }

    /// Remove a member from a topic (admin only)
    ///
    /// Generates a new epoch key and sends it to all remaining members.
    pub async fn remove_topic_member(
        &self,
        topic_id: &[u8; 32],
        member_id: &[u8; 32],
    ) -> ControlResult<()> {
        super::outgoing::remove_topic_member(self, topic_id, member_id).await
    }

    // =========================================================================
    // Introduction operations (implemented in outgoing.rs)
    // =========================================================================

    /// Suggest a peer to another peer
    pub async fn suggest_peer(
        &self,
        to_peer: &[u8; 32],
        suggested_peer: &[u8; 32],
        note: Option<&str>,
    ) -> ControlResult<[u8; 32]> {
        super::outgoing::suggest_peer(self, to_peer, suggested_peer, note).await
    }
}

/// A connect invite (QR code / invite string data)
#[derive(Debug, Clone)]
pub struct ConnectInvite {
    /// Our endpoint ID
    pub endpoint_id: [u8; 32],
    /// Our relay URL
    pub relay_url: Option<String>,
    /// One-time secret token
    pub token: [u8; 32],
}

impl ConnectInvite {
    /// Encode as an invite string (format: endpoint_id:token:relay_url)
    pub fn encode(&self) -> String {
        let mut result = format!(
            "{}:{}",
            hex::encode(&self.endpoint_id),
            hex::encode(&self.token)
        );
        if let Some(ref url) = self.relay_url {
            result.push(':');
            result.push_str(url);
        }
        result
    }

    /// Decode from an invite string
    pub fn decode(invite: &str) -> Option<Self> {
        let parts: Vec<&str> = invite.splitn(3, ':').collect();
        if parts.len() < 2 {
            return None;
        }
        let endpoint_id = hex::decode(parts[0]).ok()?;
        let token = hex::decode(parts[1]).ok()?;
        if endpoint_id.len() != 32 || token.len() != 32 {
            return None;
        }
        let mut ep = [0u8; 32];
        let mut tk = [0u8; 32];
        ep.copy_from_slice(&endpoint_id);
        tk.copy_from_slice(&token);
        let relay_url = if parts.len() > 2 {
            Some(parts[2].to_string())
        } else {
            None
        };
        Some(Self {
            endpoint_id: ep,
            relay_url,
            token: tk,
        })
    }
}

/// Generate a random 32-byte ID
pub fn generate_id() -> [u8; 32] {
    use rand::RngCore;
    let mut id = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut id);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connect_invite_encode_decode() {
        let invite = ConnectInvite {
            endpoint_id: [1u8; 32],
            relay_url: Some("https://relay.example.com".to_string()),
            token: [2u8; 32],
        };

        let encoded = invite.encode();
        let decoded = ConnectInvite::decode(&encoded).unwrap();

        assert_eq!(decoded.endpoint_id, invite.endpoint_id);
        assert_eq!(decoded.token, invite.token);
        assert_eq!(decoded.relay_url, invite.relay_url);
    }

    #[test]
    fn test_connect_invite_no_relay() {
        let invite = ConnectInvite {
            endpoint_id: [1u8; 32],
            relay_url: None,
            token: [2u8; 32],
        };

        let encoded = invite.encode();
        let decoded = ConnectInvite::decode(&encoded).unwrap();

        assert_eq!(decoded.endpoint_id, invite.endpoint_id);
        assert_eq!(decoded.token, invite.token);
        assert!(decoded.relay_url.is_none());
    }

    #[test]
    fn test_connect_invite_invalid() {
        // Too short
        assert!(ConnectInvite::decode("AAAA").is_none());
        // Invalid base64
        assert!(ConnectInvite::decode("!!!invalid!!!").is_none());
    }
}
