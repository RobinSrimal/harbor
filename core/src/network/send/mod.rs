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
//! - **Harbor storage**: Uses seal() to apply full crypto stack (encrypt + MAC + sign)
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

pub use protocol::{SEND_ALPN, Receipt};
pub use service::{
    SendConfig, SendService, SendResult, SendError, SendOptions,
    ProcessResult, ProcessError, PacketSource, ReceiveError,
};

// Re-export message types from network/packet for backwards compatibility
pub use crate::network::packet::{
    // Payload structs
    FileAnnouncementMessage, CanSeedMessage, SyncUpdateMessage, StreamRequestMessage,
    DmStreamRequestMessage, StreamAcceptMessage, StreamRejectMessage,
    StreamQueryMessage, StreamActiveMessage, StreamEndedMessage,
    // Wrapper enums
    TopicMessage, DmMessage, StreamSignalingMessage,
    // Decode error
    DecodeError as TopicMessageDecodeError,
    // Helper functions
    is_dm_message_type, is_stream_signaling_type,
};
