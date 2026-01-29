//! DM (Direct Message) message types
//!
//! Separate from TopicMessage because DMs don't support topic-specific
//! control messages (Join, Leave). Uses 0x80+ type byte range to distinguish
//! from topic messages on the wire.

use serde::{Deserialize, Serialize};

use super::topic_messages::FileAnnouncementMessage;

/// DM message type prefix byte (0x80+ range)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmMessageType {
    /// Content message (raw payload) — 0x80 + 0x00 (MessageType::Content)
    Content = 0x80,
    /// File share announcement — 0x80 + 0x03 (MessageType::FileAnnouncement)
    FileAnnouncement = 0x83,
    /// Sync update (CRDT delta bytes) — 0x80 + 0x05 (MessageType::SyncUpdate)
    SyncUpdate = 0x85,
    /// Sync request (request full state, no payload) — 0x80 + 0x06 (MessageType::SyncRequest)
    SyncRequest = 0x86,
    /// Stream accept — 0x80 + 0x08
    StreamAccept = 0x87,
    /// Stream reject — 0x80 + 0x09
    StreamReject = 0x88,
    /// Stream liveness query — 0x80 + 0x0A
    StreamQuery = 0x89,
    /// Stream active (response to query) — 0x80 + 0x0B
    StreamActive = 0x8A,
    /// Stream ended (response to query) — 0x80 + 0x0C
    StreamEnded = 0x8B,
    /// Stream request (source → destination, DM-scoped) — 0x80 + 0x0D
    StreamRequest = 0x8C,
}

impl TryFrom<u8> for DmMessageType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x80 => Ok(DmMessageType::Content),
            0x83 => Ok(DmMessageType::FileAnnouncement),
            0x85 => Ok(DmMessageType::SyncUpdate),
            0x86 => Ok(DmMessageType::SyncRequest),
            0x87 => Ok(DmMessageType::StreamAccept),
            0x88 => Ok(DmMessageType::StreamReject),
            0x89 => Ok(DmMessageType::StreamQuery),
            0x8A => Ok(DmMessageType::StreamActive),
            0x8B => Ok(DmMessageType::StreamEnded),
            0x8C => Ok(DmMessageType::StreamRequest),
            _ => Err(()),
        }
    }
}

/// A direct message
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmMessage {
    /// Content payload (app defines format)
    Content(Vec<u8>),
    /// Sync update (CRDT delta bytes)
    SyncUpdate(Vec<u8>),
    /// Sync request (request full state)
    SyncRequest,
    /// File share announcement
    FileAnnouncement(FileAnnouncementMessage),
    /// Stream accept (destination → source)
    StreamAccept(DmStreamAcceptMessage),
    /// Stream reject (destination → source)
    StreamReject(DmStreamRejectMessage),
    /// Stream liveness query (destination → source)
    StreamQuery(DmStreamQueryMessage),
    /// Stream active response (source → destination)
    StreamActive(DmStreamActiveMessage),
    /// Stream ended response (source → destination)
    StreamEnded(DmStreamEndedMessage),
    /// Stream request (source → destination, DM-scoped peer-to-peer)
    StreamRequest(DmStreamRequestMessage),
}

impl DmMessage {
    /// Encode the message for inclusion in a DM packet payload
    pub fn encode(&self) -> Vec<u8> {
        match self {
            DmMessage::Content(data) => {
                let mut bytes = Vec::with_capacity(1 + data.len());
                bytes.push(DmMessageType::Content as u8);
                bytes.extend_from_slice(data);
                bytes
            }
            DmMessage::SyncUpdate(data) => {
                let mut bytes = Vec::with_capacity(1 + data.len());
                bytes.push(DmMessageType::SyncUpdate as u8);
                bytes.extend_from_slice(data);
                bytes
            }
            DmMessage::SyncRequest => {
                vec![DmMessageType::SyncRequest as u8]
            }
            DmMessage::FileAnnouncement(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(DmMessageType::FileAnnouncement as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            DmMessage::StreamAccept(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(DmMessageType::StreamAccept as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            DmMessage::StreamReject(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(DmMessageType::StreamReject as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            DmMessage::StreamQuery(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(DmMessageType::StreamQuery as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            DmMessage::StreamActive(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(DmMessageType::StreamActive as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            DmMessage::StreamEnded(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(DmMessageType::StreamEnded as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            DmMessage::StreamRequest(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(DmMessageType::StreamRequest as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
        }
    }

    /// Decode a message from a DM packet payload
    pub fn decode(bytes: &[u8]) -> Result<Self, DmDecodeError> {
        if bytes.is_empty() {
            return Err(DmDecodeError::Empty);
        }

        let msg_type = DmMessageType::try_from(bytes[0])
            .map_err(|_| DmDecodeError::UnknownType(bytes[0]))?;

        match msg_type {
            DmMessageType::Content => Ok(DmMessage::Content(bytes[1..].to_vec())),
            DmMessageType::SyncUpdate => Ok(DmMessage::SyncUpdate(bytes[1..].to_vec())),
            DmMessageType::SyncRequest => Ok(DmMessage::SyncRequest),
            DmMessageType::FileAnnouncement => {
                let msg: FileAnnouncementMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DmDecodeError::InvalidPayload(e.to_string()))?;
                Ok(DmMessage::FileAnnouncement(msg))
            }
            DmMessageType::StreamAccept => {
                let msg: DmStreamAcceptMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DmDecodeError::InvalidPayload(e.to_string()))?;
                Ok(DmMessage::StreamAccept(msg))
            }
            DmMessageType::StreamReject => {
                let msg: DmStreamRejectMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DmDecodeError::InvalidPayload(e.to_string()))?;
                Ok(DmMessage::StreamReject(msg))
            }
            DmMessageType::StreamQuery => {
                let msg: DmStreamQueryMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DmDecodeError::InvalidPayload(e.to_string()))?;
                Ok(DmMessage::StreamQuery(msg))
            }
            DmMessageType::StreamActive => {
                let msg: DmStreamActiveMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DmDecodeError::InvalidPayload(e.to_string()))?;
                Ok(DmMessage::StreamActive(msg))
            }
            DmMessageType::StreamEnded => {
                let msg: DmStreamEndedMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DmDecodeError::InvalidPayload(e.to_string()))?;
                Ok(DmMessage::StreamEnded(msg))
            }
            DmMessageType::StreamRequest => {
                let msg: DmStreamRequestMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DmDecodeError::InvalidPayload(e.to_string()))?;
                Ok(DmMessage::StreamRequest(msg))
            }
        }
    }

    /// Get the message type
    pub fn message_type(&self) -> DmMessageType {
        match self {
            DmMessage::Content(_) => DmMessageType::Content,
            DmMessage::SyncUpdate(_) => DmMessageType::SyncUpdate,
            DmMessage::SyncRequest => DmMessageType::SyncRequest,
            DmMessage::FileAnnouncement(_) => DmMessageType::FileAnnouncement,
            DmMessage::StreamAccept(_) => DmMessageType::StreamAccept,
            DmMessage::StreamReject(_) => DmMessageType::StreamReject,
            DmMessage::StreamQuery(_) => DmMessageType::StreamQuery,
            DmMessage::StreamActive(_) => DmMessageType::StreamActive,
            DmMessage::StreamEnded(_) => DmMessageType::StreamEnded,
            DmMessage::StreamRequest(_) => DmMessageType::StreamRequest,
        }
    }
}

/// Check if a type byte is in the DM range (0x80+)
pub fn is_dm_message_type(type_byte: u8) -> bool {
    type_byte >= 0x80
}

/// DM stream accept message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmStreamAcceptMessage {
    pub topic_id: [u8; 32],
    pub request_id: [u8; 32],
}

/// DM stream reject message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmStreamRejectMessage {
    pub topic_id: [u8; 32],
    pub request_id: [u8; 32],
    pub reason: Option<String>,
}

/// DM stream liveness query message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmStreamQueryMessage {
    pub topic_id: [u8; 32],
    pub request_id: [u8; 32],
}

/// DM stream active response message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmStreamActiveMessage {
    pub topic_id: [u8; 32],
    pub request_id: [u8; 32],
}

/// DM stream ended response message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmStreamEndedMessage {
    pub topic_id: [u8; 32],
    pub request_id: [u8; 32],
}

/// DM stream request message (peer-to-peer, no topic)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmStreamRequestMessage {
    pub request_id: [u8; 32],
    pub stream_name: String,
}

/// Error decoding a DM message
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmDecodeError {
    /// Empty payload
    Empty,
    /// Unknown message type
    UnknownType(u8),
    /// Invalid payload data
    InvalidPayload(String),
}

impl std::fmt::Display for DmDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DmDecodeError::Empty => write!(f, "empty DM payload"),
            DmDecodeError::UnknownType(t) => write!(f, "unknown DM message type: {:#x}", t),
            DmDecodeError::InvalidPayload(e) => write!(f, "invalid DM payload: {}", e),
        }
    }
}

impl std::error::Error for DmDecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_roundtrip() {
        let original = DmMessage::Content(b"Hello DM!".to_vec());
        let encoded = original.encode();
        assert_eq!(encoded[0], 0x80);
        let decoded = DmMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_empty_content() {
        let original = DmMessage::Content(vec![]);
        let encoded = original.encode();
        let decoded = DmMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_decode_empty() {
        assert!(matches!(DmMessage::decode(&[]), Err(DmDecodeError::Empty)));
    }

    #[test]
    fn test_decode_unknown_type() {
        assert!(matches!(
            DmMessage::decode(&[0xFF]),
            Err(DmDecodeError::UnknownType(0xFF))
        ));
    }

    #[test]
    fn test_sync_update_roundtrip() {
        let original = DmMessage::SyncUpdate(b"crdt delta".to_vec());
        let encoded = original.encode();
        assert_eq!(encoded[0], 0x85);
        let decoded = DmMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_sync_request_roundtrip() {
        let original = DmMessage::SyncRequest;
        let encoded = original.encode();
        assert_eq!(encoded, vec![0x86]);
        let decoded = DmMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_topic_type_rejected() {
        // Topic message type 0x00 should be rejected
        assert!(matches!(
            DmMessage::decode(&[0x00, 0x01, 0x02]),
            Err(DmDecodeError::UnknownType(0x00))
        ));
    }

    #[test]
    fn test_file_announcement_roundtrip() {
        let original = DmMessage::FileAnnouncement(FileAnnouncementMessage::new(
            [1u8; 32], [2u8; 32], 1024 * 1024, 2, 2, "test.bin".to_string(),
        ));
        let encoded = original.encode();
        assert_eq!(encoded[0], 0x83);
        let decoded = DmMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_is_dm_message_type() {
        assert!(!is_dm_message_type(0x00)); // Topic Content
        assert!(!is_dm_message_type(0x01)); // Topic Join
        assert!(!is_dm_message_type(0x7F)); // Last non-DM
        assert!(is_dm_message_type(0x80));  // DM Content
        assert!(is_dm_message_type(0xFF));  // Future DM types
    }
}
