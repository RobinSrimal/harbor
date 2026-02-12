//! Security module for Harbor Protocol
//!
//! Provides cryptographic operations:
//! - Key pair generation (Ed25519)
//! - Key derivation (BLAKE3)
//! - Topic key derivation (keys derived from topic_id)
//! - Harbor packet security (AEAD encryption, MAC, signing)
//! - Send packet security for topic and DM packets
//!
//! # Current Crypto Model
//!
//! Key derivation: `key = KDF(topic_id)` - same for all members
//! - Anyone with `topic_id` can derive encryption keys
//! - No eviction capability (no key rotation)
//! - MAC + Signature verification proves topic membership and sender identity
//! - DM packets use sender/recipient keys via ECDH-derived encryption + signature

pub mod create_key_pair;
pub mod dm_keys;
pub mod harbor;
pub mod send;
pub mod topic_keys;

// Re-export commonly used items
pub use create_key_pair::{KeyPair, generate_key_pair, key_pair_from_bytes};
pub use dm_keys::{DmCryptoError, dm_open, dm_seal};
pub use send::{
    // Types
    EpochKeys,
    FLAG_DM,
    PacketError,
    PacketId,
    SendPacket,
    VerificationMode,
    // DM
    create_dm_packet,
    // Topic functions
    create_packet,
    derive_epoch_secret_from_topic,
    verify_and_decrypt_dm_packet,
    verify_and_decrypt_packet,
    verify_and_decrypt_packet_with_mode,
    // Epoch-key topic functions
    verify_and_decrypt_with_epoch,
};
pub use topic_keys::{TopicKeys, harbor_id_from_topic};
