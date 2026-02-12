//! Sync Service - handles CRDT sync transport
//!
//! The SyncService handles:
//! - Incoming SYNC_ALPN connections (sync requests/responses)
//! - Outgoing sync updates (via SendService broadcast)
//! - Outgoing sync requests (via SendService broadcast)
//! - Outgoing sync responses (via direct SYNC_ALPN connection)

use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection as DbConnection;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};

use crate::data::membership::get_topic_by_harbor_id;
use crate::data::{LocalIdentity, get_topic_members};
use crate::network::connect::Connector;
use crate::network::gate::ConnectionGate;
use crate::network::packet::{DmMessage, SyncUpdateMessage, TopicMessage};
use crate::network::send::{SendOptions, SendService};
use crate::protocol::{
    DmSyncResponseEvent, MemberInfo, ProtocolError, ProtocolEvent, SyncRequestEvent,
    SyncResponseEvent,
};

/// Message type bytes for sync protocol
pub mod sync_message_type {
    /// SyncRequest - peer is requesting our state
    pub const SYNC_REQUEST: u8 = 0x01;
    /// SyncResponse - peer is providing their state
    pub const SYNC_RESPONSE: u8 = 0x02;
    /// DmSyncResponse - peer is providing their DM sync state
    pub const DM_SYNC_RESPONSE: u8 = 0x03;
}

/// Error during sync message processing
#[derive(Debug)]
pub enum ProcessSyncError {
    /// Message too short (need at least 33 bytes: 32 context + 1 type)
    MessageTooShort(usize),
    /// Empty message payload after topic_id
    EmptyPayload,
    /// Unknown message type
    UnknownMessageType(u8),
    /// Unknown topic for harbor_id
    UnknownTopic([u8; 32]),
    /// Membership proof failed verification
    InvalidMembershipProof,
}

impl std::fmt::Display for ProcessSyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessSyncError::MessageTooShort(len) => {
                write!(f, "message too short: {} bytes (need at least 33)", len)
            }
            ProcessSyncError::EmptyPayload => write!(f, "empty message payload"),
            ProcessSyncError::UnknownMessageType(t) => {
                write!(f, "unknown message type: 0x{:02x}", t)
            }
            ProcessSyncError::UnknownTopic(h) => {
                write!(f, "unknown topic for harbor_id: {}", hex::encode(&h[..8]))
            }
            ProcessSyncError::InvalidMembershipProof => write!(f, "invalid membership proof"),
        }
    }
}

impl std::error::Error for ProcessSyncError {}

/// Configuration for the Sync service
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Timeout for sync response connections (default: 30 seconds)
    pub connect_timeout: Duration,
    /// Maximum sync response size (default: 100MB)
    pub max_response_size: usize,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(30),
            max_response_size: 100 * 1024 * 1024, // 100MB
        }
    }
}

/// Maximum message size for send protocol (512 KB)
const MAX_MESSAGE_SIZE: usize = 512 * 1024;

/// Sync service - handles CRDT sync transport
///
/// Owns incoming SYNC_ALPN handling and outgoing sync operations.
/// Uses SendService for broadcasting updates/requests via the send protocol.
impl std::fmt::Debug for SyncService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncService").finish_non_exhaustive()
    }
}

pub struct SyncService {
    /// Connector for outgoing SYNC_ALPN connections
    connector: Arc<Connector>,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// Event sender (for incoming sync events)
    event_tx: mpsc::Sender<ProtocolEvent>,
    /// Send service (for broadcasting via send protocol)
    send_service: Arc<SendService>,
    /// Connection gate for peer authorization
    connection_gate: Option<Arc<ConnectionGate>>,
    /// Configuration
    config: SyncConfig,
}

impl SyncService {
    /// Create a new Sync service
    pub fn new(
        connector: Arc<Connector>,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        send_service: Arc<SendService>,
        connection_gate: Option<Arc<ConnectionGate>>,
    ) -> Self {
        Self {
            connector,
            identity,
            db,
            event_tx,
            send_service,
            connection_gate,
            config: SyncConfig::default(),
        }
    }

    /// Get the connection gate (for handler to check peer authorization)
    pub fn connection_gate(&self) -> Option<&Arc<ConnectionGate>> {
        self.connection_gate.as_ref()
    }

    // ========== Incoming ==========
    // handle_sync_connection is in handlers/sync.rs (impl SyncService)

    /// Get the sync config (used by incoming handler)
    pub(crate) fn config(&self) -> &SyncConfig {
        &self.config
    }

    // ========== Outgoing ==========

    /// Send a sync update to all topic members
    ///
    /// The `data` bytes are opaque to Harbor - application provides
    /// serialized CRDT updates (Loro delta, Yjs update, etc.)
    ///
    /// Max size: 512 KB (uses broadcast via send protocol)
    pub async fn send_sync_update(
        &self,
        topic_id: &[u8; 32],
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        // Check size limit
        if data.len() > MAX_MESSAGE_SIZE {
            return Err(ProtocolError::MessageTooLarge);
        }

        // Get topic members
        let members = {
            let db = self.db.lock().await;
            get_topic_members(&db, topic_id).map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        if members.is_empty() {
            return Err(ProtocolError::TopicNotFound);
        }

        let our_id = self.identity.public_key;

        // Get recipients (all members except us)
        let recipients: Vec<MemberInfo> = members
            .into_iter()
            .filter(|m| *m != our_id)
            .map(|endpoint_id| MemberInfo::new(endpoint_id))
            .collect();

        if recipients.is_empty() {
            return Ok(()); // No one to send to
        }

        // Encode as SyncUpdate message
        let msg = TopicMessage::SyncUpdate(SyncUpdateMessage { data });
        let encoded = msg.encode();

        // Broadcast to all members via SendService
        self.send_service
            .send_to_topic(topic_id, &encoded, &recipients, SendOptions::content())
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
            .map(|_| ())
    }

    /// Request sync state from topic members
    ///
    /// Broadcasts a sync request to all members. When peers respond,
    /// you'll receive `ProtocolEvent::SyncResponse` events.
    pub async fn request_sync(&self, topic_id: &[u8; 32]) -> Result<(), ProtocolError> {
        // Get topic members
        let members = {
            let db = self.db.lock().await;
            get_topic_members(&db, topic_id).map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        if members.is_empty() {
            return Err(ProtocolError::TopicNotFound);
        }

        let our_id = self.identity.public_key;

        // Get recipients (all members except us)
        let recipients: Vec<MemberInfo> = members
            .into_iter()
            .filter(|m| *m != our_id)
            .map(|endpoint_id| MemberInfo::new(endpoint_id))
            .collect();

        if recipients.is_empty() {
            return Ok(()); // No one to request from
        }

        // Encode as SyncRequest message (no payload)
        let msg = TopicMessage::SyncRequest;
        let encoded = msg.encode();

        info!(
            topic = %hex::encode(&topic_id[..8]),
            recipients = recipients.len(),
            "Broadcasting sync request"
        );

        // Broadcast to all members via SendService
        self.send_service
            .send_to_topic(topic_id, &encoded, &recipients, SendOptions::content())
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
            .map(|_| ())
    }

    /// Respond to a sync request with current state
    ///
    /// Call this when you receive `ProtocolEvent::SyncRequest`.
    /// Uses direct SYNC_ALPN connection - no size limit.
    pub async fn respond_sync(
        &self,
        topic_id: &[u8; 32],
        requester_id: &[u8; 32],
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        // Build SYNC_ALPN message
        let message = encode_sync_response(topic_id, &self.identity.public_key, &data);

        // Connect to requester via SYNC_ALPN
        let conn = self
            .connector
            .connect_with_timeout(requester_id, self.config.connect_timeout)
            .await
            .map_err(|e| ProtocolError::Network(format!("connection failed: {}", e)))?;

        // Open unidirectional stream and send
        let mut send = conn
            .open_uni()
            .await
            .map_err(|e| ProtocolError::Network(format!("failed to open stream: {}", e)))?;

        tokio::io::AsyncWriteExt::write_all(&mut send, &message)
            .await
            .map_err(|e| ProtocolError::Network(format!("failed to write: {}", e)))?;

        // Finish the stream
        send.finish()
            .map_err(|e| ProtocolError::Network(format!("failed to finish: {}", e)))?;

        // Wait for the stream to actually be transmitted
        send.stopped()
            .await
            .map_err(|e| ProtocolError::Network(format!("stream error: {}", e)))?;

        info!(
            topic = %hex::encode(&topic_id[..8]),
            requester = %hex::encode(&requester_id[..8]),
            size = data.len(),
            "Sent sync response via SYNC_ALPN"
        );

        Ok(())
    }

    // ========== DM Sync Outgoing ==========

    /// Send a DM sync update to a single peer
    ///
    /// Max size: 512 KB (uses DM via send protocol)
    pub async fn send_dm_sync_update(
        &self,
        recipient_id: &[u8; 32],
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        if data.len() > MAX_MESSAGE_SIZE {
            return Err(ProtocolError::MessageTooLarge);
        }

        let encoded = DmMessage::SyncUpdate(SyncUpdateMessage { data }).encode();

        self.send_service
            .send_to_dm(recipient_id, &encoded)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Request DM sync state from a peer
    ///
    /// When the peer responds, you'll receive a `ProtocolEvent::DmSyncResponse` event.
    pub async fn request_dm_sync(&self, recipient_id: &[u8; 32]) -> Result<(), ProtocolError> {
        let encoded = DmMessage::SyncRequest.encode();

        info!(
            recipient = %hex::encode(&recipient_id[..8]),
            "Sending DM sync request"
        );

        self.send_service
            .send_to_dm(recipient_id, &encoded)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
    }

    /// Respond to a DM sync request with current state
    ///
    /// Call this when you receive `ProtocolEvent::DmSyncRequest`.
    /// Uses direct SYNC_ALPN connection - no size limit.
    pub async fn respond_dm_sync(
        &self,
        recipient_id: &[u8; 32],
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        let message = encode_dm_sync_response(&self.identity.public_key, &data);

        let conn = self
            .connector
            .connect_with_timeout(recipient_id, self.config.connect_timeout)
            .await
            .map_err(|e| ProtocolError::Network(format!("connection failed: {}", e)))?;

        let mut send = conn
            .open_uni()
            .await
            .map_err(|e| ProtocolError::Network(format!("failed to open stream: {}", e)))?;

        tokio::io::AsyncWriteExt::write_all(&mut send, &message)
            .await
            .map_err(|e| ProtocolError::Network(format!("failed to write: {}", e)))?;

        send.finish()
            .map_err(|e| ProtocolError::Network(format!("failed to finish: {}", e)))?;

        send.stopped()
            .await
            .map_err(|e| ProtocolError::Network(format!("stream error: {}", e)))?;

        info!(
            recipient = %hex::encode(&recipient_id[..8]),
            size = data.len(),
            "Sent DM sync response via SYNC_ALPN"
        );

        Ok(())
    }
}

// ========== Free functions (kept for backward compat and tests) ==========

/// Encode a sync response message for transmission via SYNC_ALPN
///
/// Format: [32 bytes harbor_id][1 byte type = 0x02][32 bytes membership_proof][data]
pub fn encode_sync_response(topic_id: &[u8; 32], sender_id: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let harbor_id = crate::security::harbor_id_from_topic(topic_id);
    let membership_proof =
        crate::network::membership::create_membership_proof(topic_id, &harbor_id, sender_id);

    let mut message = Vec::with_capacity(32 + 1 + 32 + data.len());
    message.extend_from_slice(&harbor_id);
    message.push(sync_message_type::SYNC_RESPONSE);
    message.extend_from_slice(&membership_proof);
    message.extend_from_slice(data);
    message
}

/// Encode a DM sync response message for transmission via SYNC_ALPN
///
/// Format: [32 bytes sender_peer_id][1 byte type = 0x03][data]
pub fn encode_dm_sync_response(sender_id: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(32 + 1 + data.len());
    message.extend_from_slice(sender_id);
    message.push(sync_message_type::DM_SYNC_RESPONSE);
    message.extend_from_slice(data);
    message
}

impl SyncService {
    /// Process an incoming sync message - handles all business logic
    pub async fn process_incoming_message(
        &self,
        buf: &[u8],
        sender_id: [u8; 32],
    ) -> Result<(), ProcessSyncError> {
        Self::process_sync_message(buf, sender_id, &self.db, &self.event_tx).await
    }

    /// Core sync message processing logic (static for testability)
    pub(crate) async fn process_sync_message(
        buf: &[u8],
        sender_id: [u8; 32],
        db: &Arc<Mutex<DbConnection>>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
    ) -> Result<(), ProcessSyncError> {
        // Check minimum size: 32 bytes harbor_id + at least 1 byte message type
        if buf.len() < 33 {
            warn!(
                len = buf.len(),
                sender = %hex::encode(&sender_id[..8]),
                "SYNC_SERVICE: message too short"
            );
            return Err(ProcessSyncError::MessageTooShort(buf.len()));
        }

        // First 32 bytes are the context id (harbor_id for topic sync)
        let mut context_id = [0u8; 32];
        context_id.copy_from_slice(&buf[..32]);
        let message_bytes = &buf[32..];

        info!(
            context = %hex::encode(&context_id[..8]),
            sender = %hex::encode(&sender_id[..8]),
            message_len = message_bytes.len(),
            "SYNC_SERVICE: received message"
        );

        // Check for empty payload
        if message_bytes.is_empty() {
            warn!(
                context = %hex::encode(&context_id[..8]),
                sender = %hex::encode(&sender_id[..8]),
                "SYNC_SERVICE: empty message payload"
            );
            return Err(ProcessSyncError::EmptyPayload);
        }

        // Message type byte determines what kind of message this is
        let event = match message_bytes[0] {
            sync_message_type::SYNC_REQUEST => {
                // Needs membership proof
                if message_bytes.len() < 33 {
                    return Err(ProcessSyncError::MessageTooShort(buf.len()));
                }

                let mut proof = [0u8; 32];
                proof.copy_from_slice(&message_bytes[1..33]);

                // Resolve topic_id from harbor_id
                let topic_id = {
                    let db = db.lock().await;
                    let topic = get_topic_by_harbor_id(&db, &context_id)
                        .map_err(|_| ProcessSyncError::UnknownTopic(context_id))?;
                    topic
                        .map(|t| t.topic_id)
                        .ok_or(ProcessSyncError::UnknownTopic(context_id))?
                };

                // Verify membership proof
                if !crate::network::membership::verify_membership_proof(
                    &topic_id,
                    &context_id,
                    &sender_id,
                    &proof,
                ) {
                    return Err(ProcessSyncError::InvalidMembershipProof);
                }

                info!(
                    topic = %hex::encode(&topic_id[..8]),
                    sender = %hex::encode(&sender_id[..8]),
                    "SYNC_SERVICE: received SyncRequest"
                );
                ProtocolEvent::SyncRequest(SyncRequestEvent {
                    topic_id,
                    sender_id,
                })
            }
            sync_message_type::SYNC_RESPONSE => {
                if message_bytes.len() < 33 {
                    return Err(ProcessSyncError::MessageTooShort(buf.len()));
                }

                let mut proof = [0u8; 32];
                proof.copy_from_slice(&message_bytes[1..33]);
                let data = message_bytes[33..].to_vec();

                // Resolve topic_id from harbor_id
                let topic_id = {
                    let db = db.lock().await;
                    let topic = get_topic_by_harbor_id(&db, &context_id)
                        .map_err(|_| ProcessSyncError::UnknownTopic(context_id))?;
                    topic
                        .map(|t| t.topic_id)
                        .ok_or(ProcessSyncError::UnknownTopic(context_id))?
                };

                // Verify membership proof
                if !crate::network::membership::verify_membership_proof(
                    &topic_id,
                    &context_id,
                    &sender_id,
                    &proof,
                ) {
                    return Err(ProcessSyncError::InvalidMembershipProof);
                }

                info!(
                    topic = %hex::encode(&topic_id[..8]),
                    sender = %hex::encode(&sender_id[..8]),
                    data_len = data.len(),
                    "SYNC_SERVICE: received SyncResponse"
                );
                ProtocolEvent::SyncResponse(SyncResponseEvent { topic_id, data })
            }
            sync_message_type::DM_SYNC_RESPONSE => {
                // For DM sync responses, the 32-byte prefix is the sender's peer_id
                // (not a harbor_id). sender_id from the QUIC connection is the authoritative source.
                let data = message_bytes[1..].to_vec();

                info!(
                    sender = %hex::encode(&sender_id[..8]),
                    data_len = data.len(),
                    "SYNC_SERVICE: received DmSyncResponse"
                );
                ProtocolEvent::DmSyncResponse(DmSyncResponseEvent { sender_id, data })
            }
            other => {
                debug!(
                    context = %hex::encode(&context_id[..8]),
                    first_byte = other,
                    "SYNC_SERVICE: unknown message type"
                );
                return Err(ProcessSyncError::UnknownMessageType(other));
            }
        };

        // Emit the event
        if event_tx.send(event).await.is_err() {
            debug!("event receiver dropped");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn test_sender() -> [u8; 32] {
        [1u8; 32]
    }

    fn test_topic() -> [u8; 32] {
        [2u8; 32]
    }

    async fn test_db_with_topic() -> Arc<Mutex<DbConnection>> {
        let conn = crate::data::start_memory_db().unwrap();
        // Use sender as admin for simplicity
        crate::data::subscribe_topic_with_admin(&conn, &test_topic(), &test_sender()).unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[tokio::test]
    async fn test_process_sync_request() {
        let (tx, mut rx) = mpsc::channel(10);
        let db = test_db_with_topic().await;

        let mut buf = Vec::new();
        let harbor_id = crate::security::harbor_id_from_topic(&test_topic());
        let proof = crate::network::membership::create_membership_proof(
            &test_topic(),
            &harbor_id,
            &test_sender(),
        );

        buf.extend_from_slice(&harbor_id);
        buf.push(sync_message_type::SYNC_REQUEST);
        buf.extend_from_slice(&proof);

        SyncService::process_sync_message(&buf, test_sender(), &db, &tx)
            .await
            .unwrap();

        let event = rx.recv().await.unwrap();
        match event {
            ProtocolEvent::SyncRequest(req) => {
                assert_eq!(req.topic_id, test_topic());
                assert_eq!(req.sender_id, test_sender());
            }
            _ => panic!("expected SyncRequest event"),
        }
    }

    #[tokio::test]
    async fn test_process_sync_response() {
        let (tx, mut rx) = mpsc::channel(10);
        let db = test_db_with_topic().await;

        let mut buf = Vec::new();
        let harbor_id = crate::security::harbor_id_from_topic(&test_topic());
        let proof = crate::network::membership::create_membership_proof(
            &test_topic(),
            &harbor_id,
            &test_sender(),
        );

        buf.extend_from_slice(&harbor_id);
        buf.push(sync_message_type::SYNC_RESPONSE);
        buf.extend_from_slice(&proof);
        buf.extend_from_slice(b"test data");

        SyncService::process_sync_message(&buf, test_sender(), &db, &tx)
            .await
            .unwrap();

        let event = rx.recv().await.unwrap();
        match event {
            ProtocolEvent::SyncResponse(resp) => {
                assert_eq!(resp.topic_id, test_topic());
                assert_eq!(resp.data, b"test data");
            }
            _ => panic!("expected SyncResponse event"),
        }
    }

    #[tokio::test]
    async fn test_message_too_short() {
        let (tx, _rx) = mpsc::channel(10);
        let db = test_db_with_topic().await;
        let buf = vec![0u8; 32]; // Only topic_id, no type byte
        let result = SyncService::process_sync_message(&buf, test_sender(), &db, &tx).await;
        assert!(matches!(result, Err(ProcessSyncError::MessageTooShort(_))));
    }

    #[tokio::test]
    async fn test_unknown_message_type() {
        let (tx, _rx) = mpsc::channel(10);
        let db = test_db_with_topic().await;

        let mut buf = Vec::new();
        let harbor_id = crate::security::harbor_id_from_topic(&test_topic());
        buf.extend_from_slice(&harbor_id);
        buf.push(0xFF); // Unknown type

        let result = SyncService::process_sync_message(&buf, test_sender(), &db, &tx).await;
        assert!(matches!(
            result,
            Err(ProcessSyncError::UnknownMessageType(0xFF))
        ));
    }

    #[test]
    fn test_process_sync_error_display() {
        let err = ProcessSyncError::MessageTooShort(10);
        assert!(err.to_string().contains("10"));

        let err = ProcessSyncError::EmptyPayload;
        assert!(err.to_string().contains("empty"));

        let err = ProcessSyncError::UnknownMessageType(0xFF);
        assert!(err.to_string().contains("0xff"));

        let err = ProcessSyncError::InvalidMembershipProof;
        assert!(err.to_string().contains("membership"));
    }

    #[test]
    fn test_sync_config_default() {
        let config = SyncConfig::default();
        assert_eq!(config.connect_timeout, Duration::from_secs(30));
        assert_eq!(config.max_response_size, 100 * 1024 * 1024);
    }

    #[test]
    fn test_encode_sync_response() {
        let topic_id = test_topic();
        let sender_id = test_sender();
        let data = b"test snapshot data";

        let encoded = encode_sync_response(&topic_id, &sender_id, data);
        let harbor_id = crate::security::harbor_id_from_topic(&topic_id);
        let proof =
            crate::network::membership::create_membership_proof(&topic_id, &harbor_id, &sender_id);

        // Check format: [32 bytes harbor_id][1 byte type][32 bytes proof][data]
        assert_eq!(encoded.len(), 32 + 1 + 32 + data.len());
        assert_eq!(&encoded[..32], &harbor_id);
        assert_eq!(encoded[32], sync_message_type::SYNC_RESPONSE);
        assert_eq!(&encoded[33..65], &proof);
        assert_eq!(&encoded[65..], data);
    }

    #[test]
    fn test_encode_sync_response_empty_data() {
        let topic_id = test_topic();
        let sender_id = test_sender();
        let data: &[u8] = &[];

        let encoded = encode_sync_response(&topic_id, &sender_id, data);
        let harbor_id = crate::security::harbor_id_from_topic(&topic_id);
        let proof =
            crate::network::membership::create_membership_proof(&topic_id, &harbor_id, &sender_id);

        assert_eq!(encoded.len(), 65); // harbor_id + type + proof
        assert_eq!(&encoded[..32], &harbor_id);
        assert_eq!(encoded[32], sync_message_type::SYNC_RESPONSE);
        assert_eq!(&encoded[33..65], &proof);
    }
}
