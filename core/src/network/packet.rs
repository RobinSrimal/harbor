//! Unified packet type system for Harbor Protocol
//!
//! This module provides the complete packet type classification and payload encoding
//! for all messages that flow through the harbor store-and-forward system.
//!
//! # Byte Layout
//!
//! - 0x00-0x0F: Topic-scoped (broadcast to topic members)
//! - 0x40-0x4F: DM-scoped (point-to-point)
//! - 0x50-0x5F: Stream signaling (point-to-point, request/response patterns)
//! - 0x80-0xBF: Control (connection, membership, introductions)
//!
//! Harbor stores opaque encrypted bytes. Recipients derive decryption key
//! from the harbor_id they're pulling from, then read plaintext[0] for PacketType.

use serde::{Deserialize, Serialize};

// =============================================================================
// PacketType - Unified type classification
// =============================================================================

/// Unified packet type for all harbor-routed messages
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketType {
    // =========================================================================
    // Topic-scoped (0x00-0x0F) - Broadcast to topic members
    // Encryption: Topic epoch key
    // Harbor ID: hash(topic_id)
    // =========================================================================
    /// Text/binary message content
    TopicContent = 0x00,
    /// File announcement with metadata
    TopicFileAnnounce = 0x01,
    /// Announce ability to seed a file
    TopicCanSeed = 0x02,
    /// CRDT sync state update
    TopicSyncUpdate = 0x03,
    /// Request sync state from peers
    TopicSyncRequest = 0x04,
    /// Request to initiate a stream
    TopicStreamRequest = 0x05,

    // =========================================================================
    // DM-scoped (0x40-0x4F) - Point-to-point
    // Encryption: DM shared key (ECDH)
    // Harbor ID: recipient_id
    // =========================================================================
    /// Direct message content
    DmContent = 0x40,
    /// File announcement in DM
    DmFileAnnounce = 0x41,
    /// CRDT sync update in DM context
    DmSyncUpdate = 0x42,
    /// Request sync state in DM context
    DmSyncRequest = 0x43,
    /// Request to initiate a stream in DM
    DmStreamRequest = 0x44,

    // =========================================================================
    // Stream signaling (0x50-0x5F) - Point-to-point stream control
    // Encryption: DM shared key (ECDH)
    // Harbor ID: recipient_id
    // =========================================================================
    /// Accept a stream request
    StreamAccept = 0x50,
    /// Reject a stream request
    StreamReject = 0x51,
    /// Query stream status
    StreamQuery = 0x52,
    /// Notify stream is active
    StreamActive = 0x53,
    /// Notify stream has ended
    StreamEnded = 0x54,

    // =========================================================================
    // Control (0x80-0xBF) - Connection, membership, introductions
    // =========================================================================
    /// Request peer connection (DM key, harbor_id = recipient_id)
    ConnectRequest = 0x80,
    /// Accept connection request (DM key, harbor_id = recipient_id)
    ConnectAccept = 0x81,
    /// Decline connection request (DM key, harbor_id = recipient_id)
    ConnectDecline = 0x82,
    /// Invite peer to topic (DM key, harbor_id = recipient_id)
    TopicInvite = 0x83,
    /// Announce joining a topic (Topic key, harbor_id = hash(topic_id))
    TopicJoin = 0x84,
    /// Announce leaving a topic (Topic key, harbor_id = hash(topic_id))
    TopicLeave = 0x85,
    /// Remove member + key rotation (DM key, harbor_id = recipient_id)
    RemoveMember = 0x86,
    /// Suggest peer introduction (DM key, harbor_id = recipient_id)
    Suggest = 0x87,
}

/// Scope of a packet (determines encryption and routing patterns)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Broadcast to topic members
    Topic,
    /// Point-to-point direct message
    Dm,
    /// Stream signaling (point-to-point)
    StreamSignaling,
    /// Control messages (mixed routing)
    Control,
}

/// Verification mode for packet authentication
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMode {
    /// Full verification: MAC + signature check
    Full,
    /// MAC only: for packets where sender may be unknown (TopicJoin)
    MacOnly,
}

/// Source of harbor_id for routing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarborIdSource {
    /// hash(topic_id) - for topic-scoped messages
    TopicHash,
    /// recipient's endpoint_id - for DM/control messages
    RecipientId,
}

/// Encryption key type for packet encryption
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionKeyType {
    /// Topic epoch key (symmetric, derived from topic secret)
    TopicKey,
    /// DM shared key (ECDH between sender and recipient)
    DmKey,
}

impl PacketType {
    /// Convert from byte value
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            // Topic
            0x00 => Some(Self::TopicContent),
            0x01 => Some(Self::TopicFileAnnounce),
            0x02 => Some(Self::TopicCanSeed),
            0x03 => Some(Self::TopicSyncUpdate),
            0x04 => Some(Self::TopicSyncRequest),
            0x05 => Some(Self::TopicStreamRequest),
            // DM
            0x40 => Some(Self::DmContent),
            0x41 => Some(Self::DmFileAnnounce),
            0x42 => Some(Self::DmSyncUpdate),
            0x43 => Some(Self::DmSyncRequest),
            0x44 => Some(Self::DmStreamRequest),
            // Stream signaling
            0x50 => Some(Self::StreamAccept),
            0x51 => Some(Self::StreamReject),
            0x52 => Some(Self::StreamQuery),
            0x53 => Some(Self::StreamActive),
            0x54 => Some(Self::StreamEnded),
            // Control
            0x80 => Some(Self::ConnectRequest),
            0x81 => Some(Self::ConnectAccept),
            0x82 => Some(Self::ConnectDecline),
            0x83 => Some(Self::TopicInvite),
            0x84 => Some(Self::TopicJoin),
            0x85 => Some(Self::TopicLeave),
            0x86 => Some(Self::RemoveMember),
            0x87 => Some(Self::Suggest),
            _ => None,
        }
    }

    /// Convert to byte value
    pub fn as_byte(self) -> u8 {
        self as u8
    }

    /// Get the scope of this packet type
    pub fn scope(self) -> Scope {
        match self {
            Self::TopicContent
            | Self::TopicFileAnnounce
            | Self::TopicCanSeed
            | Self::TopicSyncUpdate
            | Self::TopicSyncRequest
            | Self::TopicStreamRequest => Scope::Topic,

            Self::DmContent
            | Self::DmFileAnnounce
            | Self::DmSyncUpdate
            | Self::DmSyncRequest
            | Self::DmStreamRequest => Scope::Dm,

            Self::StreamAccept
            | Self::StreamReject
            | Self::StreamQuery
            | Self::StreamActive
            | Self::StreamEnded => Scope::StreamSignaling,

            Self::ConnectRequest
            | Self::ConnectAccept
            | Self::ConnectDecline
            | Self::TopicInvite
            | Self::TopicJoin
            | Self::TopicLeave
            | Self::RemoveMember
            | Self::Suggest => Scope::Control,
        }
    }

    /// Get verification mode for this packet type
    ///
    /// TopicJoin uses MacOnly because the sender may be unknown to some
    /// recipients (they haven't connected before).
    pub fn verification_mode(self) -> VerificationMode {
        match self {
            Self::TopicJoin => VerificationMode::MacOnly,
            _ => VerificationMode::Full,
        }
    }

    /// Get harbor_id source for routing
    ///
    /// Most packets use recipient_id. Topic-scoped packets and TopicJoin/TopicLeave
    /// use hash(topic_id) since they're broadcast to all topic members.
    pub fn harbor_id_source(self) -> HarborIdSource {
        match self {
            // Topic messages -> hash(topic_id)
            Self::TopicContent
            | Self::TopicFileAnnounce
            | Self::TopicCanSeed
            | Self::TopicSyncUpdate
            | Self::TopicSyncRequest
            | Self::TopicStreamRequest => HarborIdSource::TopicHash,

            // TopicJoin/TopicLeave broadcast to topic members
            Self::TopicJoin | Self::TopicLeave => HarborIdSource::TopicHash,

            // Everything else -> recipient_id
            _ => HarborIdSource::RecipientId,
        }
    }

    /// Get encryption key type for this packet
    ///
    /// Topic-scoped messages use topic epoch key.
    /// TopicJoin/TopicLeave also use topic key (broadcast to members).
    /// Everything else uses DM shared key.
    pub fn encryption_key_type(self) -> EncryptionKeyType {
        match self {
            // Topic messages use topic key
            Self::TopicContent
            | Self::TopicFileAnnounce
            | Self::TopicCanSeed
            | Self::TopicSyncUpdate
            | Self::TopicSyncRequest
            | Self::TopicStreamRequest => EncryptionKeyType::TopicKey,

            // TopicJoin/TopicLeave use topic key (broadcast to members)
            Self::TopicJoin | Self::TopicLeave => EncryptionKeyType::TopicKey,

            // Everything else uses DM key
            _ => EncryptionKeyType::DmKey,
        }
    }

    /// Check if this is a topic-scoped packet
    pub fn is_topic(self) -> bool {
        matches!(self.scope(), Scope::Topic)
    }

    /// Check if this is a DM-scoped packet
    pub fn is_dm(self) -> bool {
        matches!(self.scope(), Scope::Dm)
    }

    /// Check if this is a stream signaling packet
    pub fn is_stream_signaling(self) -> bool {
        matches!(self.scope(), Scope::StreamSignaling)
    }

    /// Check if this is a control packet
    pub fn is_control(self) -> bool {
        matches!(self.scope(), Scope::Control)
    }

    /// Check if this packet type emits an event to the application
    ///
    /// Some packets are handled internally (e.g., CanSeed updates seeder records)
    /// while others emit events for the application to process.
    pub fn emits_event(self) -> bool {
        match self {
            // Content messages emit events
            Self::TopicContent | Self::DmContent => true,

            // File announcements emit events
            Self::TopicFileAnnounce | Self::DmFileAnnounce => true,

            // Sync updates/requests emit events for CRDT processing
            Self::TopicSyncUpdate
            | Self::TopicSyncRequest
            | Self::DmSyncUpdate
            | Self::DmSyncRequest => true,

            // Stream requests emit events
            Self::TopicStreamRequest | Self::DmStreamRequest => true,

            // Stream signaling emits events
            Self::StreamAccept
            | Self::StreamReject
            | Self::StreamQuery
            | Self::StreamActive
            | Self::StreamEnded => true,

            // Control messages that need app attention
            Self::ConnectRequest | Self::ConnectAccept | Self::ConnectDecline => true,
            Self::TopicInvite => true,
            Self::Suggest => true,

            // Internal handling only
            Self::TopicCanSeed => false,     // Updates seeder records internally
            Self::TopicJoin => false,        // Updates membership internally
            Self::TopicLeave => false,       // Updates membership internally
            Self::RemoveMember => false,     // Updates epoch key internally
        }
    }
}

// =============================================================================
// Payload Structs - Shared between Topic and DM contexts
// =============================================================================

/// File announcement message - announces a new shared file
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

/// CRDT sync update message (delta)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncUpdateMessage {
    /// Raw CRDT delta bytes
    pub data: Vec<u8>,
}

/// Live stream request message (topic-scoped, broadcast)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRequestMessage {
    /// Unique stream session ID
    pub request_id: [u8; 32],
    /// Stream name (e.g., "camera", "screen")
    pub name: String,
    /// Serialized catalog metadata (codecs, renditions)
    #[serde(with = "serde_bytes")]
    pub catalog: Vec<u8>,
}

/// DM stream request message (point-to-point)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmStreamRequestMessage {
    /// Unique request ID
    pub request_id: [u8; 32],
    /// Stream name (e.g., "camera", "screen")
    pub stream_name: String,
}

// =============================================================================
// Stream Signaling Payloads
// =============================================================================

/// Stream accept message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamAcceptMessage {
    /// Correlates to original StreamRequest
    pub request_id: [u8; 32],
}

/// Stream reject message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRejectMessage {
    /// Correlates to original StreamRequest
    pub request_id: [u8; 32],
    /// Optional reason for rejection
    pub reason: Option<String>,
}

/// Stream liveness query message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamQueryMessage {
    /// Correlates to original StreamRequest
    pub request_id: [u8; 32],
}

/// Stream active response message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamActiveMessage {
    /// Correlates to original StreamRequest
    pub request_id: [u8; 32],
}

/// Stream ended response message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamEndedMessage {
    /// Correlates to original StreamRequest
    pub request_id: [u8; 32],
}

// =============================================================================
// TopicMessage - Wrapper enum for topic-scoped messages
// =============================================================================

/// A topic message (broadcast to topic members)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicMessage {
    /// Regular content (the payload itself)
    Content(Vec<u8>),
    /// File share announcement (source is sharing a new file)
    FileAnnouncement(FileAnnouncementMessage),
    /// Can seed announcement (peer has complete file, can serve)
    CanSeed(CanSeedMessage),
    /// CRDT sync update (delta bytes)
    SyncUpdate(SyncUpdateMessage),
    /// Sync request (asking peers for their current state)
    SyncRequest,
    /// Live stream request (source → all topic members)
    StreamRequest(StreamRequestMessage),
}

impl TopicMessage {
    /// Encode the message for inclusion in a Send packet payload
    pub fn encode(&self) -> Vec<u8> {
        match self {
            TopicMessage::Content(data) => {
                let mut bytes = Vec::with_capacity(1 + data.len());
                bytes.push(PacketType::TopicContent.as_byte());
                bytes.extend_from_slice(data);
                bytes
            }
            TopicMessage::FileAnnouncement(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::TopicFileAnnounce.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
            TopicMessage::CanSeed(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::TopicCanSeed.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
            TopicMessage::SyncUpdate(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::TopicSyncUpdate.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
            TopicMessage::SyncRequest => {
                vec![PacketType::TopicSyncRequest.as_byte()]
            }
            TopicMessage::StreamRequest(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::TopicStreamRequest.as_byte());
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

        let packet_type = PacketType::from_byte(bytes[0])
            .ok_or(DecodeError::UnknownType(bytes[0]))?;

        match packet_type {
            PacketType::TopicContent => Ok(TopicMessage::Content(bytes[1..].to_vec())),
            PacketType::TopicFileAnnounce => {
                let msg: FileAnnouncementMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(TopicMessage::FileAnnouncement(msg))
            }
            PacketType::TopicCanSeed => {
                let msg: CanSeedMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(TopicMessage::CanSeed(msg))
            }
            PacketType::TopicSyncUpdate => {
                let msg: SyncUpdateMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(TopicMessage::SyncUpdate(msg))
            }
            PacketType::TopicSyncRequest => Ok(TopicMessage::SyncRequest),
            PacketType::TopicStreamRequest => {
                let msg: StreamRequestMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(TopicMessage::StreamRequest(msg))
            }
            _ => Err(DecodeError::UnknownType(bytes[0])),
        }
    }

    /// Check if this is a control message (FileAnnouncement or CanSeed)
    pub fn is_control(&self) -> bool {
        matches!(
            self,
            TopicMessage::FileAnnouncement(_) | TopicMessage::CanSeed(_)
        )
    }

    /// Get the content if this is a content message
    pub fn as_content(&self) -> Option<&[u8]> {
        match self {
            TopicMessage::Content(data) => Some(data),
            _ => None,
        }
    }

    /// Get the packet type
    pub fn packet_type(&self) -> PacketType {
        match self {
            TopicMessage::Content(_) => PacketType::TopicContent,
            TopicMessage::FileAnnouncement(_) => PacketType::TopicFileAnnounce,
            TopicMessage::CanSeed(_) => PacketType::TopicCanSeed,
            TopicMessage::SyncUpdate(_) => PacketType::TopicSyncUpdate,
            TopicMessage::SyncRequest => PacketType::TopicSyncRequest,
            TopicMessage::StreamRequest(_) => PacketType::TopicStreamRequest,
        }
    }
}

// =============================================================================
// DmMessage - Wrapper enum for DM-scoped messages
// =============================================================================

/// A direct message (point-to-point)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmMessage {
    /// Content payload (app defines format)
    Content(Vec<u8>),
    /// File share announcement
    FileAnnouncement(FileAnnouncementMessage),
    /// Sync update (CRDT delta bytes)
    SyncUpdate(SyncUpdateMessage),
    /// Sync request (request full state)
    SyncRequest,
    /// Stream request (DM-scoped peer-to-peer)
    StreamRequest(DmStreamRequestMessage),
}

impl DmMessage {
    /// Encode the message for inclusion in a DM packet payload
    pub fn encode(&self) -> Vec<u8> {
        match self {
            DmMessage::Content(data) => {
                let mut bytes = Vec::with_capacity(1 + data.len());
                bytes.push(PacketType::DmContent.as_byte());
                bytes.extend_from_slice(data);
                bytes
            }
            DmMessage::FileAnnouncement(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::DmFileAnnounce.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
            DmMessage::SyncUpdate(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::DmSyncUpdate.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
            DmMessage::SyncRequest => {
                vec![PacketType::DmSyncRequest.as_byte()]
            }
            DmMessage::StreamRequest(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::DmStreamRequest.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
        }
    }

    /// Decode a message from a DM packet payload
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError::Empty);
        }

        let packet_type = PacketType::from_byte(bytes[0])
            .ok_or(DecodeError::UnknownType(bytes[0]))?;

        match packet_type {
            PacketType::DmContent => Ok(DmMessage::Content(bytes[1..].to_vec())),
            PacketType::DmFileAnnounce => {
                let msg: FileAnnouncementMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(DmMessage::FileAnnouncement(msg))
            }
            PacketType::DmSyncUpdate => {
                let msg: SyncUpdateMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(DmMessage::SyncUpdate(msg))
            }
            PacketType::DmSyncRequest => Ok(DmMessage::SyncRequest),
            PacketType::DmStreamRequest => {
                let msg: DmStreamRequestMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(DmMessage::StreamRequest(msg))
            }
            _ => Err(DecodeError::UnknownType(bytes[0])),
        }
    }

    /// Get the packet type
    pub fn packet_type(&self) -> PacketType {
        match self {
            DmMessage::Content(_) => PacketType::DmContent,
            DmMessage::FileAnnouncement(_) => PacketType::DmFileAnnounce,
            DmMessage::SyncUpdate(_) => PacketType::DmSyncUpdate,
            DmMessage::SyncRequest => PacketType::DmSyncRequest,
            DmMessage::StreamRequest(_) => PacketType::DmStreamRequest,
        }
    }
}

// =============================================================================
// StreamSignalingMessage - Wrapper enum for stream signaling
// =============================================================================

/// Stream signaling message (point-to-point responses)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamSignalingMessage {
    /// Accept a stream request
    Accept(StreamAcceptMessage),
    /// Reject a stream request
    Reject(StreamRejectMessage),
    /// Query stream liveness
    Query(StreamQueryMessage),
    /// Stream is active
    Active(StreamActiveMessage),
    /// Stream has ended
    Ended(StreamEndedMessage),
}

impl StreamSignalingMessage {
    /// Encode the message for inclusion in a packet payload
    pub fn encode(&self) -> Vec<u8> {
        match self {
            StreamSignalingMessage::Accept(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::StreamAccept.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
            StreamSignalingMessage::Reject(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::StreamReject.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
            StreamSignalingMessage::Query(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::StreamQuery.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
            StreamSignalingMessage::Active(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::StreamActive.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
            StreamSignalingMessage::Ended(msg) => {
                let payload = postcard::to_allocvec(msg).expect("serialization should not fail");
                let mut bytes = Vec::with_capacity(1 + payload.len());
                bytes.push(PacketType::StreamEnded.as_byte());
                bytes.extend_from_slice(&payload);
                bytes
            }
        }
    }

    /// Decode a message from a packet payload
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError::Empty);
        }

        let packet_type = PacketType::from_byte(bytes[0])
            .ok_or(DecodeError::UnknownType(bytes[0]))?;

        match packet_type {
            PacketType::StreamAccept => {
                let msg: StreamAcceptMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(StreamSignalingMessage::Accept(msg))
            }
            PacketType::StreamReject => {
                let msg: StreamRejectMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(StreamSignalingMessage::Reject(msg))
            }
            PacketType::StreamQuery => {
                let msg: StreamQueryMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(StreamSignalingMessage::Query(msg))
            }
            PacketType::StreamActive => {
                let msg: StreamActiveMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(StreamSignalingMessage::Active(msg))
            }
            PacketType::StreamEnded => {
                let msg: StreamEndedMessage = postcard::from_bytes(&bytes[1..])
                    .map_err(|e| DecodeError::InvalidPayload(e.to_string()))?;
                Ok(StreamSignalingMessage::Ended(msg))
            }
            _ => Err(DecodeError::UnknownType(bytes[0])),
        }
    }

    /// Get the packet type
    pub fn packet_type(&self) -> PacketType {
        match self {
            StreamSignalingMessage::Accept(_) => PacketType::StreamAccept,
            StreamSignalingMessage::Reject(_) => PacketType::StreamReject,
            StreamSignalingMessage::Query(_) => PacketType::StreamQuery,
            StreamSignalingMessage::Active(_) => PacketType::StreamActive,
            StreamSignalingMessage::Ended(_) => PacketType::StreamEnded,
        }
    }
}

// =============================================================================
// Errors
// =============================================================================

/// Error decoding a message
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
            DecodeError::UnknownType(t) => write!(f, "unknown message type: {:#x}", t),
            DecodeError::InvalidPayload(e) => write!(f, "invalid payload: {}", e),
        }
    }
}

impl std::error::Error for DecodeError {}

// =============================================================================
// Helper Functions
// =============================================================================

/// Check if a type byte is in the DM range (0x40-0x4F)
pub fn is_dm_message_type(type_byte: u8) -> bool {
    type_byte >= 0x40 && type_byte < 0x50
}

/// Check if a type byte is in the stream signaling range (0x50-0x5F)
pub fn is_stream_signaling_type(type_byte: u8) -> bool {
    type_byte >= 0x50 && type_byte < 0x60
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // PacketType tests
    #[test]
    fn test_byte_conversion_roundtrip() {
        let types = [
            PacketType::TopicContent,
            PacketType::TopicFileAnnounce,
            PacketType::TopicCanSeed,
            PacketType::TopicSyncUpdate,
            PacketType::TopicSyncRequest,
            PacketType::TopicStreamRequest,
            PacketType::DmContent,
            PacketType::DmFileAnnounce,
            PacketType::DmSyncUpdate,
            PacketType::DmSyncRequest,
            PacketType::DmStreamRequest,
            PacketType::StreamAccept,
            PacketType::StreamReject,
            PacketType::StreamQuery,
            PacketType::StreamActive,
            PacketType::StreamEnded,
            PacketType::ConnectRequest,
            PacketType::ConnectAccept,
            PacketType::ConnectDecline,
            PacketType::TopicInvite,
            PacketType::TopicJoin,
            PacketType::TopicLeave,
            PacketType::RemoveMember,
            PacketType::Suggest,
        ];

        for t in types {
            let byte = t.as_byte();
            let recovered = PacketType::from_byte(byte).expect("should parse");
            assert_eq!(t, recovered);
        }
    }

    #[test]
    fn test_byte_values() {
        assert_eq!(PacketType::TopicContent.as_byte(), 0x00);
        assert_eq!(PacketType::TopicStreamRequest.as_byte(), 0x05);
        assert_eq!(PacketType::DmContent.as_byte(), 0x40);
        assert_eq!(PacketType::DmStreamRequest.as_byte(), 0x44);
        assert_eq!(PacketType::StreamAccept.as_byte(), 0x50);
        assert_eq!(PacketType::StreamEnded.as_byte(), 0x54);
        assert_eq!(PacketType::ConnectRequest.as_byte(), 0x80);
        assert_eq!(PacketType::Suggest.as_byte(), 0x87);
    }

    #[test]
    fn test_invalid_byte() {
        assert!(PacketType::from_byte(0xFF).is_none());
        assert!(PacketType::from_byte(0x10).is_none());
        assert!(PacketType::from_byte(0x60).is_none());
    }

    #[test]
    fn test_scope() {
        assert_eq!(PacketType::TopicContent.scope(), Scope::Topic);
        assert_eq!(PacketType::DmContent.scope(), Scope::Dm);
        assert_eq!(PacketType::StreamAccept.scope(), Scope::StreamSignaling);
        assert_eq!(PacketType::ConnectRequest.scope(), Scope::Control);
        assert_eq!(PacketType::TopicJoin.scope(), Scope::Control);
    }

    #[test]
    fn test_verification_mode() {
        assert_eq!(PacketType::TopicJoin.verification_mode(), VerificationMode::MacOnly);
        assert_eq!(PacketType::TopicContent.verification_mode(), VerificationMode::Full);
        assert_eq!(PacketType::TopicLeave.verification_mode(), VerificationMode::Full);
    }

    // TopicMessage tests
    #[test]
    fn test_topic_content_roundtrip() {
        let original = TopicMessage::Content(b"Hello, world!".to_vec());
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(encoded[0], 0x00);
    }

    #[test]
    fn test_topic_file_announcement_roundtrip() {
        let ann = FileAnnouncementMessage::new(
            [1u8; 32], [2u8; 32], 1024 * 1024, 2, 2, "test_file.bin".to_string(),
        );
        let original = TopicMessage::FileAnnouncement(ann);
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(encoded[0], 0x01);
    }

    #[test]
    fn test_topic_can_seed_roundtrip() {
        let can_seed = CanSeedMessage::new([3u8; 32], [4u8; 32]);
        let original = TopicMessage::CanSeed(can_seed);
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(encoded[0], 0x02);
    }

    #[test]
    fn test_topic_sync_request_roundtrip() {
        let original = TopicMessage::SyncRequest;
        let encoded = original.encode();
        let decoded = TopicMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(encoded, vec![0x04]);
    }

    // DmMessage tests
    #[test]
    fn test_dm_content_roundtrip() {
        let original = DmMessage::Content(b"Hello DM!".to_vec());
        let encoded = original.encode();
        assert_eq!(encoded[0], 0x40);
        let decoded = DmMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_dm_file_announcement_roundtrip() {
        let original = DmMessage::FileAnnouncement(FileAnnouncementMessage::new(
            [1u8; 32], [2u8; 32], 1024 * 1024, 2, 2, "test.bin".to_string(),
        ));
        let encoded = original.encode();
        assert_eq!(encoded[0], 0x41);
        let decoded = DmMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_dm_sync_request_roundtrip() {
        let original = DmMessage::SyncRequest;
        let encoded = original.encode();
        assert_eq!(encoded, vec![0x43]);
        let decoded = DmMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_dm_stream_request_roundtrip() {
        let original = DmMessage::StreamRequest(DmStreamRequestMessage {
            request_id: [5u8; 32],
            stream_name: "camera".to_string(),
        });
        let encoded = original.encode();
        assert_eq!(encoded[0], 0x44);
        let decoded = DmMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    // StreamSignalingMessage tests
    #[test]
    fn test_stream_accept_roundtrip() {
        let original = StreamSignalingMessage::Accept(StreamAcceptMessage {
            request_id: [1u8; 32],
        });
        let encoded = original.encode();
        assert_eq!(encoded[0], 0x50);
        let decoded = StreamSignalingMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_stream_reject_roundtrip() {
        let original = StreamSignalingMessage::Reject(StreamRejectMessage {
            request_id: [2u8; 32],
            reason: Some("busy".to_string()),
        });
        let encoded = original.encode();
        assert_eq!(encoded[0], 0x51);
        let decoded = StreamSignalingMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    // Helper function tests
    #[test]
    fn test_is_dm_message_type() {
        assert!(!is_dm_message_type(0x00)); // Topic Content
        assert!(!is_dm_message_type(0x3F)); // Last topic
        assert!(is_dm_message_type(0x40));  // DM Content
        assert!(is_dm_message_type(0x44));  // DM StreamRequest
        assert!(!is_dm_message_type(0x50)); // Stream signaling
    }

    #[test]
    fn test_is_stream_signaling_type() {
        assert!(!is_stream_signaling_type(0x4F)); // Last DM
        assert!(is_stream_signaling_type(0x50));  // StreamAccept
        assert!(is_stream_signaling_type(0x54));  // StreamEnded
        assert!(!is_stream_signaling_type(0x60)); // Reserved
    }

    // Decode error tests
    #[test]
    fn test_decode_empty() {
        assert!(matches!(TopicMessage::decode(&[]), Err(DecodeError::Empty)));
        assert!(matches!(DmMessage::decode(&[]), Err(DecodeError::Empty)));
    }

    #[test]
    fn test_decode_unknown_type() {
        assert!(matches!(TopicMessage::decode(&[0xFF]), Err(DecodeError::UnknownType(0xFF))));
        assert!(matches!(DmMessage::decode(&[0xFF]), Err(DecodeError::UnknownType(0xFF))));
    }

    #[test]
    fn test_decode_error_display() {
        assert_eq!(DecodeError::Empty.to_string(), "empty payload");
        assert_eq!(DecodeError::UnknownType(0xFF).to_string(), "unknown message type: 0xff");
        assert_eq!(
            DecodeError::InvalidPayload("test error".to_string()).to_string(),
            "invalid payload: test error"
        );
    }
}
