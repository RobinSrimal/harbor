//! Sync Service - handles CRDT sync transport
//!
//! The SyncService handles:
//! - Incoming SYNC_ALPN connections (sync requests/responses)
//! - Outgoing sync updates (via SendService broadcast)
//! - Outgoing sync requests (via SendService broadcast)
//! - Outgoing sync responses (via direct SYNC_ALPN connection)

use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId};
use rusqlite::Connection as DbConnection;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use crate::data::{get_topic_members_with_info, LocalIdentity};
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::send::SendService;
use crate::network::send::topic_messages::{TopicMessage, SyncUpdateMessage};
use crate::protocol::{MemberInfo, ProtocolError, ProtocolEvent, SyncRequestEvent, SyncResponseEvent};

use super::protocol::SYNC_ALPN;

/// Message type bytes for sync protocol
pub mod sync_message_type {
    /// SyncRequest - peer is requesting our state
    pub const SYNC_REQUEST: u8 = 0x01;
    /// SyncResponse - peer is providing their state
    pub const SYNC_RESPONSE: u8 = 0x02;
}

/// Error during sync message processing
#[derive(Debug)]
pub enum ProcessSyncError {
    /// Message too short (need at least 33 bytes: 32 topic_id + 1 type)
    MessageTooShort(usize),
    /// Empty message payload after topic_id
    EmptyPayload,
    /// Unknown message type
    UnknownMessageType(u8),
}

impl std::fmt::Display for ProcessSyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessSyncError::MessageTooShort(len) => {
                write!(f, "message too short: {} bytes (need at least 33)", len)
            }
            ProcessSyncError::EmptyPayload => write!(f, "empty message payload"),
            ProcessSyncError::UnknownMessageType(t) => write!(f, "unknown message type: 0x{:02x}", t),
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
pub struct SyncService {
    /// Iroh endpoint (for outgoing SYNC_ALPN connections)
    endpoint: Endpoint,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// Event sender (for incoming sync events)
    event_tx: mpsc::Sender<ProtocolEvent>,
    /// Send service (for broadcasting via send protocol)
    send_service: Arc<SendService>,
    /// Configuration
    config: SyncConfig,
}

impl SyncService {
    /// Create a new Sync service
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        send_service: Arc<SendService>,
    ) -> Self {
        Self {
            endpoint,
            identity,
            db,
            event_tx,
            send_service,
            config: SyncConfig::default(),
        }
    }

    // ========== Incoming ==========
    // handle_sync_connection is in handlers/incoming/sync.rs (impl SyncService)

    /// Get the event sender (used by incoming handler)
    pub(crate) fn event_tx(&self) -> &mpsc::Sender<ProtocolEvent> {
        &self.event_tx
    }

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
        let members_with_info = {
            let db = self.db.lock().await;
            get_topic_members_with_info(&db, topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        if members_with_info.is_empty() {
            return Err(ProtocolError::TopicNotFound);
        }

        let our_id = self.identity.public_key;

        // Get recipients (all members except us)
        let recipients: Vec<MemberInfo> = members_with_info
            .into_iter()
            .filter(|m| m.endpoint_id != our_id)
            .map(|m| MemberInfo {
                endpoint_id: m.endpoint_id,
                relay_url: m.relay_url,
            })
            .collect();

        if recipients.is_empty() {
            return Ok(()); // No one to send to
        }

        // Encode as SyncUpdate message
        let msg = TopicMessage::SyncUpdate(SyncUpdateMessage { data });
        let encoded = msg.encode();

        // Broadcast to all members via SendService
        self.send_service
            .send_to_topic(topic_id, &encoded, &recipients, HarborPacketType::Content)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
            .map(|_| ())
    }

    /// Request sync state from topic members
    ///
    /// Broadcasts a sync request to all members. When peers respond,
    /// you'll receive `ProtocolEvent::SyncResponse` events.
    pub async fn request_sync(
        &self,
        topic_id: &[u8; 32],
    ) -> Result<(), ProtocolError> {
        // Get topic members
        let members_with_info = {
            let db = self.db.lock().await;
            get_topic_members_with_info(&db, topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        if members_with_info.is_empty() {
            return Err(ProtocolError::TopicNotFound);
        }

        let our_id = self.identity.public_key;

        // Get recipients (all members except us)
        let recipients: Vec<MemberInfo> = members_with_info
            .into_iter()
            .filter(|m| m.endpoint_id != our_id)
            .map(|m| MemberInfo {
                endpoint_id: m.endpoint_id,
                relay_url: m.relay_url,
            })
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
            .send_to_topic(topic_id, &encoded, &recipients, HarborPacketType::Content)
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
        let message = encode_sync_response(topic_id, &data);

        // Connect to requester via SYNC_ALPN
        let node_id = EndpointId::from_bytes(requester_id)
            .map_err(|e| ProtocolError::Network(format!("invalid node id: {}", e)))?;

        let node_addr: EndpointAddr = node_id.into();

        let conn = tokio::time::timeout(
            self.config.connect_timeout,
            self.endpoint.connect(node_addr, SYNC_ALPN),
        )
        .await
        .map_err(|_| ProtocolError::Network("sync connect timeout".to_string()))?
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
}

// ========== Free functions (kept for backward compat and tests) ==========

/// Encode a sync response message for transmission via SYNC_ALPN
///
/// Format: [32 bytes topic_id][1 byte type = 0x02][data]
pub fn encode_sync_response(topic_id: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(32 + 1 + data.len());
    message.extend_from_slice(topic_id);
    message.push(sync_message_type::SYNC_RESPONSE);
    message.extend_from_slice(data);
    message
}

/// Process an incoming sync message - handles all business logic
pub async fn process_incoming_sync_message(
    buf: &[u8],
    sender_id: [u8; 32],
    event_tx: &mpsc::Sender<ProtocolEvent>,
) -> Result<(), ProcessSyncError> {
    // Check minimum size: 32 bytes topic_id + at least 1 byte message type
    if buf.len() < 33 {
        warn!(
            len = buf.len(),
            sender = %hex::encode(&sender_id[..8]),
            "SYNC_SERVICE: message too short"
        );
        return Err(ProcessSyncError::MessageTooShort(buf.len()));
    }

    // First 32 bytes are the topic_id
    let mut topic_id = [0u8; 32];
    topic_id.copy_from_slice(&buf[..32]);
    let message_bytes = &buf[32..];

    info!(
        topic = %hex::encode(&topic_id[..8]),
        sender = %hex::encode(&sender_id[..8]),
        message_len = message_bytes.len(),
        "SYNC_SERVICE: received message"
    );

    // Check for empty payload
    if message_bytes.is_empty() {
        warn!(
            topic = %hex::encode(&topic_id[..8]),
            sender = %hex::encode(&sender_id[..8]),
            "SYNC_SERVICE: empty message payload"
        );
        return Err(ProcessSyncError::EmptyPayload);
    }

    // Message type byte determines what kind of message this is
    let event = match message_bytes[0] {
        sync_message_type::SYNC_REQUEST => {
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
            let data = message_bytes[1..].to_vec();

            info!(
                topic = %hex::encode(&topic_id[..8]),
                sender = %hex::encode(&sender_id[..8]),
                data_len = data.len(),
                "SYNC_SERVICE: received SyncResponse"
            );
            ProtocolEvent::SyncResponse(SyncResponseEvent {
                topic_id,
                data,
            })
        }
        other => {
            debug!(
                topic = %hex::encode(&topic_id[..8]),
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

    #[tokio::test]
    async fn test_process_sync_request() {
        let (tx, mut rx) = mpsc::channel(10);

        let mut buf = Vec::new();
        buf.extend_from_slice(&test_topic());
        buf.push(sync_message_type::SYNC_REQUEST);

        process_incoming_sync_message(&buf, test_sender(), &tx).await.unwrap();

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

        let mut buf = Vec::new();
        buf.extend_from_slice(&test_topic());
        buf.push(sync_message_type::SYNC_RESPONSE);
        buf.extend_from_slice(b"test data");

        process_incoming_sync_message(&buf, test_sender(), &tx).await.unwrap();

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
        let buf = vec![0u8; 32]; // Only topic_id, no type byte
        let result = process_incoming_sync_message(&buf, test_sender(), &tx).await;
        assert!(matches!(result, Err(ProcessSyncError::MessageTooShort(_))));
    }

    #[tokio::test]
    async fn test_unknown_message_type() {
        let (tx, _rx) = mpsc::channel(10);

        let mut buf = Vec::new();
        buf.extend_from_slice(&test_topic());
        buf.push(0xFF); // Unknown type

        let result = process_incoming_sync_message(&buf, test_sender(), &tx).await;
        assert!(matches!(result, Err(ProcessSyncError::UnknownMessageType(0xFF))));
    }

    #[test]
    fn test_process_sync_error_display() {
        let err = ProcessSyncError::MessageTooShort(10);
        assert!(err.to_string().contains("10"));

        let err = ProcessSyncError::EmptyPayload;
        assert!(err.to_string().contains("empty"));

        let err = ProcessSyncError::UnknownMessageType(0xFF);
        assert!(err.to_string().contains("0xff"));
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
        let data = b"test snapshot data";

        let encoded = encode_sync_response(&topic_id, data);

        // Check format: [32 bytes topic_id][1 byte type][data]
        assert_eq!(encoded.len(), 32 + 1 + data.len());
        assert_eq!(&encoded[..32], &topic_id);
        assert_eq!(encoded[32], sync_message_type::SYNC_RESPONSE);
        assert_eq!(&encoded[33..], data);
    }

    #[test]
    fn test_encode_sync_response_empty_data() {
        let topic_id = test_topic();
        let data: &[u8] = &[];

        let encoded = encode_sync_response(&topic_id, data);

        assert_eq!(encoded.len(), 33); // Just topic_id + type byte
        assert_eq!(&encoded[..32], &topic_id);
        assert_eq!(encoded[32], sync_message_type::SYNC_RESPONSE);
    }
}
