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

pub mod pool;
pub mod protocol;
pub mod service;

pub use pool::{SendPool, SendPoolConfig, SendPoolError, SendConnectionRef, SendPoolStats, SEND_ALPN as SEND_ALPN_FROM_POOL};
pub use protocol::{SEND_ALPN, SendMessage, Receipt};
pub use service::{SendService, SendConfig};

