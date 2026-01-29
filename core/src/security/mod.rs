//! Security module for Harbor Protocol
//!
//! Provides cryptographic operations:
//! - Key pair generation (Ed25519)
//! - Key derivation (BLAKE3)
//! - Topic key derivation (keys derived from topic_id - Tier 1)
//! - Harbor packet security (AEAD encryption, MAC, signing)
//! - Send packet security (full pipeline)
//!
//! # Tier 1 Security Model
//!
//! Key derivation: `key = KDF(topic_id)` - same for all members
//! - Anyone with `topic_id` can derive encryption keys
//! - No eviction capability (no key rotation)
//! - MAC + Signature verification proves topic membership and sender identity

pub mod create_key_pair;
pub mod derive_key;
pub mod dm_keys;
pub mod harbor;
pub mod send;
pub mod topic_keys;

// Re-export commonly used items
pub use create_key_pair::{generate_key_pair, key_pair_from_bytes, KeyPair};
pub use send::{
    // Tier 1 functions (topic_id-derived keys, epoch 0)
    create_packet, verify_and_decrypt_packet, verify_and_decrypt_packet_with_mode,
    // Types
    EpochKeys, PacketError, SendPacket, VerificationMode,
    // DM
    create_dm_packet, verify_and_decrypt_dm_packet, FLAG_DM,
};
pub use topic_keys::{harbor_id_from_topic, send_target_id, TopicKeys};
pub use dm_keys::{dm_seal, dm_open, DmCryptoError};
