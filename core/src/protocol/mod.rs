//! Harbor Protocol - Public Interface
//!
//! This module provides the public API for the Harbor protocol.
//! External code imports types and methods from here.
//!
//! # Module Structure
//!
//! - `core.rs`: Protocol struct, start/stop, lifecycle
//! - `config.rs`: ProtocolConfig builder
//! - `error.rs`: ProtocolError
//! - `events.rs`: Protocol events (messages, file events)
//! - `types.rs`: Public types (TopicInvite, MemberInfo)
//! - `topics.rs`: Topic operations (create, join, leave)
//! - `send.rs`: Message sending (send)
//! - `stats/`: Stats and monitoring
//! - `share.rs`: File sharing operations
//!
//! # Example
//!
//! ```ignore
//! use harbor_core::{Protocol, ProtocolConfig};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = ProtocolConfig::default();
//!     let protocol = Protocol::start(config).await?;
//!
//!     let invite = protocol.create_topic().await?;
//!     protocol.send(&invite.topic_id, b"Hello!").await?;
//!
//!     protocol.stop().await;
//!     Ok(())
//! }
//! ```

mod api;
mod config;
pub(crate) mod core;
mod error;
mod events;
mod stats;

// Core protocol
pub use config::ProtocolConfig;
pub use core::Protocol;

// Error type
pub use error::ProtocolError;

// Events (for app layer)
pub use events::{
    ConnectionAcceptedEvent,
    ConnectionDeclinedEvent,
    // Control events
    ConnectionRequestEvent,
    DmFileAnnouncedEvent,
    DmReceivedEvent,
    DmSyncRequestEvent,
    DmSyncResponseEvent,
    DmSyncUpdateEvent,
    FileAnnouncedEvent,
    FileCompleteEvent,
    FileProgressEvent,
    IncomingMessage,
    PeerSuggestedEvent,
    ProtocolEvent,
    StreamAcceptedEvent,
    StreamConnectedEvent,
    StreamEndedEvent,
    StreamRejectedEvent,
    StreamRequestEvent,
    SyncRequestEvent,
    SyncResponseEvent,
    SyncUpdateEvent,
    TopicEpochRotatedEvent,
    TopicInviteReceivedEvent,
    TopicMemberJoinedEvent,
    TopicMemberLeftEvent,
};

// Domain types (from ControlService)
pub use crate::network::control::{MemberInfo, TopicInvite};

// Stats types
pub use stats::{
    DhtBucketInfo, DhtNodeInfo, DhtStats, HarborStats, IdentityStats, NetworkStats, OutgoingStats,
    ProtocolStats, TopicDetails, TopicMemberInfo, TopicSummary, TopicsStats,
};

// Share types
pub use api::ShareStatus;

// Control types
pub use crate::data::{ConnectionInfo, ConnectionState, PendingTopicInvite};
pub use crate::network::control::ConnectInvite;

// Target types (Topic vs DM)
mod target;
pub use target::Target;
