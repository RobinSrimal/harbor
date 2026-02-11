//! Send Service - Core struct, types, and shared infrastructure
//!
//! Contains:
//! - `SendService` struct + constructor + accessors
//! - `SendConfig`, `SendOptions`, `SendError`, `SendResult`
//! - `ProcessError`, `ProcessResult`, `PacketSource`, `ReceiveError`
//! - PoW verification
//! - Connection management (`get_connection`, `close_connection`)
//! - Receipt processing
//!
//! Domain-specific logic lives in:
//! - `topic.rs` — topic send + process + handler
//! - `dm.rs` — DM send + process + handler

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use iroh::EndpointId;
use iroh::endpoint::Connection;
use rusqlite::Connection as DbConnection;
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::data::send::acknowledge_receipt as db_acknowledge_receipt;
use crate::data::LocalIdentity;
use crate::network::connect::Connector;
use crate::network::gate::ConnectionGate;
use crate::protocol::ProtocolEvent;
use crate::resilience::{ProofOfWork, PoWConfig, PoWResult, PoWVerifier, build_context};
use crate::security::PacketError;
use super::protocol::Receipt;

// =============================================================================
// Configuration
// =============================================================================

/// Configuration for the Send service
#[derive(Debug, Clone)]
pub struct SendConfig {
    /// Timeout for sending to a single recipient (default: 5 seconds)
    pub send_timeout: Duration,
    /// How long to wait for receipts before replicating to Harbor (default: 30 seconds)
    pub receipt_timeout: Duration,
    /// Maximum concurrent sends (default: 10)
    pub max_concurrent_sends: usize,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            send_timeout: Duration::from_secs(5),
            receipt_timeout: Duration::from_secs(30),
            max_concurrent_sends: 10,
        }
    }
}

// =============================================================================
// Send options / errors / results
// =============================================================================

/// Options controlling how a packet is sent and stored
#[derive(Debug, Clone)]
pub struct SendOptions {
    /// If true, skip storing in outgoing table (direct delivery only, no harbor replication)
    pub skip_harbor: bool,
    /// Override default harbor TTL. None = use default (PACKET_LIFETIME_SECS)
    pub ttl: Option<Duration>,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            skip_harbor: false,
            ttl: None,
        }
    }
}

impl SendOptions {
    /// Content packet with default harbor behavior
    pub fn content() -> Self {
        Self::default()
    }

    /// Direct-only delivery (no harbor storage)
    pub fn direct_only() -> Self {
        Self {
            skip_harbor: true,
            ttl: None,
        }
    }

    /// With a custom harbor TTL
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }
}

/// Error during Send operations
#[derive(Debug)]
pub enum SendError {
    /// Failed to create packet
    PacketCreation(PacketError),
    /// Failed to connect to recipient
    Connection(String),
    /// Failed to send data
    Send(String),
    /// Database error
    Database(String),
    /// No recipients
    NoRecipients,
    /// Topic not found
    TopicNotFound,
    /// Not a member of the topic
    NotMember,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::PacketCreation(e) => write!(f, "packet creation failed: {}", e),
            SendError::Connection(e) => write!(f, "connection failed: {}", e),
            SendError::Send(e) => write!(f, "send failed: {}", e),
            SendError::Database(e) => write!(f, "database error: {}", e),
            SendError::NoRecipients => write!(f, "no recipients specified"),
            SendError::TopicNotFound => write!(f, "topic not found"),
            SendError::NotMember => write!(f, "not a member of this topic"),
        }
    }
}

impl std::error::Error for SendError {}

impl From<PacketError> for SendError {
    fn from(e: PacketError) -> Self {
        SendError::PacketCreation(e)
    }
}

/// Result of sending a packet
#[derive(Debug, Clone)]
pub struct SendResult {
    /// The packet ID
    pub packet_id: [u8; 32],
    /// Recipients that were successfully reached
    pub delivered_to: Vec<[u8; 32]>,
    /// Recipients that failed
    pub failed: Vec<([u8; 32], String)>,
}

// =============================================================================
// Incoming processing types
// =============================================================================

/// Error during incoming packet processing
#[derive(Debug)]
pub enum ProcessError {
    /// Decryption/verification failed (try next topic)
    VerificationFailed(String),
    /// Database error
    Database(String),
    /// Invalid message format
    InvalidMessage(String),
    /// Sender validation failed (e.g., joiner != sender)
    SenderMismatch(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessError::VerificationFailed(e) => write!(f, "verification failed: {}", e),
            ProcessError::Database(e) => write!(f, "database error: {}", e),
            ProcessError::InvalidMessage(e) => write!(f, "invalid message: {}", e),
            ProcessError::SenderMismatch(e) => write!(f, "sender mismatch: {}", e),
        }
    }
}

impl std::error::Error for ProcessError {}

/// Source of an incoming packet — determines processing behavior
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketSource {
    /// Received via live QUIC connection (sender is online)
    Direct,
    /// Pulled from harbor storage (sender may be offline)
    HarborPull,
}

/// Result of processing an incoming packet
#[derive(Debug)]
pub struct ProcessResult {
    /// The receipt to send back
    pub receipt: Receipt,
    /// Whether the packet was a Content message (should be forwarded to app)
    pub content_payload: Option<Vec<u8>>,
}

/// Error during receive operations
#[derive(Debug)]
pub enum ReceiveError {
    /// Failed to verify/decrypt packet
    PacketVerification(PacketError),
}

impl std::fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReceiveError::PacketVerification(e) => write!(f, "packet verification failed: {}", e),
        }
    }
}

impl std::error::Error for ReceiveError {}

impl From<PacketError> for ReceiveError {
    fn from(e: PacketError) -> Self {
        ReceiveError::PacketVerification(e)
    }
}

// =============================================================================
// SendService
// =============================================================================

/// Send service - single entry point for all send operations
///
/// Owns connector, identity, and provides both outgoing
/// (send_to_topic) and incoming (handle_deliver_topic) operations.
impl std::fmt::Debug for SendService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendService").finish_non_exhaustive()
    }
}

pub struct SendService {
    /// Connector for outgoing QUIC connections
    connector: Arc<Connector>,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// Event sender (for incoming packet processing)
    event_tx: mpsc::Sender<ProtocolEvent>,
    /// Stream service for stream signaling routing (set after construction)
    stream_service: RwLock<Option<Arc<crate::network::stream::StreamService>>>,
    /// Connection gate for peer authorization
    connection_gate: Option<Arc<ConnectionGate>>,
    /// Proof of Work verifier for abuse prevention
    pow_verifier: StdMutex<PoWVerifier>,
}

impl SendService {
    /// Create a new Send service
    pub fn new(
        connector: Arc<Connector>,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        connection_gate: Option<Arc<ConnectionGate>>,
    ) -> Self {
        Self {
            connector,
            identity,
            db,
            event_tx,
            stream_service: RwLock::new(None),
            connection_gate,
            pow_verifier: StdMutex::new(PoWVerifier::new(PoWConfig::send())),
        }
    }

    /// Get the connection gate (for handler to check peer authorization)
    pub fn connection_gate(&self) -> Option<&Arc<ConnectionGate>> {
        self.connection_gate.as_ref()
    }

    /// Set the StreamService reference (called after both services are constructed)
    pub async fn set_stream_service(&self, stream: Arc<crate::network::stream::StreamService>) {
        *self.stream_service.write().await = Some(stream);
    }

    /// Get the StreamService reference (if set)
    pub(crate) async fn stream_service(&self) -> Option<Arc<crate::network::stream::StreamService>> {
        self.stream_service.read().await.clone()
    }

    /// Get our EndpointID
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.identity.public_key
    }

    /// Get the identity (for signing/encryption operations)
    pub fn identity(&self) -> &LocalIdentity {
        &self.identity
    }

    /// Get the event sender
    pub fn event_tx(&self) -> &mpsc::Sender<ProtocolEvent> {
        &self.event_tx
    }

    /// Get the database connection
    pub fn db(&self) -> &Arc<Mutex<DbConnection>> {
        &self.db
    }

    // ========== PoW Verification ==========

    /// Verify Proof of Work for a topic delivery
    ///
    /// Context for Send PoW (topics): `sender_id || harbor_id`
    /// Uses ByteBased scaling with gentle progression.
    pub fn verify_topic_pow(
        &self,
        pow: &ProofOfWork,
        sender_id: &[u8; 32],
        harbor_id: &[u8; 32],
        payload_len: usize,
    ) -> bool {
        let context = build_context(&[sender_id, harbor_id]);
        let verifier = self.pow_verifier.lock().unwrap();

        matches!(
            verifier.verify(pow, &context, sender_id, Some(payload_len as u64)),
            PoWResult::Allowed
        )
    }

    /// Verify Proof of Work for a DM delivery
    ///
    /// Context for Send PoW (DMs): `sender_id || recipient_id`
    /// Uses ByteBased scaling with gentle progression.
    pub fn verify_dm_pow(
        &self,
        pow: &ProofOfWork,
        sender_id: &[u8; 32],
        recipient_id: &[u8; 32],
        payload_len: usize,
    ) -> bool {
        let context = build_context(&[sender_id, recipient_id]);
        let verifier = self.pow_verifier.lock().unwrap();

        matches!(
            verifier.verify(pow, &context, sender_id, Some(payload_len as u64)),
            PoWResult::Allowed
        )
    }

    // ========== Connection Management ==========

    /// Get or create a connection to a node
    pub(crate) async fn get_connection(&self, node_id: EndpointId) -> Result<Connection, SendError> {
        let endpoint_id_bytes: [u8; 32] = *node_id.as_bytes();
        self.connector
            .connect(&endpoint_id_bytes)
            .await
            .map_err(|e| SendError::Connection(e.to_string()))
    }

    /// Close connection to a node
    pub async fn close_connection(&self, node_id: EndpointId) {
        let endpoint_id_bytes: [u8; 32] = *node_id.as_bytes();
        self.connector.close(&endpoint_id_bytes).await;
    }

    // ========== Receipt Processing ==========

    /// Process an incoming receipt - update tracking in database
    pub fn process_receipt(
        receipt: &Receipt,
        conn: &rusqlite::Connection,
    ) -> Result<bool, rusqlite::Error> {
        db_acknowledge_receipt(conn, &receipt.packet_id, &receipt.sender)
    }
}

/// Generate a random packet ID (32 bytes)
pub(super) fn generate_packet_id() -> [u8; 32] {
    let mut id = [0u8; 32];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut id);
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_all_tables;
    use crate::data::send::{store_outgoing_packet, get_packets_needing_replication};
    use crate::security::harbor_id_from_topic;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn setup_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_all_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_send_config_default() {
        let config = SendConfig::default();
        assert_eq!(config.send_timeout, Duration::from_secs(5));
        assert_eq!(config.receipt_timeout, Duration::from_secs(30));
        assert_eq!(config.max_concurrent_sends, 10);
    }

    #[test]
    fn test_send_error_display() {
        let err = SendError::NoRecipients;
        assert_eq!(err.to_string(), "no recipients specified");

        let err = SendError::Connection("test".to_string());
        assert_eq!(err.to_string(), "connection failed: test");

        let err = SendError::Send("stream closed".to_string());
        assert_eq!(err.to_string(), "send failed: stream closed");

        let err = SendError::Database("constraint failed".to_string());
        assert_eq!(err.to_string(), "database error: constraint failed");
    }

    #[test]
    fn test_send_error_from_packet_error() {
        let packet_err = PacketError::InvalidMac;
        let send_err: SendError = packet_err.into();
        assert!(matches!(send_err, SendError::PacketCreation(_)));
    }

    #[test]
    fn test_send_result_structure() {
        let result = SendResult {
            packet_id: [1u8; 32],
            delivered_to: vec![test_id(1), test_id(2)],
            failed: vec![(test_id(3), "timeout".to_string())],
        };

        assert_eq!(result.packet_id, [1u8; 32]);
        assert_eq!(result.delivered_to.len(), 2);
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].1, "timeout");
    }

    #[test]
    fn test_process_error_display() {
        let err = ProcessError::VerificationFailed("bad MAC".to_string());
        assert!(err.to_string().contains("verification failed"));

        let err = ProcessError::Database("constraint".to_string());
        assert!(err.to_string().contains("database error"));

        let err = ProcessError::InvalidMessage("bad format".to_string());
        assert!(err.to_string().contains("invalid message"));

        let err = ProcessError::SenderMismatch("wrong sender".to_string());
        assert!(err.to_string().contains("sender mismatch"));
    }

    #[test]
    fn test_process_receipt_success() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let packet_id = [42u8; 32];
        let recipient_id = test_id(2);

        store_outgoing_packet(
            &mut db_conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"packet data",
            &[recipient_id],
            0,
        ).unwrap();

        let receipt = Receipt::new(packet_id, recipient_id);
        let acked = SendService::process_receipt(&receipt, &db_conn).unwrap();
        assert!(acked);
    }

    #[test]
    fn test_process_receipt_already_acked() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let packet_id = [42u8; 32];
        let recipient_id = test_id(2);

        store_outgoing_packet(
            &mut db_conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"packet data",
            &[recipient_id],
            0,
        ).unwrap();

        let receipt = Receipt::new(packet_id, recipient_id);
        let acked1 = SendService::process_receipt(&receipt, &db_conn).unwrap();
        assert!(acked1);

        let acked2 = SendService::process_receipt(&receipt, &db_conn).unwrap();
        assert!(!acked2);
    }

    #[test]
    fn test_outgoing_packet_tracking() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let packet_id = [1u8; 32];
        let recipients = vec![test_id(10), test_id(11), test_id(12)];

        // Store packet
        store_outgoing_packet(
            &mut db_conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"data",
            &recipients,
            0, // Content
        ).unwrap();

        // Get packets needing replication (none acked yet)
        let needs_replication = get_packets_needing_replication(&db_conn).unwrap();
        assert_eq!(needs_replication.len(), 1);
    }
}
