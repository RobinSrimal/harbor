//! Control Service - Lifecycle and relationship management
//!
//! Handles peer connections, topic invitations, and membership management.
//! One-off exchanges: open connection, send message, get response, close.
//! Does not maintain a connection pool.
//!
//! Business logic is split by domain:
//! - `peers.rs` — peer connection lifecycle
//! - `membership.rs` — topic invite/join/leave/remove
//! - `lifecycle.rs` — topic create/join/list/get_invite
//! - `introductions.rs` — suggest peer

use std::sync::{Arc, Mutex as StdMutex};
use rusqlite::Connection as DbConnection;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use crate::data::LocalIdentity;
use crate::data::send::store_outgoing_packet;
use crate::network::connect::Connector;
use crate::network::gate::ConnectionGate;
use crate::protocol::ProtocolEvent;
use crate::resilience::{PoWConfig, PoWResult, PoWVerifier, ProofOfWork, build_context};

use super::protocol::{ControlPacketType, ControlRpcProtocol};

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
    InsufficientPoW {
        /// Required difficulty bits
        required: u8,
    },
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
            ControlError::InsufficientPoW { required } => {
                write!(f, "insufficient proof of work: {} bits required", required)
            }
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
/// - Topic lifecycle (create, join, leave, list)
/// - Key rotation (member removal)
/// - Peer introductions (suggest)
pub struct ControlService {
    /// Connector for outgoing QUIC connections
    connector: Arc<Connector>,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// Event sender for protocol events
    event_tx: mpsc::Sender<ProtocolEvent>,
    /// Connection gate for notifying state changes (NOT for gating Control connections)
    connection_gate: Option<Arc<ConnectionGate>>,
    /// Proof of Work verifier for abuse prevention
    pow_verifier: StdMutex<PoWVerifier>,
}

impl std::fmt::Debug for ControlService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlService").finish_non_exhaustive()
    }
}

impl ControlService {
    /// Create a new ControlService
    pub fn new(
        connector: Arc<Connector>,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        connection_gate: Option<Arc<ConnectionGate>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            connector,
            identity,
            db,
            event_tx,
            connection_gate,
            pow_verifier: StdMutex::new(PoWVerifier::new(PoWConfig::control())),
        })
    }

    /// Get the connection gate (for notifying state changes)
    pub fn connection_gate(&self) -> Option<&Arc<ConnectionGate>> {
        self.connection_gate.as_ref()
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
    pub fn endpoint(&self) -> &iroh::Endpoint {
        self.connector.endpoint()
    }

    // === PoW Verification ===

    /// Verify Proof of Work for a control message
    ///
    /// Context for Control PoW: `sender_id || message_type_byte`
    /// Uses RequestBased scaling.
    pub fn verify_pow(
        &self,
        pow: &ProofOfWork,
        sender_id: &[u8; 32],
        message_type: ControlPacketType,
    ) -> Result<(), ControlError> {
        let context = build_context(&[sender_id, &[message_type as u8]]);
        let verifier = self.pow_verifier.lock().unwrap();

        match verifier.verify(pow, &context, sender_id, None) {
            PoWResult::Allowed => Ok(()),
            PoWResult::InsufficientDifficulty { required, .. } => {
                Err(ControlError::InsufficientPoW { required })
            }
            PoWResult::Expired { .. } => {
                let required = verifier.effective_difficulty(sender_id, None);
                Err(ControlError::InsufficientPoW { required })
            }
            PoWResult::InvalidContext => {
                let required = verifier.effective_difficulty(sender_id, None);
                Err(ControlError::InsufficientPoW { required })
            }
        }
    }

    /// Dial a peer on the control ALPN
    ///
    /// Opens a one-off connection for a control message exchange.
    pub(crate) async fn dial_peer(
        &self,
        peer_id: &[u8; 32],
    ) -> ControlResult<irpc::Client<ControlRpcProtocol>> {
        let conn = self.connector
            .connect(peer_id)
            .await
            .map_err(|e| ControlError::Connection(e.to_string()))?;

        let rpc_client = irpc::Client::<ControlRpcProtocol>::boxed(
            crate::network::rpc::ExistingConnection::new(&conn),
        );
        Ok(rpc_client)
    }

    /// Store a control packet for harbor replication
    pub(crate) async fn store_control_packet(
        &self,
        packet_id: &[u8; 32],
        harbor_id: &[u8; 32],
        recipients: &[[u8; 32]],
        packet_data: &[u8],
        packet_type: ControlPacketType,
    ) -> ControlResult<()> {
        let mut db = self.db.lock().await;

        // For point-to-point control packets, use harbor_id as topic_id
        // so the replication task seals them as DM packets (topic_id == harbor_id check).
        // For topic-scoped packets (TopicJoin/TopicLeave), harbor_id is hash(topic_id),
        // so they won't match and will be sealed as topic packets.
        let topic_id = *harbor_id;

        // Prepend type byte to packet data so receiver can identify Control packets.
        // DM packets use 0x80+ range, Control packets use their ControlPacketType value.
        let mut prefixed_data = Vec::with_capacity(1 + packet_data.len());
        prefixed_data.push(packet_type as u8);
        prefixed_data.extend_from_slice(packet_data);

        store_outgoing_packet(
            &mut db,
            packet_id,
            &topic_id,
            harbor_id,
            &prefixed_data,
            recipients,
            packet_type as u8,
        )
        .map_err(|e| ControlError::Database(e.to_string()))?;

        debug!(
            packet_id = %hex::encode(&packet_id[..8]),
            harbor_id = %hex::encode(&harbor_id[..8]),
            packet_type = ?packet_type,
            recipients = recipients.len(),
            "stored control packet for harbor replication"
        );

        Ok(())
    }
}

/// Compute PoW for a control message
/// Context: sender_id || message_type_byte
pub(super) fn compute_control_pow(sender_id: &[u8; 32], message_type: ControlPacketType) -> ControlResult<ProofOfWork> {
    let context = build_context(&[sender_id, &[message_type as u8]]);
    let difficulty = PoWConfig::control().base_difficulty;
    ProofOfWork::compute(&context, difficulty)
        .ok_or_else(|| ControlError::Rpc("failed to compute PoW".to_string()))
}

/// Verify sender_id matches the QUIC connection peer
pub(super) fn verify_sender(claimed_sender: &[u8; 32], quic_sender: &[u8; 32]) -> bool {
    claimed_sender == quic_sender
}

/// Generate a random 32-byte ID
pub fn generate_id() -> [u8; 32] {
    use rand::RngCore;
    let mut id = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut id);
    id
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
