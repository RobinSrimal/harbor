//! Sync Service - handles sync message processing and encoding
//!
//! The SyncService handles:
//! - Parsing sync message format (topic_id + type byte + data)
//! - Validating message structure
//! - Emitting protocol events for the app layer
//! - Encoding outgoing sync messages

use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::protocol::{ProtocolEvent, SyncRequestEvent, SyncResponseEvent};

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
///
/// This function:
/// 1. Validates message length
/// 2. Extracts topic_id (first 32 bytes)
/// 3. Parses message type and data
/// 4. Emits appropriate protocol event
///
/// # Arguments
/// * `buf` - Raw message bytes from the stream
/// * `sender_id` - The sender's EndpointID
/// * `event_tx` - Channel for emitting protocol events
///
/// # Returns
/// * `Ok(())` - Message processed and event emitted
/// * `Err(ProcessSyncError)` - Parsing/validation failed
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
            // SyncRequest - peer is requesting our state
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
            // SyncResponse - peer is providing their state
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
