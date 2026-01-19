//! Harbor Protocol - Public Interface
//!
//! This module provides the public API for the Harbor protocol.
//! External code imports types and methods from here.
//!
//! # Module Structure
//!
//! - `core.rs`: Protocol struct, start/stop, lifecycle
//! - `config.rs`: ProtocolConfig builder
//! - `types.rs`: Public types (errors, stats, TopicInvite)
//! - `topics.rs`: Topic operations (create, join, leave, send)
//! - `stats.rs`: Stats and monitoring
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
mod types;
mod topics;
mod stats;
mod share;

pub use core::Protocol;
pub use config::ProtocolConfig;
pub use types::{
    IncomingMessage, TopicInvite, MemberInfo, ProtocolError,
    // Protocol events (for app event bus)
    ProtocolEvent, FileAnnouncedEvent, FileProgressEvent, FileCompleteEvent,
    // Stats types
    ProtocolStats, IdentityStats, NetworkStats, DhtStats, TopicsStats,
    OutgoingStats, HarborStats, DhtBucketInfo, DhtNodeInfo, TopicDetails,
    TopicMemberInfo, TopicSummary,
};
pub use share::ShareStatus;

