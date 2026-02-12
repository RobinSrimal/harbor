//! Network layer for Harbor Protocol
//!
//! Contains:
//! - DHT: Kademlia-style distributed hash table for peer discovery
//! - Send: Core messaging protocol for packet delivery and receipts
//! - Harbor: Offline message storage and retrieval
//! - Share: P2P file sharing for files ≥512 KB
//! - Sync: CRDT synchronization for collaborative documents
//!
//! Each protocol has its own ALPN; only Send and DHT use connection pools.

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
pub mod wire;

// Re-export packet type system (application-level messages, may be Harbor-routed)
pub use packet::{
    CanSeedMessage,
    DecodeError,
    DmMessage,
    DmStreamRequestMessage,
    EncryptionKeyType,
    // Payload structs
    FileAnnouncementMessage,
    HarborIdSource,
    PacketType,
    Scope,
    StreamAcceptMessage,
    StreamActiveMessage,
    StreamEndedMessage,
    StreamQueryMessage,
    StreamRejectMessage,
    StreamRequestMessage,
    StreamSignalingMessage,
    SyncUpdateMessage,
    // Wrapper enums
    TopicMessage,
    VerificationMode,
    is_dm_message_type,
    is_stream_signaling_type,
};

// Re-export membership proof helpers
pub use membership::{
    MembershipProof, create_membership_binding, create_membership_proof, verify_membership_proof,
};

// Re-export commonly used items
pub use connect::Connector;
pub use control::{
    CONTROL_ALPN, ConnectAccept, ConnectDecline, ConnectInvite, ConnectRequest, ControlAck,
    ControlError, ControlPacketType, ControlResult, ControlRpcMessage, ControlRpcProtocol,
    ControlService, MemberInfo, RemoveMember, Suggest, TopicInvite, TopicJoin, TopicLeave,
};
pub use gate::ConnectionGate;
pub use harbor::{HARBOR_ALPN, HarborError, HarborMessage, HarborService};
pub use process::{ProcessContext, ProcessError, ProcessScope, process_packet};
pub use send::{Receipt, SEND_ALPN, SendConfig, SendService};
pub use share::{
    CanSeed, FileAnnouncement, SHARE_ALPN, ShareConfig, ShareError, ShareMessage, ShareService,
};
pub use stream::{STREAM_ALPN, STREAM_SIGNAL_TTL, StreamError, StreamService, StreamSession};
pub use sync::{
    DecodeError as SyncDecodeError, InitialSyncRequest, InitialSyncResponse, SYNC_ALPN,
    SyncMessage, SyncMessageType, SyncService, SyncUpdate,
};

// Sample files are kept for reference but not compiled
// See src/network/connection_pool_sample.rs.bak and src/network/dht/dht_sample.rs.bak
