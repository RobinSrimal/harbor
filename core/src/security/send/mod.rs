//! Send packet security
//!
//! Implements packet crypto for Harbor storage:
//! - Topic packets: AEAD encryption with AAD = HarborID || EndpointID || epoch,
//!   topic MAC, and sender signature
//! - DM packets: ECDH-derived encryption and sender signature
//!
//! # Topic and DM Modes
//!
//! Topic packets support per-epoch keys (epoch 0 for the default topic flow).
//! DM packets do not use topic MAC/epoch key derivation.
//!
//! # Direct vs Harbor Delivery
//!
//! - **Direct delivery**: Uses QUIC TLS only (no app-level crypto)
//! - **Harbor storage**: Uses `seal` to apply topic/DM packet crypto

pub mod packet;
pub mod seal;

pub use packet::{
    EpochKeys,
    // DM flag
    FLAG_DM,
    PACKET_ID_SIZE,
    PacketBuilder,
    PacketError,
    PacketId,
    // Core types
    SendPacket,
    VerificationMode,
    // DM functions
    create_dm_packet,
    // Topic functions
    create_packet,
    // Epoch-key topic functions
    create_packet_with_epoch,
    // Epoch key derivation
    derive_epoch_secret_from_topic,
    // Packet ID generation
    generate_packet_id,
    verify_and_decrypt_dm_packet,
    verify_and_decrypt_packet,
    verify_and_decrypt_packet_with_mode,
    verify_and_decrypt_with_epoch,
    // MAC verification
    verify_mac_only,
    verify_mac_only_raw,
};

pub use seal::{
    seal_dm_packet, seal_dm_packet_bytes, seal_topic_packet, seal_topic_packet_bytes,
    seal_topic_packet_with_epoch,
};
