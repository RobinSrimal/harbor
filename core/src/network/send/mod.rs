//! Send Protocol - Core messaging for Harbor
//!
//! The Send protocol delivers encrypted packets to topic members and handles
//! read receipts. If not all recipients acknowledge, packets are replicated
//! to Harbor Nodes.
//!
//! # Protocol Flow
//!
//! 1. Create packet (encrypt → MAC → sign)
//! 2. Send to all topic members
//! 3. Track read receipts
//! 4. If receipts missing after timeout, replicate to Harbor Nodes
//!
//! # Wire Format
//!
//! ```text
//! Message Type (1 byte):
//!   0x01 = Packet
//!   0x02 = Receipt
//!
//! Packet:
//!   [type=0x01][length:4][SendPacket bytes]
//!
//! Receipt:
//!   [type=0x02][packet_id:16][sender:32]
//! ```
//!
//! # Module Structure
//!
//! - `service.rs` - Shared configuration (SendConfig)
//! - `outgoing.rs` - Sending packets (SendService, SendResult, SendError)
//! - `incoming.rs` - Receiving packets (process_incoming_packet, ProcessResult, ProcessError)
//! - `protocol.rs` - Wire protocol (SendMessage, Receipt, SEND_ALPN)
//! - `pool.rs` - Connection pooling
//! - `topic_messages.rs` - TopicMessage format

pub mod incoming;
pub mod outgoing;
pub mod pool;
pub mod protocol;
pub mod service;
pub mod topic_messages;

pub use pool::{SendPool, SendPoolConfig, SendPoolError, SendConnectionRef, SendPoolStats, SEND_ALPN as SEND_ALPN_FROM_POOL};
pub use protocol::{SEND_ALPN, SendMessage, Receipt};
pub use service::SendConfig;

// Outgoing (sending packets)
pub use outgoing::{SendService, SendResult, SendError};

// Incoming (receiving packets)
pub use incoming::{
    process_incoming_packet, ProcessResult, ProcessError,
    receive_packet, receive_packet_with_pow, process_receipt, ReceiveError,
};

// Topic message types (payload format for Send packets)
pub use topic_messages::{
    get_verification_mode_from_payload, DecodeError as TopicMessageDecodeError,
    JoinMessage, LeaveMessage, MessageType as TopicMessageType, TopicMessage,
    FileAnnouncementMessage, CanSeedMessage, SyncUpdateMessage,
};

