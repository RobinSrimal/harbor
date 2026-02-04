//! Send packet security
//!
//! Implements the full cryptographic pipeline for Send packets:
//! 1. AEAD encryption with AAD = HarborID || EndpointID || epoch
//! 2. MAC for topic authentication (epoch-bound)
//! 3. Signature for sender verification
//!
//! # Epoch-Based Encryption (Forward Secrecy)
//!
//! Messages are encrypted with epoch-specific keys derived from MLS.
//! This provides forward secrecy: late joiners cannot decrypt old messages.
//!
//! # Direct vs Harbor Delivery
//!
//! - **Direct delivery**: Uses QUIC TLS only (no app-level crypto)
//! - **Harbor storage**: Uses `seal` module to apply full crypto stack

pub mod packet;
pub mod seal;

pub use packet::{
    // Core types
    SendPacket, PacketBuilder, PacketError, VerificationMode, EpochKeys,
    // Epoch key derivation
    derive_epoch_secret_from_topic,
    // Epoch-based functions (preferred for forward secrecy)
    create_packet_with_epoch, verify_and_decrypt_with_epoch,
    // Legacy functions (backward compatibility with epoch 0)
    create_packet, verify_and_decrypt_packet, verify_and_decrypt_packet_with_mode,
    // MAC verification
    verify_mac_only, verify_mac_only_raw,
    // DM functions
    create_dm_packet, verify_and_decrypt_dm_packet,
    // DM flag
    FLAG_DM,
};

pub use seal::{
    seal_topic_packet, seal_topic_packet_bytes, seal_topic_packet_with_epoch,
    seal_dm_packet, seal_dm_packet_bytes,
};

