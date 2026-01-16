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

pub mod packet;

pub use packet::{
    // Core types
    SendPacket, PacketBuilder, PacketError, VerificationMode, EpochKeys,
    // Epoch-based functions (preferred for forward secrecy)
    create_packet_with_epoch, verify_and_decrypt_with_epoch,
    // Legacy functions (backward compatibility with epoch 0)
    create_packet, verify_and_decrypt_packet, verify_and_decrypt_packet_with_mode,
    // MAC verification
    verify_mac_only, verify_mac_only_raw,
};

