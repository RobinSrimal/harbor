//! Topic key derivation
//!
//! Derives encryption and MAC keys from a TopicID using BLAKE3 KDF.
//! Each topic has its own keys for encrypting packets.
//!
//! Keys are derived on demand (not stored) since BLAKE3 derivation is
//! extremely fast (<1μs) and reduces secret material in storage.

use blake3::derive_key;
use std::fmt;

/// Keys derived from a TopicID for packet encryption.
///
/// These keys are derived deterministically from the TopicID using BLAKE3 KDF.
/// The same TopicID will always produce the same keys.
pub struct TopicKeys {
    /// 32-byte encryption key for AEAD (ChaCha20-Poly1305)
    pub k_enc: [u8; 32],
    /// 32-byte MAC key for BLAKE3 keyed MAC authentication
    pub k_mac: [u8; 32],
}

// Manual Clone implementation (can't derive due to custom Debug)
impl Clone for TopicKeys {
    fn clone(&self) -> Self {
        Self {
            k_enc: self.k_enc,
            k_mac: self.k_mac,
        }
    }
}

// Custom Debug implementation to prevent accidental key exposure in logs
impl fmt::Debug for TopicKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TopicKeys")
            .field("k_enc", &"[REDACTED]")
            .field("k_mac", &"[REDACTED]")
            .finish()
    }
}

/// Context strings for key derivation (as specified in Proposal)
const ENC_CONTEXT: &str = "harbor topic encryption key";
const MAC_CONTEXT: &str = "harbor topic mac key";

impl TopicKeys {
    /// Derive keys from a TopicID using BLAKE3 KDF.
    ///
    /// ```text
    /// K_enc = BLAKE3_KDF(context="harbor topic encryption key", TopicID)
    /// K_mac = BLAKE3_KDF(context="harbor topic mac key", TopicID)
    /// ```
    ///
    /// # Security
    /// The TopicID is the master secret for the topic. Anyone with the
    /// TopicID can derive these keys and decrypt topic messages.
    pub fn derive(topic_id: &[u8; 32]) -> Self {
        Self {
            k_enc: derive_key(ENC_CONTEXT, topic_id),
            k_mac: derive_key(MAC_CONTEXT, topic_id),
        }
    }

    /// Derive keys from a TopicID slice (convenience method).
    ///
    /// Returns `None` if the slice is not exactly 32 bytes.
    pub fn derive_from_slice(topic_id: &[u8]) -> Option<Self> {
        if topic_id.len() != 32 {
            return None;
        }
        let mut id = [0u8; 32];
        id.copy_from_slice(topic_id);
        Some(Self::derive(&id))
    }
}

/// Compute HarborID from TopicID (BLAKE3 hash).
///
/// The HarborID is used to find Harbor Nodes for a topic without
/// revealing the TopicID itself.
///
/// ```text
/// HarborID = BLAKE3_HASH(TopicID)
/// ```
pub fn harbor_id_from_topic(topic_id: &[u8; 32]) -> [u8; 32] {
    *blake3::hash(topic_id).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_keys() {
        let topic_id = [42u8; 32];
        let keys = TopicKeys::derive(&topic_id);

        // Keys should be 32 bytes
        assert_eq!(keys.k_enc.len(), 32);
        assert_eq!(keys.k_mac.len(), 32);

        // Encryption and MAC keys should be different
        assert_ne!(keys.k_enc, keys.k_mac);
    }

    #[test]
    fn test_derive_keys_deterministic() {
        let topic_id = [42u8; 32];

        let keys1 = TopicKeys::derive(&topic_id);
        let keys2 = TopicKeys::derive(&topic_id);

        // Same input should produce same keys
        assert_eq!(keys1.k_enc, keys2.k_enc);
        assert_eq!(keys1.k_mac, keys2.k_mac);
    }

    #[test]
    fn test_different_topics_different_keys() {
        let topic1 = [1u8; 32];
        let topic2 = [2u8; 32];

        let keys1 = TopicKeys::derive(&topic1);
        let keys2 = TopicKeys::derive(&topic2);

        // Different topics should have different keys
        assert_ne!(keys1.k_enc, keys2.k_enc);
        assert_ne!(keys1.k_mac, keys2.k_mac);
    }

    #[test]
    fn test_derive_from_slice() {
        let topic_id = [42u8; 32];

        let keys1 = TopicKeys::derive(&topic_id);
        let keys2 = TopicKeys::derive_from_slice(&topic_id).unwrap();

        assert_eq!(keys1.k_enc, keys2.k_enc);
        assert_eq!(keys1.k_mac, keys2.k_mac);
    }

    #[test]
    fn test_derive_from_slice_wrong_length() {
        let short = [42u8; 16];
        assert!(TopicKeys::derive_from_slice(&short).is_none());

        let long = [42u8; 64];
        assert!(TopicKeys::derive_from_slice(&long).is_none());

        let empty: [u8; 0] = [];
        assert!(TopicKeys::derive_from_slice(&empty).is_none());
    }

    #[test]
    fn test_harbor_id_from_topic() {
        let topic_id = [42u8; 32];
        let harbor_id = harbor_id_from_topic(&topic_id);

        // Should be 32 bytes
        assert_eq!(harbor_id.len(), 32);

        // Should be different from topic_id (it's a hash)
        assert_ne!(harbor_id, topic_id);

        // Should be deterministic
        let harbor_id2 = harbor_id_from_topic(&topic_id);
        assert_eq!(harbor_id, harbor_id2);
    }

    #[test]
    fn test_different_topics_different_harbor_ids() {
        let topic1 = [1u8; 32];
        let topic2 = [2u8; 32];

        let harbor1 = harbor_id_from_topic(&topic1);
        let harbor2 = harbor_id_from_topic(&topic2);

        assert_ne!(harbor1, harbor2);
    }

    #[test]
    fn test_all_zero_topic_id() {
        // Edge case: all-zero topic ID should still produce valid keys
        let zero_topic = [0u8; 32];
        let keys = TopicKeys::derive(&zero_topic);

        // Should produce valid 32-byte keys
        assert_eq!(keys.k_enc.len(), 32);
        assert_eq!(keys.k_mac.len(), 32);

        // Keys should still be different from each other
        assert_ne!(keys.k_enc, keys.k_mac);

        // Keys should be non-zero (KDF output is pseudorandom)
        assert_ne!(keys.k_enc, [0u8; 32]);
        assert_ne!(keys.k_mac, [0u8; 32]);
    }

    #[test]
    fn test_all_zero_topic_harbor_id() {
        // Edge case: all-zero topic ID should produce valid harbor ID
        let zero_topic = [0u8; 32];
        let harbor_id = harbor_id_from_topic(&zero_topic);

        assert_eq!(harbor_id.len(), 32);
        assert_ne!(harbor_id, zero_topic);
        assert_ne!(harbor_id, [0u8; 32]);
    }

    #[test]
    fn test_debug_does_not_expose_keys() {
        let topic_id = [42u8; 32];
        let keys = TopicKeys::derive(&topic_id);
        let debug_output = format!("{:?}", keys);

        // Debug output should redact both keys
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output should redact keys"
        );

        // Should NOT contain actual key bytes (check a few bytes as hex)
        let k_enc_hex = hex::encode(&keys.k_enc[0..4]);
        let k_mac_hex = hex::encode(&keys.k_mac[0..4]);
        assert!(
            !debug_output.contains(&k_enc_hex),
            "Debug should not contain k_enc bytes"
        );
        assert!(
            !debug_output.contains(&k_mac_hex),
            "Debug should not contain k_mac bytes"
        );
    }

    #[test]
    fn test_clone() {
        let topic_id = [42u8; 32];
        let keys1 = TopicKeys::derive(&topic_id);
        let keys2 = keys1.clone();

        assert_eq!(keys1.k_enc, keys2.k_enc);
        assert_eq!(keys1.k_mac, keys2.k_mac);
    }

    #[test]
    fn test_topic_key_known_vectors() {
        let topic_id = [42u8; 32];
        let keys = TopicKeys::derive(&topic_id);

        let expected_k_enc: [u8; 32] = [
            185, 42, 147, 25, 76, 145, 7, 13, 180, 161, 75, 130, 33, 188, 172, 8, 216, 73, 45, 12,
            112, 84, 148, 50, 98, 120, 104, 84, 133, 240, 142, 7,
        ];
        let expected_k_mac: [u8; 32] = [
            67, 226, 52, 11, 186, 5, 203, 26, 195, 119, 229, 240, 87, 137, 162, 113, 55, 8, 155,
            217, 108, 172, 109, 249, 119, 206, 117, 188, 192, 156, 17, 94,
        ];

        assert_eq!(keys.k_enc, expected_k_enc);
        assert_eq!(keys.k_mac, expected_k_mac);
    }

    #[test]
    fn test_harbor_id_known_vector() {
        let topic_id = [42u8; 32];
        let harbor_id = harbor_id_from_topic(&topic_id);
        let expected_harbor_id: [u8; 32] = [
            67, 52, 16, 1, 167, 38, 35, 224, 13, 89, 14, 139, 247, 127, 144, 185, 239, 153, 183,
            179, 28, 145, 232, 11, 130, 13, 78, 112, 98, 247, 57, 55,
        ];

        assert_eq!(harbor_id, expected_harbor_id);
    }

    #[test]
    fn test_single_bit_difference() {
        // Changing a single bit in topic_id should produce completely different keys
        let topic1 = [0u8; 32];
        let mut topic2 = [0u8; 32];
        topic2[0] = 1; // Single bit difference

        let keys1 = TopicKeys::derive(&topic1);
        let keys2 = TopicKeys::derive(&topic2);

        // Keys should be completely different (avalanche effect)
        assert_ne!(keys1.k_enc, keys2.k_enc);
        assert_ne!(keys1.k_mac, keys2.k_mac);

        // Count differing bytes - should be many (good avalanche)
        let enc_diff: usize = keys1
            .k_enc
            .iter()
            .zip(keys2.k_enc.iter())
            .filter(|(a, b)| a != b)
            .count();
        let mac_diff: usize = keys1
            .k_mac
            .iter()
            .zip(keys2.k_mac.iter())
            .filter(|(a, b)| a != b)
            .count();

        // At least half the bytes should differ (good avalanche property)
        assert!(enc_diff >= 16, "Poor avalanche effect on k_enc");
        assert!(mac_diff >= 16, "Poor avalanche effect on k_mac");
    }
}
