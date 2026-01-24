//! Harbor Node Protocol
//!
//! Handles storing and retrieving packets for offline topic members.
//!
//! # Architecture
//!
//! - **Harbor Node**: Any node can act as a Harbor Node for HarborIDs close to it
//! - **Finding nodes**: Use DHT to find 30 closest nodes to HarborID
//! - **Replication**: Store packets on 10 Harbor Nodes when direct delivery fails
//! - **Sync**: Harbor Nodes sync among themselves every 5 minutes
//!
//! # Protocol Flow
//!
//! ## Storing a packet:
//! 1. Send packet fails to reach all members
//! 2. Find Harbor Nodes via DHT lookup on HarborID
//! 3. Store packet on 10 Harbor Nodes with recipient list
//!
//! ## Pulling packets:
//! 1. Client comes online
//! 2. Find Harbor Nodes via DHT lookup on HarborID
//! 3. Request missed packets since last sync
//! 4. Acknowledge receipt to Harbor Nodes
//!
//! ## Syncing (Harbor Nodes):
//! 1. Every 5 minutes (configurable), find closest 5 Harbor Nodes
//! 2. Pick 1 randomly to sync with (reduces traffic, achieves coverage over time)
//! 3. Exchange packet metadata and delivery status
//! 4. Apply missing packets and delivery updates
//!
//! # Logging
//!
//! See root README for `RUST_LOG` configuration. Log prefixes in this module:
//!
//! - `Harbor STORE:` - incoming store requests
//! - `Harbor PULL:` - incoming pull requests  
//! - `Harbor ACK:` - delivery acknowledgments
//! - `Harbor SYNC:` - Harbor-to-Harbor sync
//! - `Harbor CLIENT:` - outgoing operations

pub mod incoming;
pub mod outgoing;
pub mod protocol;
pub mod service;

pub use protocol::{HarborMessage, StoreRequest, PullRequest, PullResponse, DeliveryAck, HARBOR_ALPN};
pub use service::{HarborService, HarborError};

