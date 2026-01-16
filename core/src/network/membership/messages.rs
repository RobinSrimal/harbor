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
}

impl MessageType {
    /// Get the verification mode required for this message type
    ///
    /// - Content: Full (MAC + signature) - sender should be known member
    /// - Join: MacOnly - sender's key is not yet known to recipients
    /// - Leave: Full - sender should be known member
    pub fn verification_mode(&self) -> VerificationMode {
        match self {
            MessageType::Content => VerificationMode::Full,
            MessageType::Join => VerificationMode::MacOnly,
            MessageType::Leave => VerificationMode::Full,
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
        }
    }

    /// Check if this is a control message (Join or Leave)
    pub fn is_control(&self) -> bool {
        matches!(self, TopicMessage::Join(_) | TopicMessage::Leave(_))
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
        
        assert!(!content.is_control());
        assert!(join.is_control());
        assert!(leave.is_control());
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
        assert!(MessageType::try_from(0x03).is_err());
    }

    #[test]
    fn test_empty_content() {
        let original = TopicMessage::Content(vec![]);
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        
        assert_eq!(decoded, original);
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
        
        assert_eq!(content.message_type(), MessageType::Content);
        assert_eq!(join.message_type(), MessageType::Join);
        assert_eq!(leave.message_type(), MessageType::Leave);
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

