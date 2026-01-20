//! Topic control messages
//!
//! These messages are sent as Send packet payloads to notify topic members
//! of membership changes.


use serde::{Deserialize, Serialize};

use crate::security::send::packet::VerificationMode;

/// Message type prefix byte
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    /// Regular content message (no prefix, raw payload)
    Content = 0x00,
    /// Join announcement
    Join = 0x01,
    /// Leave announcement  
    Leave = 0x02,
    /// File share announcement (new file available)
    FileAnnouncement = 0x03,
    /// Can seed announcement (peer has complete file)
    CanSeed = 0x04,
    /// CRDT sync update (delta)
    SyncUpdate = 0x05,
    /// Sync request (asking peers for their state)
    SyncRequest = 0x06,
    /// Sync response (full CRDT state)
    SyncResponse = 0x07,
}

impl MessageType {
    /// Get the verification mode required for this message type
    ///
    /// - Content: Full (MAC + signature) - sender should be known member
    /// - Join: MacOnly - sender's key is not yet known to recipients
    /// - Leave: Full - sender should be known member
    /// - FileAnnouncement: Full - sender should be known member
    /// - CanSeed: Full - sender should be known member
    /// - SyncUpdate/Request/Response: Full - sender should be known member
    pub fn verification_mode(&self) -> VerificationMode {
        match self {
            MessageType::Content => VerificationMode::Full,
            MessageType::Join => VerificationMode::MacOnly,
            MessageType::Leave => VerificationMode::Full,
            MessageType::FileAnnouncement => VerificationMode::Full,
            MessageType::CanSeed => VerificationMode::Full,
            MessageType::SyncUpdate => VerificationMode::Full,
            MessageType::SyncRequest => VerificationMode::Full,
            MessageType::SyncResponse => VerificationMode::Full,
        }
    }

    /// Check if this message type requires signature verification
    pub fn requires_signature(&self) -> bool {
        self.verification_mode() == VerificationMode::Full
    }
}

impl TryFrom<u8> for MessageType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(MessageType::Content),
            0x01 => Ok(MessageType::Join),
            0x02 => Ok(MessageType::Leave),
            0x03 => Ok(MessageType::FileAnnouncement),
            0x04 => Ok(MessageType::CanSeed),
            0x05 => Ok(MessageType::SyncUpdate),
            0x06 => Ok(MessageType::SyncRequest),
            0x07 => Ok(MessageType::SyncResponse),
            _ => Err(()),
        }
    }
}

/// A topic control message
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicMessage {
    /// Regular content (the payload itself)
    Content(Vec<u8>),
    /// Join announcement
    Join(JoinMessage),
    /// Leave announcement
    Leave(LeaveMessage),
    /// File share announcement (source is sharing a new file)
    FileAnnouncement(FileAnnouncementMessage),
    /// Can seed announcement (peer has complete file, can serve)
    CanSeed(CanSeedMessage),
    /// CRDT sync update (delta bytes)
    SyncUpdate(SyncUpdateMessage),
    /// Sync request (asking peers for their current state)
    SyncRequest,
    /// Sync response (full CRDT state)
    SyncResponse(SyncResponseMessage),
}

/// CRDT sync update message (delta)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncUpdateMessage {
    /// Raw CRDT delta bytes
    pub data: Vec<u8>,
}

/// CRDT sync response message (full state)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncResponseMessage {
    /// Full CRDT state bytes
    pub data: Vec<u8>,
}

impl TopicMessage {
    /// Encode the message for inclusion in a Send packet payload
    pub fn encode(&self) -> Vec<u8> {
        match self {
            TopicMessage::Content(data) => {
                // Content messages have no prefix if they don't start with 0x01 or 0x02
                // To be safe, we always prefix with 0x00 for content
                let mut bytes = Vec::with_capacity(1 + data.len());
                bytes.push(MessageType::Content as u8);
                bytes.extend_from_slice(data);
                bytes
            }
            TopicMessage::Join(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(MessageType::Join as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            TopicMessage::Leave(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(MessageType::Leave as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            TopicMessage::FileAnnouncement(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(MessageType::FileAnnouncement as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            TopicMessage::CanSeed(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(MessageType::CanSeed as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            TopicMessage::SyncUpdate(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(MessageType::SyncUpdate as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
            TopicMessage::SyncRequest => {
                // No payload, just the type byte
                vec![MessageType::SyncRequest as u8]
            }
            TopicMessage::SyncResponse(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(MessageType::SyncResponse as u8);
                bytes.extend_from_slice(&payload);
                bytes
            }
        }
    }

    /// Decode a message from a Send packet payload
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError::Empty);
        }

        let msg_type = MessageType::try_from(bytes[0])
            .map_err(|_| DecodeError::UnknownType(bytes[0]))?;

        match msg_type {
            MessageType::Content => {
                Ok(TopicMessage::Content(bytes[1..].to_vec()))
            }
            MessageType::Join => {
                let msg: JoinMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(TopicMessage::Join(msg))
            }
            MessageType::Leave => {
                let msg: LeaveMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(TopicMessage::Leave(msg))
            }
            MessageType::FileAnnouncement => {
                let msg: FileAnnouncementMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(TopicMessage::FileAnnouncement(msg))
            }
            MessageType::CanSeed => {
                let msg: CanSeedMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(TopicMessage::CanSeed(msg))
            }
            MessageType::SyncUpdate => {
                let msg: SyncUpdateMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(TopicMessage::SyncUpdate(msg))
            }
            MessageType::SyncRequest => {
                // No payload
                Ok(TopicMessage::SyncRequest)
            }
            MessageType::SyncResponse => {
                let msg: SyncResponseMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(TopicMessage::SyncResponse(msg))
            }
        }
    }

    /// Check if this is a control message (Join, Leave, FileAnnouncement, or CanSeed)
    pub fn is_control(&self) -> bool {
        matches!(
            self,
            TopicMessage::Join(_)
                | TopicMessage::Leave(_)
                | TopicMessage::FileAnnouncement(_)
                | TopicMessage::CanSeed(_)
        )
    }

    /// Get the content if this is a content message
    pub fn as_content(&self) -> Option<&[u8]> {
        match self {
            TopicMessage::Content(data) => Some(data),
            _ => None,
        }
    }

    /// Get the message type
    pub fn message_type(&self) -> MessageType {
        match self {
            TopicMessage::Content(_) => MessageType::Content,
            TopicMessage::Join(_) => MessageType::Join,
            TopicMessage::Leave(_) => MessageType::Leave,
            TopicMessage::FileAnnouncement(_) => MessageType::FileAnnouncement,
            TopicMessage::CanSeed(_) => MessageType::CanSeed,
            TopicMessage::SyncUpdate(_) => MessageType::SyncUpdate,
            TopicMessage::SyncRequest => MessageType::SyncRequest,
            TopicMessage::SyncResponse(_) => MessageType::SyncResponse,
        }
    }

    /// Get the verification mode required for this message
    pub fn verification_mode(&self) -> VerificationMode {
        self.message_type().verification_mode()
    }

    /// Check if this message requires signature verification
    pub fn requires_signature(&self) -> bool {
        self.message_type().requires_signature()
    }
}

/// Get the verification mode for a raw payload without fully decoding
///
/// This is useful for Harbor Nodes that need to determine verification
/// mode before processing.
pub fn get_verification_mode_from_payload(payload: &[u8]) -> Option<VerificationMode> {
    if payload.is_empty() {
        return None;
    }
    MessageType::try_from(payload[0])
        .ok()
        .map(|t| t.verification_mode())
}

/// Join announcement message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinMessage {
    /// The joining node's EndpointID (same as packet sender)
    pub joiner: [u8; 32],
    /// Optional: the joiner's current direct address
    pub address: Option<String>,
    /// Optional: the joiner's relay URL for cross-network connectivity
    #[serde(default)]
    pub relay_url: Option<String>,
}

impl JoinMessage {
    /// Create a new join message
    pub fn new(joiner: [u8; 32]) -> Self {
        Self {
            joiner,
            address: None,
            relay_url: None,
        }
    }

    /// Create a new join message with relay URL
    pub fn with_relay(joiner: [u8; 32], relay_url: String) -> Self {
        Self {
            joiner,
            address: None,
            relay_url: Some(relay_url),
        }
    }

    /// Create a join message with address
    pub fn with_address(joiner: [u8; 32], address: String) -> Self {
        Self {
            joiner,
            address: Some(address),
            relay_url: None,
        }
    }
}

/// Leave announcement message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveMessage {
    /// The leaving node's EndpointID (same as packet sender)
    pub leaver: [u8; 32],
}

impl LeaveMessage {
    /// Create a new leave message
    pub fn new(leaver: [u8; 32]) -> Self {
        Self { leaver }
    }
}

/// File announcement message - announces a new shared file to topic members
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAnnouncementMessage {
    /// BLAKE3 hash of the complete file
    pub hash: [u8; 32],
    /// Source endpoint ID (who has the file)
    pub source_id: [u8; 32],
    /// File size in bytes
    pub total_size: u64,
    /// Number of 512 KB chunks
    pub total_chunks: u32,
    /// Number of sections for distribution
    pub num_sections: u8,
    /// Human-readable filename
    pub display_name: String,
}

impl FileAnnouncementMessage {
    /// Create a new file announcement
    pub fn new(
        hash: [u8; 32],
        source_id: [u8; 32],
        total_size: u64,
        total_chunks: u32,
        num_sections: u8,
        display_name: String,
    ) -> Self {
        Self {
            hash,
            source_id,
            total_size,
            total_chunks,
            num_sections,
            display_name,
        }
    }
}

/// Can seed message - announces that a peer has the complete file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanSeedMessage {
    /// BLAKE3 hash of the file
    pub hash: [u8; 32],
    /// Endpoint ID of the seeder
    pub seeder_id: [u8; 32],
}

impl CanSeedMessage {
    /// Create a new can seed message
    pub fn new(hash: [u8; 32], seeder_id: [u8; 32]) -> Self {
        Self { hash, seeder_id }
    }
}

/// Error decoding a topic message
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Empty payload
    Empty,
    /// Unknown message type
    UnknownType(u8),
    /// Invalid payload data
    InvalidPayload(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Empty => write!(f, "empty payload"),
            DecodeError::UnknownType(t) => write!(f, "unknown message type: {}", t),
            DecodeError::InvalidPayload(e) => write!(f, "invalid payload: {}", e),
        }
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_message_roundtrip() {
        let original = TopicMessage::Content(b"Hello, world!".to_vec());
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_join_message_roundtrip() {
        let join = JoinMessage::new([42u8; 32]);
        let original = TopicMessage::Join(join);
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_join_message_with_address() {
        let join = JoinMessage::with_address([42u8; 32], "192.168.1.1:4433".to_string());
        let original = TopicMessage::Join(join);
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        
        assert_eq!(decoded, original);
        
        if let TopicMessage::Join(msg) = decoded {
            assert_eq!(msg.address, Some("192.168.1.1:4433".to_string()));
        }
    }

    #[test]
    fn test_leave_message_roundtrip() {
        let leave = LeaveMessage::new([99u8; 32]);
        let original = TopicMessage::Leave(leave);
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_file_announcement_roundtrip() {
        let ann = FileAnnouncementMessage::new(
            [1u8; 32],  // hash
            [2u8; 32],  // source_id
            1024 * 1024,  // total_size
            2,  // total_chunks
            2,  // num_sections
            "test_file.bin".to_string(),
        );
        let original = TopicMessage::FileAnnouncement(ann);
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        
        assert_eq!(decoded, original);
        
        if let TopicMessage::FileAnnouncement(msg) = decoded {
            assert_eq!(msg.hash, [1u8; 32]);
            assert_eq!(msg.source_id, [2u8; 32]);
            assert_eq!(msg.total_size, 1024 * 1024);
            assert_eq!(msg.display_name, "test_file.bin");
        } else {
            panic!("expected FileAnnouncement");
        }
    }

    #[test]
    fn test_can_seed_roundtrip() {
        let can_seed = CanSeedMessage::new([3u8; 32], [4u8; 32]);
        let original = TopicMessage::CanSeed(can_seed);
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        
        assert_eq!(decoded, original);
        
        if let TopicMessage::CanSeed(msg) = decoded {
            assert_eq!(msg.hash, [3u8; 32]);
            assert_eq!(msg.seeder_id, [4u8; 32]);
        } else {
            panic!("expected CanSeed");
        }
    }

    #[test]
    fn test_decode_empty() {
        let result = TopicMessage::decode(&[]);
        assert!(matches!(result, Err(DecodeError::Empty)));
    }

    #[test]
    fn test_decode_unknown_type() {
        let result = TopicMessage::decode(&[0xFF]);
        assert!(matches!(result, Err(DecodeError::UnknownType(0xFF))));
    }

    #[test]
    fn test_is_control() {
        let content = TopicMessage::Content(b"test".to_vec());
        let join = TopicMessage::Join(JoinMessage::new([0u8; 32]));
        let leave = TopicMessage::Leave(LeaveMessage::new([0u8; 32]));
        let file_ann = TopicMessage::FileAnnouncement(FileAnnouncementMessage::new(
            [0u8; 32], [0u8; 32], 1024, 1, 1, "test".to_string(),
        ));
        let can_seed = TopicMessage::CanSeed(CanSeedMessage::new([0u8; 32], [0u8; 32]));
        
        assert!(!content.is_control());
        assert!(join.is_control());
        assert!(leave.is_control());
        assert!(file_ann.is_control());
        assert!(can_seed.is_control());
    }

    #[test]
    fn test_as_content() {
        let content = TopicMessage::Content(b"test data".to_vec());
        assert_eq!(content.as_content(), Some(b"test data".as_slice()));
        
        let join = TopicMessage::Join(JoinMessage::new([0u8; 32]));
        assert_eq!(join.as_content(), None);
    }

    #[test]
    fn test_message_type_conversion() {
        assert_eq!(MessageType::try_from(0x00), Ok(MessageType::Content));
        assert_eq!(MessageType::try_from(0x01), Ok(MessageType::Join));
        assert_eq!(MessageType::try_from(0x02), Ok(MessageType::Leave));
        assert_eq!(MessageType::try_from(0x03), Ok(MessageType::FileAnnouncement));
        assert_eq!(MessageType::try_from(0x04), Ok(MessageType::CanSeed));
        assert_eq!(MessageType::try_from(0x05), Ok(MessageType::SyncUpdate));
        assert!(MessageType::try_from(0x06).is_err());
    }

    #[test]
    fn test_empty_content() {
        let original = TopicMessage::Content(vec![]);
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_sync_update_roundtrip() {
        let original = TopicMessage::SyncUpdate(SyncUpdateMessage {
            data: vec![1, 2, 3, 4, 5],
        });
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        
        assert_eq!(decoded, original);
        assert_eq!(encoded[0], 0x05); // SyncUpdate type byte
    }

    #[test]
    fn test_verification_mode() {
        // Content requires full verification (MAC + signature)
        assert_eq!(MessageType::Content.verification_mode(), VerificationMode::Full);
        
        // Join only requires MAC (sender not yet known)
        assert_eq!(MessageType::Join.verification_mode(), VerificationMode::MacOnly);
        
        // Leave requires full verification (sender should be known)
        assert_eq!(MessageType::Leave.verification_mode(), VerificationMode::Full);
    }

    #[test]
    fn test_requires_signature() {
        assert!(MessageType::Content.requires_signature());
        assert!(!MessageType::Join.requires_signature());
        assert!(MessageType::Leave.requires_signature());
    }

    #[test]
    fn test_topic_message_verification_mode() {
        let content = TopicMessage::Content(b"test".to_vec());
        let join = TopicMessage::Join(JoinMessage::new([0u8; 32]));
        let leave = TopicMessage::Leave(LeaveMessage::new([0u8; 32]));
        
        assert_eq!(content.verification_mode(), VerificationMode::Full);
        assert_eq!(join.verification_mode(), VerificationMode::MacOnly);
        assert_eq!(leave.verification_mode(), VerificationMode::Full);
        
        assert!(content.requires_signature());
        assert!(!join.requires_signature());
        assert!(leave.requires_signature());
    }

    #[test]
    fn test_get_verification_mode_from_payload() {
        // Content payload
        let content = TopicMessage::Content(b"test".to_vec()).encode();
        assert_eq!(
            get_verification_mode_from_payload(&content),
            Some(VerificationMode::Full)
        );
        
        // Join payload
        let join = TopicMessage::Join(JoinMessage::new([0u8; 32])).encode();
        assert_eq!(
            get_verification_mode_from_payload(&join),
            Some(VerificationMode::MacOnly)
        );
        
        // Empty payload
        assert_eq!(get_verification_mode_from_payload(&[]), None);
        
        // Unknown type
        assert_eq!(get_verification_mode_from_payload(&[0xFF]), None);
    }

    #[test]
    fn test_join_message_with_relay() {
        let join = JoinMessage::with_relay([42u8; 32], "https://relay.example.com/".to_string());
        
        assert_eq!(join.joiner, [42u8; 32]);
        assert!(join.address.is_none());
        assert_eq!(join.relay_url, Some("https://relay.example.com/".to_string()));
        
        // Roundtrip
        let original = TopicMessage::Join(join);
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_message_type_method() {
        let content = TopicMessage::Content(b"test".to_vec());
        let join = TopicMessage::Join(JoinMessage::new([0u8; 32]));
        let leave = TopicMessage::Leave(LeaveMessage::new([0u8; 32]));
        let file_ann = TopicMessage::FileAnnouncement(FileAnnouncementMessage::new(
            [0u8; 32], [0u8; 32], 1024, 1, 1, "test".to_string(),
        ));
        let can_seed = TopicMessage::CanSeed(CanSeedMessage::new([0u8; 32], [0u8; 32]));
        
        assert_eq!(content.message_type(), MessageType::Content);
        assert_eq!(join.message_type(), MessageType::Join);
        assert_eq!(leave.message_type(), MessageType::Leave);
        assert_eq!(file_ann.message_type(), MessageType::FileAnnouncement);
        assert_eq!(can_seed.message_type(), MessageType::CanSeed);
    }

    #[test]
    fn test_decode_error_display() {
        assert_eq!(DecodeError::Empty.to_string(), "empty payload");
        assert_eq!(DecodeError::UnknownType(0xFF).to_string(), "unknown message type: 255");
        assert_eq!(
            DecodeError::InvalidPayload("test error".to_string()).to_string(),
            "invalid payload: test error"
        );
    }
}

