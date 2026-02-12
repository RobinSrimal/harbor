//! BLAKE3-based Message Authentication Code (MAC)
//!
//! Provides keyed-hash MAC creation using BLAKE3.
//! BLAKE3 keyed hashing is fast, secure, and produces 32-byte MACs.
//!
//! # Security Properties
//!
//! - **Unforgeability**: Without the key, an attacker cannot create a valid MAC.
//! - **No length extension**: BLAKE3 is resistant to length extension attacks.

use blake3::Hash;

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
    fn test_empty_message() {
        let key: [u8; 32] = [0x42; 32];
        let empty: &[u8] = b"";

        // Empty message should produce valid MAC
        let mac = create_mac(&key, empty);
        assert_eq!(mac.as_bytes().len(), 32);

        // MAC should be non-zero (it's a keyed hash, not just hash of empty)
        assert_ne!(mac.as_bytes(), &[0u8; 32]);

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
        assert!(
            diff_count >= 16,
            "Poor avalanche effect: only {} bytes differ",
            diff_count
        );
    }

    #[test]
    fn test_create_mac_known_vector() {
        let key: [u8; 32] = [0x42; 32];
        let message = b"Hello, Harbor!";
        let mac = create_mac(&key, message);

        let expected: [u8; 32] = [
            215, 185, 236, 58, 34, 72, 111, 183, 18, 89, 179, 10, 192, 248, 26, 19, 89, 95, 235,
            68, 127, 212, 118, 253, 184, 5, 114, 206, 61, 177, 207, 125,
        ];
        assert_eq!(mac.as_bytes(), &expected);
    }
}
