//! BLAKE3-based Message Authentication Code (MAC)
//!
//! Provides keyed-hash MAC creation and verification using BLAKE3.
//! BLAKE3 keyed hashing is fast, secure, and produces 32-byte MACs.
//!
//! # Security Properties
//!
//! - **Unforgeability**: Without the key, an attacker cannot create a valid MAC.
//! - **Constant-time verification**: `verify_mac` uses constant-time comparison
//!   to prevent timing attacks.
//! - **No length extension**: BLAKE3 is resistant to length extension attacks.

use blake3::{Hash, Hasher};

/// Create a MAC (message authentication code) using BLAKE3 keyed hashing.
///
/// # Arguments
/// * `key` - 32-byte secret key
/// * `message` - Data to authenticate (can be empty)
///
/// # Returns
/// 32-byte MAC as a BLAKE3 `Hash`
pub fn create_mac(key: &[u8; 32], message: &[u8]) -> Hash {
    blake3::keyed_hash(key, message)
}

/// Verify a MAC against an expected value.
///
/// Uses constant-time comparison to prevent timing attacks.
///
/// # Arguments
/// * `key` - 32-byte secret key
/// * `message` - Data that was authenticated
/// * `expected` - Expected MAC value
///
/// # Returns
/// `true` if the MAC is valid, `false` otherwise
pub fn verify_mac(key: &[u8; 32], message: &[u8], expected: &Hash) -> bool {
    let computed = create_mac(key, message);
    // BLAKE3's Hash implements PartialEq with constant_time_eq
    computed == *expected
}

/// Create a MAC for multiple chunks of data (streaming).
///
/// Useful when data is received in pieces or too large to hold in memory.
/// Produces the same result as `create_mac` with concatenated chunks.
///
/// # Example
/// ```ignore
/// let chunks: &[&[u8]] = &[b"Hello, ", b"Harbor!"];
/// let mac = create_mac_streaming(&key, chunks);
/// // Equivalent to: create_mac(&key, b"Hello, Harbor!")
/// ```
pub fn create_mac_streaming(key: &[u8; 32], chunks: &[&[u8]]) -> Hash {
    let mut hasher = Hasher::new_keyed(key);
    for chunk in chunks {
        hasher.update(chunk);
    }
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_mac() {
        let key: [u8; 32] = [0x42; 32];
        let message = b"Hello, Harbor!";

        let mac = create_mac(&key, message);

        // MAC should be 32 bytes
        assert_eq!(mac.as_bytes().len(), 32);

        // Same input produces same MAC
        let mac2 = create_mac(&key, message);
        assert_eq!(mac, mac2);

        // Different message produces different MAC
        let mac3 = create_mac(&key, b"Different message");
        assert_ne!(mac, mac3);

        // Different key produces different MAC
        let other_key: [u8; 32] = [0x43; 32];
        let mac4 = create_mac(&other_key, message);
        assert_ne!(mac, mac4);
    }

    #[test]
    fn test_verify_mac() {
        let key: [u8; 32] = [0x42; 32];
        let message = b"Secret message";

        let mac = create_mac(&key, message);

        // Valid MAC should verify
        assert!(verify_mac(&key, message, &mac));

        // Tampered message should fail
        assert!(!verify_mac(&key, b"Tampered message", &mac));

        // Wrong key should fail
        let wrong_key: [u8; 32] = [0x00; 32];
        assert!(!verify_mac(&wrong_key, message, &mac));
    }

    #[test]
    fn test_streaming_mac() {
        let key: [u8; 32] = [0x42; 32];

        // Streaming MAC of chunks should equal single MAC of combined data
        let chunks: &[&[u8]] = &[b"Hello, ", b"Harbor!"];
        let streaming_mac = create_mac_streaming(&key, chunks);

        let combined = b"Hello, Harbor!";
        let single_mac = create_mac(&key, combined);

        assert_eq!(streaming_mac, single_mac);
    }

    #[test]
    fn test_empty_message() {
        let key: [u8; 32] = [0x42; 32];
        let empty: &[u8] = b"";

        // Empty message should produce valid MAC
        let mac = create_mac(&key, empty);
        assert_eq!(mac.as_bytes().len(), 32);

        // MAC should be non-zero (it's a keyed hash, not just hash of empty)
        assert_ne!(mac.as_bytes(), &[0u8; 32]);

        // Should verify correctly
        assert!(verify_mac(&key, empty, &mac));

        // Different key should produce different MAC even for empty message
        let other_key: [u8; 32] = [0x43; 32];
        let other_mac = create_mac(&other_key, empty);
        assert_ne!(mac, other_mac);
    }

    #[test]
    fn test_all_zero_key() {
        let zero_key: [u8; 32] = [0u8; 32];
        let message = b"Test message";

        // All-zero key should still work
        let mac = create_mac(&zero_key, message);
        assert_eq!(mac.as_bytes().len(), 32);
        assert!(verify_mac(&zero_key, message, &mac));

        // Should be different from non-zero key
        let nonzero_key: [u8; 32] = [0x01; 32];
        let other_mac = create_mac(&nonzero_key, message);
        assert_ne!(mac, other_mac);
    }

    #[test]
    fn test_large_message() {
        let key: [u8; 32] = [0x42; 32];

        // 1 MB message
        let large_message = vec![0xAB_u8; 1024 * 1024];

        let mac = create_mac(&key, &large_message);
        assert_eq!(mac.as_bytes().len(), 32);
        assert!(verify_mac(&key, &large_message, &mac));
    }

    #[test]
    fn test_streaming_single_chunk() {
        let key: [u8; 32] = [0x42; 32];
        let message = b"Single chunk";

        // Single chunk streaming should equal direct MAC
        let chunks: &[&[u8]] = &[message];
        let streaming_mac = create_mac_streaming(&key, chunks);
        let direct_mac = create_mac(&key, message);

        assert_eq!(streaming_mac, direct_mac);
    }

    #[test]
    fn test_streaming_empty_chunks() {
        let key: [u8; 32] = [0x42; 32];

        // Empty chunks array should equal MAC of empty message
        let empty_chunks: &[&[u8]] = &[];
        let streaming_mac = create_mac_streaming(&key, empty_chunks);
        let empty_mac = create_mac(&key, b"");

        assert_eq!(streaming_mac, empty_mac);
    }

    #[test]
    fn test_streaming_with_empty_chunks_mixed() {
        let key: [u8; 32] = [0x42; 32];

        // Chunks with empty slices interspersed
        let chunks: &[&[u8]] = &[b"Hello", b"", b", ", b"", b"World"];
        let streaming_mac = create_mac_streaming(&key, chunks);

        let combined_mac = create_mac(&key, b"Hello, World");
        assert_eq!(streaming_mac, combined_mac);
    }

    #[test]
    fn test_single_bit_difference() {
        let key: [u8; 32] = [0x42; 32];

        let msg1 = [0u8; 32];
        let mut msg2 = [0u8; 32];
        msg2[0] = 1; // Single bit difference

        let mac1 = create_mac(&key, &msg1);
        let mac2 = create_mac(&key, &msg2);

        // Should produce completely different MACs (avalanche effect)
        assert_ne!(mac1, mac2);

        // Count differing bytes
        let diff_count: usize = mac1
            .as_bytes()
            .iter()
            .zip(mac2.as_bytes().iter())
            .filter(|(a, b)| a != b)
            .count();

        // Most bytes should differ (good avalanche property)
        assert!(diff_count >= 16, "Poor avalanche effect: only {} bytes differ", diff_count);
    }
}
