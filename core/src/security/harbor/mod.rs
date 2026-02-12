//! Harbor packet security
//!
//! Implements the cryptographic pipeline for Harbor packets:
//! 1. AEAD encryption (ChaCha20-Poly1305)
//! 2. MAC authentication (BLAKE3 keyed hash)
//! 3. Signature (Ed25519)

pub mod authenticate;
pub mod encrypt;
pub mod sign;

// Re-export for convenience
pub use authenticate::create_mac;
pub use encrypt::{decrypt, encrypt, generate_nonce};
pub use sign::{sign_packet, verify_packet};
