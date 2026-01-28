//! Send Protocol - Core messaging for Harbor
//!
//! The Send protocol delivers encrypted packets to topic members and handles
//! read receipts via irpc RPC (bi-stream request/response).
//!
//! # Protocol Flow
//!
//! 1. Create packet (encrypt → MAC → sign)
//! 2. Send to all topic members via irpc DeliverPacket RPC
//! 3. Receive Receipt inline as RPC response
//! 4. If receipts missing after timeout, replicate to Harbor Nodes
//!
//! # Module Structure
//!
//! - `outgoing.rs` - SendService (single entry point for all send operations)
//! - `incoming.rs` - Receiving packets (process_incoming_packet, ProcessResult, ProcessError)
//! - `protocol.rs` - irpc wire protocol (SendRpcProtocol, DeliverPacket, Receipt)
//! - `pool.rs` - Connection pooling
//! - `topic_messages.rs` - TopicMessage format

pub mod incoming;
pub mod outgoing;
pub mod pool;
pub mod protocol;
pub mod service;
pub mod topic_messages;

pub use pool::{SendPool, SendPoolConfig, SendPoolError, SendConnectionRef, SendPoolStats, SEND_ALPN as SEND_ALPN_FROM_POOL};
pub use protocol::{SEND_ALPN, Receipt};
pub use service::SendConfig;

// Outgoing (sending packets)
pub use outgoing::{SendService, SendResult, SendError, SendOptions};

// Incoming (types for receiving packets — logic is on SendService)
pub use incoming::{
    ProcessResult, ProcessError, PacketSource, ReceiveError,
};

// Topic message types (payload format for Send packets)
pub use topic_messages::{
    get_verification_mode_from_payload, DecodeError as TopicMessageDecodeError,
    JoinMessage, LeaveMessage, MessageType as TopicMessageType, TopicMessage,
    FileAnnouncementMessage, CanSeedMessage, SyncUpdateMessage,
    StreamRequestMessage, StreamAcceptMessage, StreamRejectMessage,
    StreamQueryMessage, StreamActiveMessage, StreamEndedMessage,
};
