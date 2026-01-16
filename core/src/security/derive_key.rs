//! Key derivation using BLAKE3 KDF
//!
//! Provides domain-separated key derivation from arbitrary key material.
//! Uses BLAKE3's built-in KDF mode which is designed for deriving multiple
//! keys from a single source of entropy.
//!
//! # Context Strings
//!
//! The context string provides domain separation - ensuring that keys derived
//! for different purposes are cryptographically independent even if the key
//! material is the same.
//!
//! Best practices for context strings:
//! - Use a unique, descriptive string for each key purpose
//! - Include application name to avoid collisions: `"harbor encryption key"`
//! - Keep it human-readable for auditability
//! - Context is NOT secret (it's part of the algorithm, not the key)
//!
//! # Example
//!
//! ```
//! use harbor_core::security::derive_key::key_from_context;
//!
//! let topic_id = [0u8; 32];
//! let enc_key = key_from_context("harbor topic encryption", &topic_id);
//! let mac_key = key_from_context("harbor topic mac", &topic_id);
//!
//! // Same material, different contexts = different keys
//! assert_ne!(enc_key, mac_key);
//! ```

use blake3::derive_key;

/// Derive a 32-byte key from context and key material using BLAKE3 KDF
///
/// # Arguments
/// * `context` - Domain separation string (see module docs for best practices)
/// * `key_material` - Source entropy (e.g., topic ID, master key, password hash)
///
/// # Returns
/// A 32-byte derived key, cryptographically bound to both inputs
///
/// # Security Properties
/// - Deterministic: same inputs always produce same output
/// - Domain separated: different contexts produce independent keys
/// - One-way: cannot recover key_material from output
/// - Collision resistant: different inputs produce different outputs
pub fn key_from_context(context: &str, key_material: &[u8]) -> [u8; 32] {
    derive_key(context, key_material)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_key_deterministic() {
        let uuid = b"550e8400-e29b-41d4-a716-446655440000";
        let key = key_from_context("my-app encryption", uuid);
        
        // Use --nocapture to see: cargo test derive_key -- --nocapture
        println!("Derived key: {}", hex::encode(key));
        
        // Same input always produces same output
        let key2 = key_from_context("my-app encryption", uuid);
        assert_eq!(key, key2, "KDF should be deterministic");
    }

    #[test]
    fn test_different_context_different_key() {
        let material = b"same key material";
        
        let key1 = key_from_context("context A", material);
        let key2 = key_from_context("context B", material);
        
        assert_ne!(key1, key2, "different contexts must produce different keys");
    }

    #[test]
    fn test_different_material_different_key() {
        let context = "same context";
        
        let key1 = key_from_context(context, b"material A");
        let key2 = key_from_context(context, b"material B");
        
        assert_ne!(key1, key2, "different material must produce different keys");
    }

    #[test]
    fn test_empty_context() {
        // Empty context is valid (though not recommended)
        let key = key_from_context("", b"some material");
        assert_eq!(key.len(), 32);
        
        // Empty context is different from non-empty
        let key2 = key_from_context("non-empty", b"some material");
        assert_ne!(key, key2);
    }

    #[test]
    fn test_empty_key_material() {
        // Empty key material is valid (though not recommended for security)
        let key = key_from_context("some context", b"");
        assert_eq!(key.len(), 32);
        
        // Empty material is different from non-empty
        let key2 = key_from_context("some context", b"non-empty");
        assert_ne!(key, key2);
    }

    #[test]
    fn test_large_key_material() {
        // Should handle large inputs efficiently
        let large_material = vec![0xABu8; 1024 * 1024]; // 1MB
        let key = key_from_context("large input test", &large_material);
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn test_single_bit_avalanche() {
        // Changing a single bit should completely change the output
        let material1 = [0u8; 32];
        let mut material2 = [0u8; 32];
        material2[0] = 1; // Single bit difference
        
        let key1 = key_from_context("avalanche test", &material1);
        let key2 = key_from_context("avalanche test", &material2);
        
        // Count differing bits (should be ~128 on average for good hash)
        let differing_bits: u32 = key1.iter()
            .zip(key2.iter())
            .map(|(a, b)| (a ^ b).count_ones())
            .sum();
        
        // Should have significant difference (not just 1 bit)
        assert!(differing_bits > 64, "avalanche effect: {} bits differ", differing_bits);
    }
}