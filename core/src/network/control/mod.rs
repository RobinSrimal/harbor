//! Control Protocol - Lifecycle and relationship management for Harbor
//!
//! The Control protocol handles peer connections, topic invitations,
//! and membership management via one-off RPC exchanges.
//!
//! # Protocol Flow
//!
//! 1. Open connection on CONTROL_ALPN
//! 2. Send control message (ConnectRequest, TopicInvite, etc.)
//! 3. Receive ControlAck response
//! 4. Close connection
//!
//! # Message Types
//!
//! - **Connection**: ConnectRequest, ConnectAccept, ConnectDecline
//! - **Topic**: TopicInvite, TopicJoin, TopicLeave
//! - **Admin**: RemoveMember (key rotation)
//! - **Introduction**: Suggest (peer introduction)
//!
//! # Harbor Replication
//!
//! Control messages are stored in `outgoing_packets` table for Harbor replication.
//! The existing replication task handles delivery to Harbor nodes.
//!
//! # Module Structure
//!
//! - `protocol.rs` - ALPN constant, message types, RPC definition
//! - `service.rs` - ControlService struct, shared utilities
//! - `peers.rs` - Peer connection lifecycle (request/accept/decline/block)
//! - `membership.rs` - Topic invite/join/leave/remove
//! - `lifecycle.rs` - Topic create/join/list/get_invite
//! - `introductions.rs` - Peer suggestions

pub mod introductions;
pub mod lifecycle;
pub mod membership;
pub mod peers;
pub mod protocol;
pub mod service;
pub mod types;

// Re-export protocol types
pub use protocol::{
    ControlAck, ControlPacketType, ControlRpcMessage, ControlRpcProtocol, CONTROL_ALPN,
};

// Re-export wire message types (TopicInvite here is the wire format, not the shareable one)
pub use protocol::{
    ConnectAccept, ConnectDecline, ConnectRequest, RemoveMember, Suggest, TopicJoin, TopicLeave,
};

// Re-export service types
pub use service::{ConnectInvite, ControlError, ControlResult, ControlService};

// Re-export public topic types (shareable invite format)
pub use types::{MemberInfo, TopicInvite};
