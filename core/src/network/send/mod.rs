//! Send Protocol - Core messaging for Harbor
//!
//! The Send protocol delivers messages to topic members and DM recipients
//! via irpc RPC (bi-stream request/response).
//!
//! # Protocol Flow
//!
//! 1. Store raw payload in outgoing_packets table
//! 2. Deliver raw payload to recipients via DeliverTopic/DeliverDm (QUIC TLS provides encryption)
//! 3. Receive Receipt inline as RPC response
//! 4. If receipts missing after timeout, seal() and replicate to Harbor Nodes
//!
//! # Direct vs Harbor Delivery
//!
//! - **Direct delivery**: Uses QUIC TLS only (no app-level crypto) - fast path
//! - **Harbor storage**: Uses seal() crypto (topic: encrypt + MAC + sign, DM: encrypt + sign)
//!
//! # Module Structure
//!
//! - `service.rs` - SendService struct, shared infrastructure, types
//! - `topic.rs` - Topic send + process + handler dispatch
//! - `dm.rs` - DM send + process + handler dispatch
//! - `protocol.rs` - irpc wire protocol (SendRpcProtocol, DeliverTopic, DeliverDm, Receipt)

pub mod dm;
pub mod protocol;
pub mod service;
pub mod topic;

pub use protocol::{Receipt, SEND_ALPN};
pub use service::{
    PacketSource, ProcessError, ProcessResult, ReceiveError, SendConfig, SendError, SendOptions,
    SendResult, SendService,
};

// Re-export message types from network/packet for backwards compatibility
pub use crate::network::packet::{
    CanSeedMessage,
    // Decode error
    DecodeError as TopicMessageDecodeError,
    DmMessage,
    DmStreamRequestMessage,
    // Payload structs
    FileAnnouncementMessage,
    StreamAcceptMessage,
    StreamActiveMessage,
    StreamEndedMessage,
    StreamQueryMessage,
    StreamRejectMessage,
    StreamRequestMessage,
    StreamSignalingMessage,
    SyncUpdateMessage,
    // Wrapper enums
    TopicMessage,
    // Helper functions
    is_dm_message_type,
    is_stream_signaling_type,
};
