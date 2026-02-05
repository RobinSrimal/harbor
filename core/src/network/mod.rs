//! Network layer for Harbor Protocol
//!
//! Contains:
//! - DHT: Kademlia-style distributed hash table for peer discovery
//! - Send: Core messaging protocol for packet delivery and receipts
//! - Harbor: Offline message storage and retrieval
//! - Share: P2P file sharing for files ≥512 KB
//! - Sync: CRDT synchronization for collaborative documents
//!
//! Each protocol has its own ALPN and connection pool.

pub mod connect;
pub mod control;
pub mod dht;
pub mod gate;
pub mod harbor;
pub mod membership;
pub mod packet;
pub mod pool;
pub mod process;
pub mod rpc;
pub mod send;
pub mod share;
pub mod stream;
pub mod sync;
pub mod topic;
pub mod wire;

// Re-export packet type system (application-level messages, may be Harbor-routed)
pub use packet::{
    PacketType, Scope, VerificationMode, HarborIdSource, EncryptionKeyType,
    DecodeError, is_dm_message_type, is_stream_signaling_type,
    // Payload structs
    FileAnnouncementMessage, CanSeedMessage, SyncUpdateMessage, StreamRequestMessage,
    DmStreamRequestMessage, StreamAcceptMessage, StreamRejectMessage,
    StreamQueryMessage, StreamActiveMessage, StreamEndedMessage,
    // Wrapper enums
    TopicMessage, DmMessage, StreamSignalingMessage,
};

// Re-export membership proof helpers
pub use membership::{create_membership_binding, create_membership_proof, verify_membership_proof, MembershipProof};

// Re-export commonly used items
pub use harbor::{HarborError, HarborMessage, HarborService, HARBOR_ALPN};
pub use send::{Receipt, SendConfig, SendService, SEND_ALPN};
pub use share::{
    FileAnnouncement, CanSeed, ShareConfig, ShareError, ShareMessage, ShareService, SHARE_ALPN,
};
pub use topic::{TopicError, TopicService};
pub use sync::{
    SyncMessage, SyncMessageType, SyncUpdate, InitialSyncRequest, InitialSyncResponse,
    DecodeError as SyncDecodeError, SYNC_ALPN, SyncService,
};
pub use stream::{StreamService, StreamError, StreamSession, STREAM_ALPN, STREAM_SIGNAL_TTL};
pub use process::{ProcessContext, ProcessScope, ProcessError, process_packet};
pub use gate::ConnectionGate;
pub use control::{
    ControlAck, ControlPacketType, ControlRpcMessage, ControlRpcProtocol, CONTROL_ALPN,
    ConnectAccept, ConnectDecline, ConnectRequest, RemoveMember, Suggest, TopicInvite,
    TopicJoin, TopicLeave, ControlService, ControlError, ControlResult, ConnectInvite,
};

// Sample files are kept for reference but not compiled
// See src/network/connection_pool_sample.rs.bak and src/network/dht/dht_sample.rs.bak
