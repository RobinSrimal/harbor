//! XOR distance metric for Kademlia DHT
//!
//! Uses 32-byte (256-bit) keyspace compatible with BLAKE3 hashes and Ed25519 public keys.

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};

/// 32-byte identifier used in the DHT
/// Can represent NodeId (Ed25519 public key) or HarborId (BLAKE3 hash of TopicId)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
pub struct Id(pub [u8; 32]);

impl Id {
    /// Zero ID (all bytes zero)
    pub const ZERO: Self = Self([0; 32]);

    /// Create an Id from a byte array
    pub fn new(bytes: [u8; 32]) -> Self {
        Id(bytes)
    }

    /// Create an Id from a BLAKE3 hash of data
    pub fn from_hash(data: &[u8]) -> Self {
        let hash = blake3::hash(data);
        Id(*hash.as_bytes())
    }

    /// Get the underlying bytes
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Calculate XOR distance to another Id
    pub fn distance(&self, other: &Id) -> Distance {
        Distance::between(&self.0, &other.0)
    }
}

impl From<[u8; 32]> for Id {
    fn from(bytes: [u8; 32]) -> Self {
        Id(bytes)
    }
}

impl AsRef<[u8; 32]> for Id {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", hex::encode(&self.0[..8]))
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

/// XOR distance between two 32-byte identifiers
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Distance([u8; 32]);

impl Distance {
    /// Maximum possible distance (all bits set)
    pub const MAX: Self = Self([0xFF; 32]);

    /// Zero distance (same ID)
    pub const ZERO: Self = Self([0; 32]);

    /// Calculate XOR distance between two 32-byte values
    pub fn between(a: &[u8; 32], b: &[u8; 32]) -> Self {
        let mut result = [0u8; 32];
        for i in 0..32 {
            result[i] = a[i] ^ b[i];
        }
        Self(result)
    }

    /// Get the bucket index for this distance
    ///
    /// Returns the number of leading zero bits (0-255).
    /// Bucket 0 is furthest (most significant bit differs),
    /// Bucket 255 is closest (only least significant bit differs).
    /// Returns None if distance is zero (same ID).
    pub fn bucket_index(&self) -> Option<usize> {
        let zeros = self.leading_zeros();
        if zeros >= 256 {
            None // Same ID
        } else {
            Some(zeros)
        }
    }

    /// Count leading zero bits
    fn leading_zeros(&self) -> usize {
        for (byte_idx, &byte) in self.0.iter().enumerate() {
            if byte != 0 {
                return byte_idx * 8 + byte.leading_zeros() as usize;
            }
        }
        256 // All zeros
    }

    /// Get the underlying bytes
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Reconstruct the original ID given the target
    /// If d = Distance::between(a, b), then d.inverse(b) == a
    pub fn inverse(&self, target: &[u8; 32]) -> [u8; 32] {
        let mut result = [0u8; 32];
        for i in 0..32 {
            result[i] = self.0[i] ^ target[i];
        }
        result
    }
}

impl Ord for Distance {
    fn cmp(&self, other: &Self) -> Ordering {
        // Compare byte by byte (big-endian, most significant first)
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for Distance {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Debug for Distance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Distance({})", hex::encode(&self.0[..8]))
    }
}

impl fmt::Display for Distance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_from_hash() {
        let data = b"test topic id";
        let id = Id::from_hash(data);
        
        // Should match direct BLAKE3 hash
        let expected = blake3::hash(data);
        assert_eq!(id.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn test_distance_symmetric() {
        let a = Id::new([1u8; 32]);
        let b = Id::new([2u8; 32]);
        
        assert_eq!(a.distance(&b), b.distance(&a));
    }

    #[test]
    fn test_distance_zero_to_self() {
        let a = Id::new([42u8; 32]);
        let dist = a.distance(&a);
        
        assert_eq!(dist, Distance::ZERO);
        assert_eq!(dist.bucket_index(), None);
    }

    #[test]
    fn test_distance_xor() {
        let a = Id::new([0xFF; 32]);
        let b = Id::new([0x00; 32]);
        
        let dist = a.distance(&b);
        assert_eq!(dist, Distance::MAX);
    }

    #[test]
    fn test_bucket_index() {
        // IDs that differ in the first bit
        let a = Id::new([0x80; 32]); // 10000000...
        let b = Id::new([0x00; 32]); // 00000000...
        assert_eq!(a.distance(&b).bucket_index(), Some(0));

        // IDs that differ in the second bit
        let c = Id::new([0x40; 32]); // 01000000...
        let d = Id::new([0x00; 32]); // 00000000...
        assert_eq!(c.distance(&d).bucket_index(), Some(1));

        // IDs that differ only in the last bit
        let mut e_bytes = [0u8; 32];
        e_bytes[31] = 0x01;
        let e = Id::new(e_bytes);
        let f = Id::new([0u8; 32]);
        assert_eq!(e.distance(&f).bucket_index(), Some(255));
    }

    #[test]
    fn test_distance_ordering() {
        let target = Id::new([0u8; 32]);
        
        // Closer to target (smaller XOR distance)
        let close = Id::new([0x01; 32]);
        // Further from target (larger XOR distance)
        let far = Id::new([0xFF; 32]);
        
        let dist_close = target.distance(&close);
        let dist_far = target.distance(&far);
        
        assert!(dist_close < dist_far);
    }

    #[test]
    fn test_distance_inverse() {
        let a = [0x42u8; 32];
        let b = [0x24u8; 32];
        
        let dist = Distance::between(&a, &b);
        
        // inverse(b) should give us back a
        assert_eq!(dist.inverse(&b), a);
        // inverse(a) should give us back b
        assert_eq!(dist.inverse(&a), b);
    }

    #[test]
    fn test_id_debug_display() {
        let id = Id::new([0xAB; 32]);
        
        // Debug shows shortened hex
        let debug = format!("{:?}", id);
        assert!(debug.contains("abababab"));
        
        // Display shows full hex
        let display = format!("{}", id);
        assert_eq!(display.len(), 64); // 32 bytes * 2 hex chars
    }

    #[test]
    fn test_leading_zeros_various() {
        // All zeros - maximum leading zeros
        let zero = Distance([0u8; 32]);
        assert_eq!(zero.leading_zeros(), 256);
        
        // First byte is 0x80 (10000000) - 0 leading zeros
        let mut bytes = [0u8; 32];
        bytes[0] = 0x80;
        assert_eq!(Distance(bytes).leading_zeros(), 0);
        
        // First byte is 0x01 (00000001) - 7 leading zeros
        let mut bytes = [0u8; 32];
        bytes[0] = 0x01;
        assert_eq!(Distance(bytes).leading_zeros(), 7);
        
        // First byte 0, second byte 0x80 - 8 leading zeros
        let mut bytes = [0u8; 32];
        bytes[1] = 0x80;
        assert_eq!(Distance(bytes).leading_zeros(), 8);
    }

    #[test]
    fn test_id_zero_and_default() {
        assert_eq!(Id::ZERO, Id::default());
        assert_eq!(Id::ZERO.as_bytes(), &[0u8; 32]);
    }

    #[test]
    fn test_id_from_trait() {
        let bytes = [0xABu8; 32];
        let id: Id = bytes.into();
        assert_eq!(id.as_bytes(), &bytes);
    }

    #[test]
    fn test_id_as_ref_trait() {
        let id = Id::new([0x42u8; 32]);
        let bytes_ref: &[u8; 32] = id.as_ref();
        assert_eq!(bytes_ref, &[0x42u8; 32]);
    }
}

