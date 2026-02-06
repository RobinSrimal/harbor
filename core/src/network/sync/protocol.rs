//! Sync protocol wire format
//!
//! Messages for CRDT synchronization:
//! - Update: Delta updates (sent via Send protocol, batched)
//! - InitialSyncRequest: New member requests full state (via Send)
//! - InitialSyncResponse: Full snapshot (via direct connection - no size limit)

use crate::network::wire;
use serde::{Deserialize, Serialize};

/// Sync protocol ALPN (for direct connections - initial sync)
pub const SYNC_ALPN: &[u8] = b"harbor/sync/0";

/// Prefix byte for sync messages sent via Send protocol
/// Used to distinguish sync messages from regular app messages
pub const SYNC_MESSAGE_PREFIX: u8 = 0x53; // 'S'

/// Message type byte for wire format
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMessageType {
    /// Delta update (batched changes)
    Update = 0x01,
    /// Request for initial sync (new/returning member)
    InitialSyncRequest = 0x02,
    /// Response with full snapshot (via direct connection)
    InitialSyncResponse = 0x03,
}

impl TryFrom<u8> for SyncMessageType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(SyncMessageType::Update),
            0x02 => Ok(SyncMessageType::InitialSyncRequest),
            0x03 => Ok(SyncMessageType::InitialSyncResponse),
            _ => Err(()),
        }
    }
}

/// Sync messages sent via Send protocol (small, need offline delivery via Harbor)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncMessage {
    /// Delta update containing batched Loro operations
    Update(SyncUpdate),
    /// Request for initial sync from existing members
    InitialSyncRequest(InitialSyncRequest),
}

/// Delta update message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncUpdate {
    /// Loro-encoded delta (changes since last broadcast)
    pub data: Vec<u8>,
}

/// Initial sync request (sent by new/returning member)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitialSyncRequest {
    /// Unique request ID (to match responses)
    pub request_id: [u8; 32],
    /// Loro-encoded version vector (what we already have, if any)
    pub version: Vec<u8>,
}

/// Initial sync response (sent via direct connection - no size limit)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitialSyncResponse {
    /// Request ID this is responding to
    pub request_id: [u8; 32],
    /// Loro-encoded snapshot (full document state)
    pub snapshot: Vec<u8>,
}

impl SyncMessage {
    /// Encode for transmission via Send protocol
    /// Format: [SYNC_PREFIX][type][length][payload] using shared wire framing
    pub fn encode(&self) -> Vec<u8> {
        let (msg_type, payload) = match self {
            SyncMessage::Update(u) => (
                SyncMessageType::Update,
                postcard::to_allocvec(u).expect("serialization should not fail"),
            ),
            SyncMessage::InitialSyncRequest(r) => (
                SyncMessageType::InitialSyncRequest,
                postcard::to_allocvec(r).expect("serialization should not fail"),
            ),
        };

        // Prepend sync prefix, then use standard wire framing
        let frame = wire::encode_frame(msg_type as u8, &payload);
        let mut bytes = Vec::with_capacity(1 + frame.len());
        bytes.push(SYNC_MESSAGE_PREFIX);
        bytes.extend_from_slice(&frame);
        bytes
    }

    /// Decode from bytes received via Send protocol
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError::TooShort);
        }

        if bytes[0] != SYNC_MESSAGE_PREFIX {
            return Err(DecodeError::InvalidPrefix);
        }

        // Skip prefix, decode using standard wire framing
        let frame = wire::decode_frame(&bytes[1..]).map_err(|_| DecodeError::TooShort)?;

        let msg_type =
            SyncMessageType::try_from(frame.msg_type).map_err(|_| DecodeError::InvalidType)?;

        match msg_type {
            SyncMessageType::Update => {
                let update: SyncUpdate =
                    postcard::from_bytes(frame.payload).map_err(|_| DecodeError::InvalidPayload)?;
                Ok(SyncMessage::Update(update))
            }
            SyncMessageType::InitialSyncRequest => {
                let request: InitialSyncRequest =
                    postcard::from_bytes(frame.payload).map_err(|_| DecodeError::InvalidPayload)?;
                Ok(SyncMessage::InitialSyncRequest(request))
            }
            SyncMessageType::InitialSyncResponse => {
                // InitialSyncResponse is sent via direct connection, not Send
                Err(DecodeError::InvalidType)
            }
        }
    }

    /// Check if bytes start with sync message prefix
    pub fn is_sync_message(bytes: &[u8]) -> bool {
        !bytes.is_empty() && bytes[0] == SYNC_MESSAGE_PREFIX
    }
}

impl InitialSyncResponse {
    /// Encode for transmission via direct connection using shared wire framing
    pub fn encode(&self) -> Vec<u8> {
        let payload = postcard::to_allocvec(self).expect("serialization should not fail");
        wire::encode_frame(SyncMessageType::InitialSyncResponse as u8, &payload)
    }

    /// Decode from bytes received via direct connection
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let frame = wire::decode_frame(bytes).map_err(|_| DecodeError::TooShort)?;

        if frame.msg_type != SyncMessageType::InitialSyncResponse as u8 {
            return Err(DecodeError::InvalidType);
        }

        postcard::from_bytes(frame.payload).map_err(|_| DecodeError::InvalidPayload)
    }
}

/// Decode errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    TooShort,
    InvalidPrefix,
    InvalidType,
    InvalidPayload,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooShort => write!(f, "message too short"),
            DecodeError::InvalidPrefix => write!(f, "invalid sync message prefix"),
            DecodeError::InvalidType => write!(f, "invalid message type"),
            DecodeError::InvalidPayload => write!(f, "invalid payload"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_roundtrip() {
        let update = SyncMessage::Update(SyncUpdate {
            data: vec![1, 2, 3, 4, 5],
        });
        
        let encoded = update.encode();
        assert_eq!(encoded[0], SYNC_MESSAGE_PREFIX);
        
        let decoded = SyncMessage::decode(&encoded).unwrap();
        match decoded {
            SyncMessage::Update(u) => assert_eq!(u.data, vec![1, 2, 3, 4, 5]),
            _ => panic!("wrong type"),
        }
    }
    
    #[test]
    fn test_initial_sync_request_roundtrip() {
        let request = SyncMessage::InitialSyncRequest(InitialSyncRequest {
            request_id: [42u8; 32],
            version: vec![10, 20, 30],
        });
        
        let encoded = request.encode();
        let decoded = SyncMessage::decode(&encoded).unwrap();
        
        match decoded {
            SyncMessage::InitialSyncRequest(r) => {
                assert_eq!(r.request_id, [42u8; 32]);
                assert_eq!(r.version, vec![10, 20, 30]);
            }
            _ => panic!("wrong type"),
        }
    }
    
    #[test]
    fn test_initial_sync_response_roundtrip() {
        let response = InitialSyncResponse {
            request_id: [99u8; 32],
            snapshot: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        };
        
        let encoded = response.encode();
        let decoded = InitialSyncResponse::decode(&encoded).unwrap();
        
        assert_eq!(decoded.request_id, [99u8; 32]);
        assert_eq!(decoded.snapshot, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }
    
    #[test]
    fn test_is_sync_message() {
        assert!(SyncMessage::is_sync_message(&[SYNC_MESSAGE_PREFIX, 0, 0, 0, 0, 0]));
        assert!(!SyncMessage::is_sync_message(&[0x00, 1, 2, 3]));
        assert!(!SyncMessage::is_sync_message(&[]));
    }
    
    #[test]
    fn test_decode_too_short() {
        // Less than 6 bytes should fail
        assert!(matches!(
            SyncMessage::decode(&[SYNC_MESSAGE_PREFIX, 0x01]),
            Err(DecodeError::TooShort)
        ));
        assert!(matches!(
            SyncMessage::decode(&[]),
            Err(DecodeError::TooShort)
        ));
    }
    
    #[test]
    fn test_decode_invalid_prefix() {
        let bytes = vec![0x00, 0x01, 0, 0, 0, 5, 1, 2, 3, 4, 5];
        assert!(matches!(
            SyncMessage::decode(&bytes),
            Err(DecodeError::InvalidPrefix)
        ));
    }
    
    #[test]
    fn test_decode_invalid_type() {
        // Type byte 0xFF is invalid
        let bytes = vec![SYNC_MESSAGE_PREFIX, 0xFF, 0, 0, 0, 0];
        assert!(matches!(
            SyncMessage::decode(&bytes),
            Err(DecodeError::InvalidType)
        ));
    }
    
    #[test]
    fn test_decode_truncated_payload() {
        // Header says 100 bytes but only 5 provided
        let bytes = vec![SYNC_MESSAGE_PREFIX, 0x01, 0, 0, 0, 100, 1, 2, 3, 4, 5];
        assert!(matches!(
            SyncMessage::decode(&bytes),
            Err(DecodeError::TooShort)
        ));
    }
    
    #[test]
    fn test_sync_message_type_conversion() {
        assert_eq!(SyncMessageType::try_from(0x01), Ok(SyncMessageType::Update));
        assert_eq!(SyncMessageType::try_from(0x02), Ok(SyncMessageType::InitialSyncRequest));
        assert_eq!(SyncMessageType::try_from(0x03), Ok(SyncMessageType::InitialSyncResponse));
        assert!(SyncMessageType::try_from(0x00).is_err());
        assert!(SyncMessageType::try_from(0x04).is_err());
        assert!(SyncMessageType::try_from(0xFF).is_err());
    }
    
    #[test]
    fn test_large_update_roundtrip() {
        // Test with larger payload (simulating real Loro data)
        let large_data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        
        let update = SyncMessage::Update(SyncUpdate {
            data: large_data.clone(),
        });
        
        let encoded = update.encode();
        let decoded = SyncMessage::decode(&encoded).unwrap();
        
        match decoded {
            SyncMessage::Update(u) => assert_eq!(u.data, large_data),
            _ => panic!("wrong type"),
        }
    }
    
    #[test]
    fn test_initial_sync_response_decode_invalid_type() {
        // First byte should be InitialSyncResponse type
        let bytes = vec![0x01, 0, 0, 0, 5, 1, 2, 3, 4, 5];
        assert!(matches!(
            InitialSyncResponse::decode(&bytes),
            Err(DecodeError::InvalidType)
        ));
    }
    
    #[test]
    fn test_initial_sync_response_decode_too_short() {
        assert!(matches!(
            InitialSyncResponse::decode(&[0x03, 0, 0]),
            Err(DecodeError::TooShort)
        ));
    }
    
    #[test]
    fn test_empty_update() {
        let update = SyncMessage::Update(SyncUpdate {
            data: vec![],
        });
        
        let encoded = update.encode();
        let decoded = SyncMessage::decode(&encoded).unwrap();
        
        match decoded {
            SyncMessage::Update(u) => assert!(u.data.is_empty()),
            _ => panic!("wrong type"),
        }
    }
}

