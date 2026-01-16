//! Harbor Core
//!
//! Core messaging protocol for Harbor - peer-to-peer encrypted messaging.
//!
//! This is the foundation crate that provides:
//! - Peer identity and connections (via Iroh)
//! - Topic-based messaging with encryption
//! - DHT for peer discovery
//! - Harbor Nodes for offline message delivery
//!
//! # Module Structure
//!
//! - `protocol/`: Public interface (Protocol, config, types, topic/stats methods)
//! - `handlers/`: Connection handlers (incoming/outgoing)
//! - `tasks/`: Background automation (replication, pull, sync, maintenance)
//! - `network/`: Wire formats and low-level primitives (DHT, Send, Harbor, Topic)
//! - `data/`: SQLite persistence
//! - `security/`: Cryptography (packets, identities, keys)
//! - `resilience/`: Anti-spam mechanisms (PoW, rate limits, storage)
//! - `testing/`: Test utilities
//!
//! # Quick Start
//!
//! ```ignore
//! use harbor_core::{Protocol, ProtocolConfig};
//!
//! // Start the protocol
//! let config = ProtocolConfig::for_testing();
//! let protocol = Protocol::start(config).await?;
//!
//! // Create a topic
//! let invite = protocol.create_topic().await?;
//!
//! // Send a message
//! protocol.send(&invite.topic_id, b"Hello!").await?;
//!
//! // Receive messages
//! let mut events = protocol.events().await.unwrap();
//! while let Some(msg) = events.recv().await {
//!     println!("Received: {:?}", msg.payload);
//! }
//! ```

// Public interface
pub mod protocol;

// Internal modules
pub(crate) mod handlers;
pub(crate) mod tasks;

// Infrastructure modules (pub for flexibility)
pub mod data;
pub mod network;
pub mod resilience;
pub mod security;
pub mod testing;

// Re-export main API types for convenience
pub use protocol::{
    Protocol, 
    ProtocolConfig, 
    ProtocolError, 
    IncomingMessage, 
    TopicInvite,
    MemberInfo,
    // Stats types
    ProtocolStats,
    IdentityStats,
    NetworkStats,
    DhtStats,
    TopicsStats,
    OutgoingStats,
    HarborStats,
    DhtBucketInfo,
    DhtNodeInfo,
    TopicDetails,
    TopicMemberInfo,
    TopicSummary,
};
