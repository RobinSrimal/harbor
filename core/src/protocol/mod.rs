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

pub(crate) mod core;
mod config;
mod error;
mod events;
mod types;
mod topics;
mod send;
mod stats;
mod share;

// Core protocol
pub use core::Protocol;
pub use config::ProtocolConfig;

// Error type
pub use error::ProtocolError;

// Events (for app layer)
pub use events::{
    FileAnnouncedEvent, FileCompleteEvent, FileProgressEvent, IncomingMessage, ProtocolEvent,
    SyncUpdateEvent, SyncRequestEvent, SyncResponseEvent,
};

// Domain types
pub use types::{MemberInfo, TopicInvite};

// Stats types
pub use stats::{
    DhtBucketInfo, DhtNodeInfo, DhtStats, HarborStats, IdentityStats, NetworkStats, OutgoingStats,
    ProtocolStats, TopicDetails, TopicMemberInfo, TopicSummary, TopicsStats,
};

// Share types
pub use share::ShareStatus;
