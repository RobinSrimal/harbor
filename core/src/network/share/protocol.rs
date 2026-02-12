//! Share protocol wire format
//!
//! Messages for the file sharing protocol:
//! - FileAnnouncement: Broadcast when source starts sharing (via Send)
//! - CanSeed: Broadcast when peer reaches 100% (via Send)
//! - ChunkMapRequest/Response: Ask source who has what
//! - ChunkRequest/Response: Request/receive chunks
//! - PeerSuggestion: "I'm busy, try peer X"
//! - Bitfield: Exchange what chunks each peer has

use crate::network::wire;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Share protocol ALPN
pub const SHARE_ALPN: &[u8] = b"harbor/share/0";

/// Message type byte for wire format
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareMessageType {
    /// Request chunk map from source
    ChunkMapRequest = 0x01,
    /// Response with peer chunk information
    ChunkMapResponse = 0x02,
    /// Request specific chunks
    ChunkRequest = 0x03,
    /// Response with chunk data
    ChunkResponse = 0x04,
    /// Bitfield exchange
    Bitfield = 0x05,
    /// Suggest another peer (when busy)
    PeerSuggestion = 0x06,
    /// Chunk acknowledgment
    ChunkAck = 0x07,
}

impl TryFrom<u8> for ShareMessageType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(ShareMessageType::ChunkMapRequest),
            0x02 => Ok(ShareMessageType::ChunkMapResponse),
            0x03 => Ok(ShareMessageType::ChunkRequest),
            0x04 => Ok(ShareMessageType::ChunkResponse),
            0x05 => Ok(ShareMessageType::Bitfield),
            0x06 => Ok(ShareMessageType::PeerSuggestion),
            0x07 => Ok(ShareMessageType::ChunkAck),
            _ => Err(()),
        }
    }
}

/// File announcement message (sent via Send protocol to topic)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAnnouncement {
    /// BLAKE3 hash of complete file
    pub hash: [u8; 32],
    /// Who has the file (source endpoint ID)
    pub source_id: [u8; 32],
    /// File size in bytes
    pub total_size: u64,
    /// Number of 512 KB chunks
    pub total_chunks: u32,
    /// Number of sections for distribution
    pub num_sections: u8,
    /// Human-readable filename
    pub display_name: String,
    /// BLAKE3 root hash for verification
    pub merkle_root: [u8; 32],
    /// Initial recipients with their assigned sections
    /// (endpoint_id, section_id, chunk_start, chunk_end)
    pub initial_recipients: Vec<InitialRecipient>,
}

/// Initial recipient in FileAnnouncement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitialRecipient {
    pub endpoint_id: [u8; 32],
    pub section_id: u8,
    pub chunk_start: u32,
    pub chunk_end: u32,
}

impl FileAnnouncement {
    /// Encode for transmission (via Send protocol)
    pub fn encode(&self) -> Vec<u8> {
        // Prefix with a type byte so we can distinguish from other topic messages
        let mut bytes = vec![0xF1]; // FileAnnouncement marker
        let payload = postcard::to_allocvec(self).expect("serialization should not fail");
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }

    /// Decode from bytes
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < 5 || bytes[0] != 0xF1 {
            return Err(DecodeError::InvalidFormat);
        }
        let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
        let total_len = 5usize.checked_add(len).ok_or(DecodeError::InvalidFormat)?;
        if bytes.len() != total_len {
            return if bytes.len() < total_len {
                Err(DecodeError::TooShort)
            } else {
                Err(DecodeError::InvalidFormat)
            };
        }

        decode_structured_payload(&bytes[5..total_len])
    }
}

/// CanSeed message (sent via Send protocol when peer reaches 100%)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanSeed {
    /// File hash
    pub hash: [u8; 32],
    /// Peer that can now seed
    pub endpoint_id: [u8; 32],
}

impl CanSeed {
    /// Encode for transmission
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![0xF2]; // CanSeed marker
        bytes.extend_from_slice(&self.hash);
        bytes.extend_from_slice(&self.endpoint_id);
        bytes
    }

    /// Decode from bytes
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() != 65 || bytes[0] != 0xF2 {
            return Err(DecodeError::InvalidFormat);
        }
        let mut hash = [0u8; 32];
        let mut endpoint_id = [0u8; 32];
        hash.copy_from_slice(&bytes[1..33]);
        endpoint_id.copy_from_slice(&bytes[33..65]);
        Ok(Self { hash, endpoint_id })
    }
}

/// Request chunk map from source (direct connection)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMapRequest {
    pub hash: [u8; 32],
}

/// Response with peer chunk information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMapResponse {
    pub hash: [u8; 32],
    /// Peers and their chunk ranges
    pub peers: Vec<PeerChunks>,
}

/// Peer chunk information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerChunks {
    pub endpoint_id: [u8; 32],
    pub chunk_start: u32,
    pub chunk_end: u32,
}

/// Request a single chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRequest {
    pub hash: [u8; 32],
    /// Chunk index to request
    pub chunk_index: u32,
}

/// Response with chunk data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkResponse {
    pub hash: [u8; 32],
    pub chunk_index: u32,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Bitfield showing which chunks a peer has
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitfieldMessage {
    pub hash: [u8; 32],
    /// Compact bitfield (1 bit per chunk)
    #[serde(with = "serde_bytes")]
    pub bitfield: Vec<u8>,
}

impl BitfieldMessage {
    /// Create from a boolean vector
    pub fn from_chunks(hash: [u8; 32], chunks: &[bool]) -> Self {
        let mut bitfield = vec![0u8; (chunks.len() + 7) / 8];
        for (i, &has_chunk) in chunks.iter().enumerate() {
            if has_chunk {
                bitfield[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        Self { hash, bitfield }
    }

    /// Convert to boolean vector
    pub fn to_chunks(&self, total_chunks: u32) -> Vec<bool> {
        let mut chunks = vec![false; total_chunks as usize];
        for (i, chunk) in chunks.iter_mut().enumerate() {
            if i / 8 < self.bitfield.len() {
                *chunk = (self.bitfield[i / 8] & (1 << (7 - (i % 8)))) != 0;
            }
        }
        chunks
    }
}

/// Suggest another peer when busy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSuggestion {
    pub hash: [u8; 32],
    pub section_id: u8,
    pub suggested_peer: [u8; 32],
}

/// Chunk acknowledgment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkAck {
    pub hash: [u8; 32],
    pub chunk_index: u32,
}

/// Share protocol message (for direct connections)
#[derive(Debug, Clone)]
pub enum ShareMessage {
    ChunkMapRequest(ChunkMapRequest),
    ChunkMapResponse(ChunkMapResponse),
    ChunkRequest(ChunkRequest),
    ChunkResponse(ChunkResponse),
    Bitfield(BitfieldMessage),
    PeerSuggestion(PeerSuggestion),
    ChunkAck(ChunkAck),
}

impl ShareMessage {
    /// Encode message for transmission using shared wire framing
    pub fn encode(&self) -> Vec<u8> {
        let (msg_type, payload) = match self {
            ShareMessage::ChunkMapRequest(msg) => (
                ShareMessageType::ChunkMapRequest,
                postcard::to_allocvec(msg).expect("serialization should not fail"),
            ),
            ShareMessage::ChunkMapResponse(msg) => (
                ShareMessageType::ChunkMapResponse,
                postcard::to_allocvec(msg).expect("serialization should not fail"),
            ),
            ShareMessage::ChunkRequest(msg) => (
                ShareMessageType::ChunkRequest,
                postcard::to_allocvec(msg).expect("serialization should not fail"),
            ),
            ShareMessage::ChunkResponse(msg) => (
                ShareMessageType::ChunkResponse,
                postcard::to_allocvec(msg).expect("serialization should not fail"),
            ),
            ShareMessage::Bitfield(msg) => (
                ShareMessageType::Bitfield,
                postcard::to_allocvec(msg).expect("serialization should not fail"),
            ),
            ShareMessage::PeerSuggestion(msg) => (
                ShareMessageType::PeerSuggestion,
                postcard::to_allocvec(msg).expect("serialization should not fail"),
            ),
            ShareMessage::ChunkAck(msg) => (
                ShareMessageType::ChunkAck,
                postcard::to_allocvec(msg).expect("serialization should not fail"),
            ),
        };
        wire::encode_frame(msg_type as u8, &payload)
    }

    /// Decode message from bytes
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let frame = wire::decode_frame_exact(bytes).map_err(map_frame_error)?;
        Self::decode_payload(frame.msg_type, frame.payload)
    }

    /// Decode message from bytes, returning both the message and bytes consumed
    /// This is useful for parsing multiple concatenated messages
    pub fn decode_with_size(bytes: &[u8]) -> Result<(Self, usize), DecodeError> {
        let frame = wire::decode_frame(bytes).map_err(map_frame_error)?;
        let msg = Self::decode_payload(frame.msg_type, frame.payload)?;

        Ok((msg, frame.total_size))
    }

    fn decode_payload(msg_type_byte: u8, payload: &[u8]) -> Result<Self, DecodeError> {
        let msg_type = ShareMessageType::try_from(msg_type_byte)
            .map_err(|_| DecodeError::UnknownType(msg_type_byte))?;

        match msg_type {
            ShareMessageType::ChunkMapRequest => Ok(ShareMessage::ChunkMapRequest(
                decode_structured_payload(payload)?,
            )),
            ShareMessageType::ChunkMapResponse => Ok(ShareMessage::ChunkMapResponse(
                decode_structured_payload(payload)?,
            )),
            ShareMessageType::ChunkRequest => Ok(ShareMessage::ChunkRequest(
                decode_structured_payload(payload)?,
            )),
            ShareMessageType::ChunkResponse => Ok(ShareMessage::ChunkResponse(
                decode_structured_payload(payload)?,
            )),
            ShareMessageType::Bitfield => {
                Ok(ShareMessage::Bitfield(decode_structured_payload(payload)?))
            }
            ShareMessageType::PeerSuggestion => Ok(ShareMessage::PeerSuggestion(
                decode_structured_payload(payload)?,
            )),
            ShareMessageType::ChunkAck => {
                Ok(ShareMessage::ChunkAck(decode_structured_payload(payload)?))
            }
        }
    }
}

fn map_frame_error(e: wire::FrameError) -> DecodeError {
    match e {
        wire::FrameError::TooShort => DecodeError::TooShort,
        wire::FrameError::TrailingBytes => DecodeError::InvalidFormat,
        wire::FrameError::PayloadTooLarge | wire::FrameError::LengthOverflow => {
            DecodeError::InvalidFormat
        }
    }
}

fn decode_structured_payload<T>(payload: &[u8]) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
{
    let (msg, rest) = postcard::take_from_bytes::<T>(payload)
        .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
    if !rest.is_empty() {
        return Err(DecodeError::InvalidPayload(format!(
            "trailing payload bytes: {}",
            rest.len()
        )));
    }
    Ok(msg)
}

/// Error decoding a message
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Invalid format
    InvalidFormat,
    /// Message too short
    TooShort,
    /// Unknown message type
    UnknownType(u8),
    /// Invalid payload
    InvalidPayload(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::InvalidFormat => write!(f, "invalid message format"),
            DecodeError::TooShort => write!(f, "message too short"),
            DecodeError::UnknownType(t) => write!(f, "unknown message type: 0x{:02x}", t),
            DecodeError::InvalidPayload(e) => write!(f, "invalid payload: {}", e),
        }
    }
}

impl std::error::Error for DecodeError {}

// NOTE: FileAnnouncement and CanSeed for topic-wide broadcasts are handled as
// TopicMessage variants (FileAnnouncementMessage, CanSeedMessage) in network/packet.rs.
//
// The FileAnnouncement and CanSeed structs in this module are used for
// direct peer-to-peer communication over the SHARE_ALPN protocol.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_announcement_roundtrip() {
        let ann = FileAnnouncement {
            hash: [1u8; 32],
            source_id: [2u8; 32],
            total_size: 1024 * 1024,
            total_chunks: 2,
            num_sections: 3,
            display_name: "test.bin".to_string(),
            merkle_root: [3u8; 32],
            initial_recipients: vec![InitialRecipient {
                endpoint_id: [4u8; 32],
                section_id: 0,
                chunk_start: 0,
                chunk_end: 1,
            }],
        };

        let encoded = ann.encode();
        let decoded = FileAnnouncement::decode(&encoded).unwrap();

        assert_eq!(decoded.hash, ann.hash);
        assert_eq!(decoded.display_name, ann.display_name);
        assert_eq!(decoded.total_chunks, ann.total_chunks);
    }

    #[test]
    fn test_can_seed_roundtrip() {
        let msg = CanSeed {
            hash: [1u8; 32],
            endpoint_id: [2u8; 32],
        };

        let encoded = msg.encode();
        let decoded = CanSeed::decode(&encoded).unwrap();

        assert_eq!(decoded.hash, msg.hash);
        assert_eq!(decoded.endpoint_id, msg.endpoint_id);
    }

    #[test]
    fn test_share_message_roundtrip() {
        let request = ShareMessage::ChunkRequest(ChunkRequest {
            hash: [1u8; 32],
            chunk_index: 5,
        });

        let encoded = request.encode();
        let decoded = ShareMessage::decode(&encoded).unwrap();

        match decoded {
            ShareMessage::ChunkRequest(req) => {
                assert_eq!(req.hash, [1u8; 32]);
                assert_eq!(req.chunk_index, 5);
            }
            _ => panic!("expected ChunkRequest"),
        }
    }

    #[test]
    fn test_bitfield() {
        let chunks = vec![true, false, true, true, false, false, true, false, true];
        let msg = BitfieldMessage::from_chunks([1u8; 32], &chunks);

        let decoded = msg.to_chunks(9);
        assert_eq!(decoded, chunks);
    }

    #[test]
    fn test_chunk_response_roundtrip() {
        let response = ShareMessage::ChunkResponse(ChunkResponse {
            hash: [5u8; 32],
            chunk_index: 42,
            data: vec![1, 2, 3, 4, 5, 6, 7, 8],
        });

        let encoded = response.encode();
        let decoded = ShareMessage::decode(&encoded).unwrap();

        match decoded {
            ShareMessage::ChunkResponse(resp) => {
                assert_eq!(resp.hash, [5u8; 32]);
                assert_eq!(resp.chunk_index, 42);
                assert_eq!(resp.data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
            }
            _ => panic!("expected ChunkResponse"),
        }
    }

    #[test]
    fn test_chunk_map_request_roundtrip() {
        let request = ShareMessage::ChunkMapRequest(ChunkMapRequest { hash: [6u8; 32] });

        let encoded = request.encode();
        let decoded = ShareMessage::decode(&encoded).unwrap();

        match decoded {
            ShareMessage::ChunkMapRequest(req) => {
                assert_eq!(req.hash, [6u8; 32]);
            }
            _ => panic!("expected ChunkMapRequest"),
        }
    }

    #[test]
    fn test_chunk_map_response_roundtrip() {
        let response = ShareMessage::ChunkMapResponse(ChunkMapResponse {
            hash: [7u8; 32],
            peers: vec![
                PeerChunks {
                    endpoint_id: [10u8; 32],
                    chunk_start: 0,
                    chunk_end: 50,
                },
                PeerChunks {
                    endpoint_id: [11u8; 32],
                    chunk_start: 50,
                    chunk_end: 100,
                },
            ],
        });

        let encoded = response.encode();
        let decoded = ShareMessage::decode(&encoded).unwrap();

        match decoded {
            ShareMessage::ChunkMapResponse(resp) => {
                assert_eq!(resp.hash, [7u8; 32]);
                assert_eq!(resp.peers.len(), 2);
                assert_eq!(resp.peers[0].chunk_start, 0);
                assert_eq!(resp.peers[0].chunk_end, 50);
                assert_eq!(resp.peers[1].chunk_start, 50);
                assert_eq!(resp.peers[1].chunk_end, 100);
            }
            _ => panic!("expected ChunkMapResponse"),
        }
    }

    #[test]
    fn test_peer_suggestion_roundtrip() {
        let suggestion = ShareMessage::PeerSuggestion(PeerSuggestion {
            hash: [8u8; 32],
            section_id: 3,
            suggested_peer: [20u8; 32],
        });

        let encoded = suggestion.encode();
        let decoded = ShareMessage::decode(&encoded).unwrap();

        match decoded {
            ShareMessage::PeerSuggestion(sugg) => {
                assert_eq!(sugg.hash, [8u8; 32]);
                assert_eq!(sugg.section_id, 3);
                assert_eq!(sugg.suggested_peer, [20u8; 32]);
            }
            _ => panic!("expected PeerSuggestion"),
        }
    }

    #[test]
    fn test_chunk_ack_roundtrip() {
        let ack = ShareMessage::ChunkAck(ChunkAck {
            hash: [9u8; 32],
            chunk_index: 100,
        });

        let encoded = ack.encode();
        let decoded = ShareMessage::decode(&encoded).unwrap();

        match decoded {
            ShareMessage::ChunkAck(a) => {
                assert_eq!(a.hash, [9u8; 32]);
                assert_eq!(a.chunk_index, 100);
            }
            _ => panic!("expected ChunkAck"),
        }
    }

    #[test]
    fn test_decode_error_too_short() {
        let result = ShareMessage::decode(&[0, 0, 0]);
        assert!(matches!(result, Err(DecodeError::TooShort)));
    }

    #[test]
    fn test_decode_error_unknown_type() {
        let bytes = vec![0xFF, 0, 0, 0, 1, 0]; // Unknown type 0xFF
        let result = ShareMessage::decode(&bytes);
        assert!(matches!(result, Err(DecodeError::UnknownType(0xFF))));
    }

    #[test]
    fn test_decode_error_truncated_payload() {
        // Type 0x01 (ChunkMapRequest), length says 100 bytes, but only 10 provided
        // bytes[0] = type, bytes[1..5] = length (big-endian)
        let mut bytes = vec![0x01u8]; // type = ChunkMapRequest
        bytes.extend_from_slice(&100u32.to_be_bytes()); // length = 100
        bytes.extend_from_slice(&[0u8; 10]); // only 10 bytes of payload

        let result = ShareMessage::decode(&bytes);
        assert!(
            matches!(result, Err(DecodeError::TooShort)),
            "Expected TooShort error, got {:?}",
            result
        );
    }

    #[test]
    fn test_decode_rejects_trailing_frame_bytes() {
        let request = ShareMessage::ChunkRequest(ChunkRequest {
            hash: [0xAA; 32],
            chunk_index: 9,
        });
        let mut encoded = request.encode();
        encoded.push(0xFF);

        let result = ShareMessage::decode(&encoded);
        assert!(
            matches!(result, Err(DecodeError::InvalidFormat)),
            "Expected InvalidFormat for trailing bytes, got {:?}",
            result
        );
    }

    #[test]
    fn test_decode_with_size_allows_concatenated_frames() {
        let first = ShareMessage::ChunkAck(ChunkAck {
            hash: [0x10; 32],
            chunk_index: 7,
        })
        .encode();
        let second = ShareMessage::Bitfield(BitfieldMessage::from_chunks(
            [0x11; 32],
            &[true, false, true, false],
        ))
        .encode();

        let mut combined = first.clone();
        combined.extend_from_slice(&second);

        let (decoded, consumed) = ShareMessage::decode_with_size(&combined).unwrap();
        assert_eq!(consumed, first.len());
        match decoded {
            ShareMessage::ChunkAck(ack) => {
                assert_eq!(ack.hash, [0x10; 32]);
                assert_eq!(ack.chunk_index, 7);
            }
            _ => panic!("expected ChunkAck"),
        }
    }

    #[test]
    fn test_decode_rejects_trailing_payload_bytes() {
        let payload = postcard::to_allocvec(&ChunkRequest {
            hash: [0x22; 32],
            chunk_index: 3,
        })
        .expect("serialize");
        let mut payload_with_trailing = payload.clone();
        payload_with_trailing.push(0x00);

        let encoded =
            wire::encode_frame(ShareMessageType::ChunkRequest as u8, &payload_with_trailing);
        let result = ShareMessage::decode(&encoded);
        assert!(
            matches!(result, Err(DecodeError::InvalidPayload(_))),
            "Expected InvalidPayload for trailing payload bytes, got {:?}",
            result
        );
    }

    #[test]
    fn test_bitfield_large() {
        // Test with more than 8 chunks (requires multiple bytes)
        let chunks = vec![
            true, false, true, true, false, false, true, false, // byte 0
            true, true, false, true, false, false, false, true, // byte 1
            true, // byte 2 (partial)
        ];
        let msg = BitfieldMessage::from_chunks([1u8; 32], &chunks);

        let decoded = msg.to_chunks(17);
        assert_eq!(decoded, chunks);
    }

    #[test]
    fn test_bitfield_empty() {
        let chunks: Vec<bool> = vec![];
        let msg = BitfieldMessage::from_chunks([1u8; 32], &chunks);

        let decoded = msg.to_chunks(0);
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_bitfield_all_true() {
        let chunks = vec![true; 100];
        let msg = BitfieldMessage::from_chunks([1u8; 32], &chunks);

        let decoded = msg.to_chunks(100);
        assert_eq!(decoded, chunks);
    }

    #[test]
    fn test_bitfield_all_false() {
        let chunks = vec![false; 100];
        let msg = BitfieldMessage::from_chunks([1u8; 32], &chunks);

        let decoded = msg.to_chunks(100);
        assert_eq!(decoded, chunks);
    }

    #[test]
    fn test_file_announcement_many_recipients() {
        let ann = FileAnnouncement {
            hash: [1u8; 32],
            source_id: [2u8; 32],
            total_size: 100 * 1024 * 1024, // 100MB
            total_chunks: 200,
            num_sections: 5,
            display_name: "large_video.mp4".to_string(),
            merkle_root: [3u8; 32],
            initial_recipients: (0..5u32)
                .map(|i| InitialRecipient {
                    endpoint_id: [i as u8; 32],
                    section_id: i as u8,
                    chunk_start: i * 40,
                    chunk_end: (i + 1) * 40,
                })
                .collect(),
        };

        let encoded = ann.encode();
        let decoded = FileAnnouncement::decode(&encoded).unwrap();

        assert_eq!(decoded.total_chunks, 200);
        assert_eq!(decoded.initial_recipients.len(), 5);
        assert_eq!(decoded.initial_recipients[2].section_id, 2);
        assert_eq!(decoded.initial_recipients[2].chunk_start, 80);
        assert_eq!(decoded.initial_recipients[2].chunk_end, 120);
    }

    #[test]
    fn test_file_announcement_decode_rejects_trailing_bytes() {
        let ann = FileAnnouncement {
            hash: [3u8; 32],
            source_id: [4u8; 32],
            total_size: 1024,
            total_chunks: 2,
            num_sections: 1,
            display_name: "x.bin".to_string(),
            merkle_root: [5u8; 32],
            initial_recipients: Vec::new(),
        };

        let mut encoded = ann.encode();
        encoded.push(0x00);

        let result = FileAnnouncement::decode(&encoded);
        assert!(
            matches!(result, Err(DecodeError::InvalidFormat)),
            "Expected InvalidFormat for trailing bytes, got {:?}",
            result
        );
    }
}
