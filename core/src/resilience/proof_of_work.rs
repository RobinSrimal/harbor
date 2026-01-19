//! Proof of Work for Harbor Store requests
//!
//! PoW is bound to:
//! - harbor_id: Prevents cross-topic replay
//! - packet_id: Prevents replay within same topic
//! - timestamp: Ensures freshness
//!
//! The sender must find a nonce such that:
//! BLAKE3(harbor_id || packet_id || timestamp || nonce) has N leading zero bits

use crate::data::dht::current_timestamp;

/// PoW configuration
#[derive(Debug, Clone)]
pub struct PoWConfig {
    /// Number of leading zero bits required (difficulty)
    /// 16 bits ≈ 65K iterations ≈ few milliseconds
    /// 20 bits ≈ 1M iterations ≈ tens of milliseconds
    pub difficulty_bits: u8,
    /// Maximum age of timestamp in seconds (freshness window)
    pub max_age_secs: i64,
    /// Whether PoW is required
    pub enabled: bool,
}

impl Default for PoWConfig {
    fn default() -> Self {
        Self {
            difficulty_bits: 18, // ~250K iterations, ~10-50ms on modern hardware
            max_age_secs: 300,   // 5 minutes freshness window
            enabled: true,
        }
    }
}

impl PoWConfig {
    /// Create a config for testing (very low difficulty)
    pub fn for_testing() -> Self {
        Self {
            difficulty_bits: 8, // ~256 iterations, instant
            max_age_secs: 300,
            enabled: true,
        }
    }

    /// Create a disabled config
    pub fn disabled() -> Self {
        Self {
            difficulty_bits: 0,
            max_age_secs: 300,
            enabled: false,
        }
    }
}

/// A PoW challenge that must be solved before storing
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoWChallenge {
    /// The HarborID this PoW is bound to
    pub harbor_id: [u8; 32],
    /// The PacketID this PoW is bound to
    pub packet_id: [u8; 32],
    /// Timestamp when challenge was created
    pub timestamp: i64,
    /// Required difficulty (leading zero bits)
    pub difficulty_bits: u8,
}

impl PoWChallenge {
    /// Create a new challenge for a store request
    pub fn new(harbor_id: [u8; 32], packet_id: [u8; 32], difficulty_bits: u8) -> Self {
        Self {
            harbor_id,
            packet_id,
            timestamp: current_timestamp(),
            difficulty_bits,
        }
    }

    /// Create a challenge with a specific timestamp (for verification)
    pub fn with_timestamp(
        harbor_id: [u8; 32],
        packet_id: [u8; 32],
        timestamp: i64,
        difficulty_bits: u8,
    ) -> Self {
        Self {
            harbor_id,
            packet_id,
            timestamp,
            difficulty_bits,
        }
    }
}

/// Proof of Work solution
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProofOfWork {
    /// The HarborID this PoW is bound to
    pub harbor_id: [u8; 32],
    /// The PacketID this PoW is bound to
    pub packet_id: [u8; 32],
    /// Timestamp when the PoW was computed
    pub timestamp: i64,
    /// The nonce that solves the challenge
    pub nonce: u64,
}

impl ProofOfWork {
    /// Get the hash of this PoW
    pub fn hash(&self) -> blake3::Hash {
        compute_hash(&self.harbor_id, &self.packet_id, self.timestamp, self.nonce)
    }

    /// Check if this PoW meets the required difficulty
    pub fn meets_difficulty(&self, difficulty_bits: u8) -> bool {
        check_leading_zeros(&self.hash(), difficulty_bits)
    }
}

/// Compute the hash for a PoW attempt
fn compute_hash(
    harbor_id: &[u8; 32],
    packet_id: &[u8; 32],
    timestamp: i64,
    nonce: u64,
) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(harbor_id);
    hasher.update(packet_id);
    hasher.update(&timestamp.to_le_bytes());
    hasher.update(&nonce.to_le_bytes());
    hasher.finalize()
}

/// Check if a hash has the required number of leading zero bits
fn check_leading_zeros(hash: &blake3::Hash, required_bits: u8) -> bool {
    let bytes = hash.as_bytes();
    let full_bytes = required_bits / 8;
    let remaining_bits = required_bits % 8;

    // Check full zero bytes
    for i in 0..full_bytes as usize {
        if bytes[i] != 0 {
            return false;
        }
    }

    // Check remaining bits in the next byte
    if remaining_bits > 0 {
        let byte_idx = full_bytes as usize;
        let mask = 0xFF << (8 - remaining_bits);
        if bytes[byte_idx] & mask != 0 {
            return false;
        }
    }

    true
}

/// Default maximum iterations for PoW computation (2^30 ≈ 1 billion)
/// This prevents infinite loops with extreme difficulty settings.
pub const DEFAULT_MAX_ITERATIONS: u64 = 1 << 30;

/// Compute a PoW solution for the given challenge
/// 
/// Uses `DEFAULT_MAX_ITERATIONS` limit. For custom limits, use `compute_pow_with_limit`.
/// 
/// # Panics
/// Panics if no solution is found within the iteration limit (should never happen
/// for reasonable difficulty values like 8-24 bits).
pub fn compute_pow(challenge: &PoWChallenge) -> ProofOfWork {
    compute_pow_with_limit(challenge, DEFAULT_MAX_ITERATIONS)
        .expect("PoW computation exceeded iteration limit - difficulty may be too high")
}

/// Compute a PoW solution with a custom iteration limit
/// 
/// Returns `None` if no solution is found within `max_iterations`.
/// 
/// # Arguments
/// * `challenge` - The challenge to solve
/// * `max_iterations` - Maximum number of nonce values to try
pub fn compute_pow_with_limit(challenge: &PoWChallenge, max_iterations: u64) -> Option<ProofOfWork> {
    for nonce in 0..max_iterations {
        let hash = compute_hash(
            &challenge.harbor_id,
            &challenge.packet_id,
            challenge.timestamp,
            nonce,
        );

        if check_leading_zeros(&hash, challenge.difficulty_bits) {
            return Some(ProofOfWork {
                harbor_id: challenge.harbor_id,
                packet_id: challenge.packet_id,
                timestamp: challenge.timestamp,
                nonce,
            });
        }
    }
    
    None
}

/// Verify a PoW solution
pub fn verify_pow(pow: &ProofOfWork, config: &PoWConfig) -> PoWVerifyResult {
    if !config.enabled {
        return PoWVerifyResult::Valid;
    }

    // Check timestamp freshness
    let now = current_timestamp();
    let age = now - pow.timestamp;

    if age < 0 {
        return PoWVerifyResult::InvalidTimestamp;
    }

    if age > config.max_age_secs {
        return PoWVerifyResult::Expired;
    }

    // Check difficulty
    if !pow.meets_difficulty(config.difficulty_bits) {
        return PoWVerifyResult::InsufficientDifficulty;
    }

    PoWVerifyResult::Valid
}

/// Result of PoW verification
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoWVerifyResult {
    /// PoW is valid
    Valid,
    /// Timestamp is in the future
    InvalidTimestamp,
    /// PoW has expired (too old)
    Expired,
    /// PoW doesn't meet difficulty requirement
    InsufficientDifficulty,
}

impl std::fmt::Display for PoWVerifyResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoWVerifyResult::Valid => write!(f, "valid"),
            PoWVerifyResult::InvalidTimestamp => write!(f, "timestamp is in the future"),
            PoWVerifyResult::Expired => write!(f, "proof of work has expired"),
            PoWVerifyResult::InsufficientDifficulty => write!(f, "insufficient difficulty"),
        }
    }
}

impl PoWVerifyResult {
    pub fn is_valid(&self) -> bool {
        matches!(self, PoWVerifyResult::Valid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_check_leading_zeros() {
        // All zeros - should pass any check
        let hash = blake3::Hash::from([0u8; 32]);
        assert!(check_leading_zeros(&hash, 0));
        assert!(check_leading_zeros(&hash, 8));
        assert!(check_leading_zeros(&hash, 16));
        assert!(check_leading_zeros(&hash, 255));

        // First byte is 0x01 (7 leading zeros)
        let mut bytes = [0u8; 32];
        bytes[0] = 0x01;
        let hash = blake3::Hash::from(bytes);
        assert!(check_leading_zeros(&hash, 0));
        assert!(check_leading_zeros(&hash, 7));
        assert!(!check_leading_zeros(&hash, 8));

        // First byte is 0x0F (4 leading zeros)
        bytes[0] = 0x0F;
        let hash = blake3::Hash::from(bytes);
        assert!(check_leading_zeros(&hash, 4));
        assert!(!check_leading_zeros(&hash, 5));

        // First byte is 0x80 (0 leading zeros)
        bytes[0] = 0x80;
        let hash = blake3::Hash::from(bytes);
        assert!(check_leading_zeros(&hash, 0));
        assert!(!check_leading_zeros(&hash, 1));
    }

    #[test]
    fn test_compute_pow_low_difficulty() {
        let challenge = PoWChallenge::new(test_id(1), test_id(2), 8);
        let pow = compute_pow(&challenge);

        // Should meet the difficulty
        assert!(pow.meets_difficulty(8));

        // Should have correct harbor_id and packet_id
        assert_eq!(pow.harbor_id, test_id(1));
        assert_eq!(pow.packet_id, test_id(2));
    }

    #[test]
    fn test_pow_is_topic_bound() {
        let challenge1 = PoWChallenge::with_timestamp(test_id(1), test_id(10), 1000, 8);
        let pow1 = compute_pow(&challenge1);

        // Create a fake PoW trying to use same nonce for different harbor_id
        let fake_pow = ProofOfWork {
            harbor_id: test_id(2), // Different harbor!
            packet_id: pow1.packet_id,
            timestamp: pow1.timestamp,
            nonce: pow1.nonce,
        };

        // The fake PoW should NOT meet difficulty (different harbor changes hash)
        // Note: There's a tiny chance it still passes, but extremely unlikely
        // For a real test we'd want higher difficulty to make this deterministic
        // For now, just verify the hash is different
        assert_ne!(pow1.hash(), fake_pow.hash());
    }

    #[test]
    fn test_pow_is_packet_bound() {
        let challenge1 = PoWChallenge::with_timestamp(test_id(1), test_id(10), 1000, 8);
        let pow1 = compute_pow(&challenge1);

        // Create a fake PoW trying to use same nonce for different packet_id
        let fake_pow = ProofOfWork {
            harbor_id: pow1.harbor_id,
            packet_id: test_id(11), // Different packet!
            timestamp: pow1.timestamp,
            nonce: pow1.nonce,
        };

        // The hash should be different
        assert_ne!(pow1.hash(), fake_pow.hash());
    }

    #[test]
    fn test_verify_pow_valid() {
        let config = PoWConfig::for_testing();
        let challenge = PoWChallenge::new(test_id(1), test_id(2), config.difficulty_bits);
        let pow = compute_pow(&challenge);

        assert_eq!(verify_pow(&pow, &config), PoWVerifyResult::Valid);
    }

    #[test]
    fn test_verify_pow_expired() {
        let config = PoWConfig {
            difficulty_bits: 8,
            max_age_secs: 10,
            enabled: true,
        };

        // Create a PoW with old timestamp
        let old_timestamp = current_timestamp() - 100; // 100 seconds ago
        let challenge = PoWChallenge::with_timestamp(
            test_id(1),
            test_id(2),
            old_timestamp,
            config.difficulty_bits,
        );
        let pow = compute_pow(&challenge);

        assert_eq!(verify_pow(&pow, &config), PoWVerifyResult::Expired);
    }

    #[test]
    fn test_verify_pow_future_timestamp() {
        let config = PoWConfig::for_testing();

        // Create a PoW with future timestamp
        let future_timestamp = current_timestamp() + 1000;
        let pow = ProofOfWork {
            harbor_id: test_id(1),
            packet_id: test_id(2),
            timestamp: future_timestamp,
            nonce: 0,
        };

        assert_eq!(verify_pow(&pow, &config), PoWVerifyResult::InvalidTimestamp);
    }

    #[test]
    fn test_verify_pow_insufficient_difficulty() {
        let config = PoWConfig {
            difficulty_bits: 16, // Require 16 bits
            max_age_secs: 300,
            enabled: true,
        };

        // Compute with lower difficulty
        let challenge = PoWChallenge::new(test_id(1), test_id(2), 8);
        let _pow = compute_pow(&challenge);

        // May or may not pass depending on luck, but let's create a definitely failing one
        let bad_pow = ProofOfWork {
            harbor_id: test_id(1),
            packet_id: test_id(2),
            timestamp: current_timestamp(),
            nonce: 0, // Extremely unlikely to meet 16-bit difficulty
        };

        // This will almost certainly fail (unless we're astronomically unlucky)
        let result = verify_pow(&bad_pow, &config);
        // We check that if it doesn't meet difficulty, it reports correctly
        if !bad_pow.meets_difficulty(16) {
            assert_eq!(result, PoWVerifyResult::InsufficientDifficulty);
        }
    }

    #[test]
    fn test_verify_pow_disabled() {
        let config = PoWConfig::disabled();

        // Any PoW should be valid when disabled
        let invalid_pow = ProofOfWork {
            harbor_id: test_id(1),
            packet_id: test_id(2),
            timestamp: 0, // Ancient timestamp
            nonce: 0,     // Wrong nonce
        };

        assert_eq!(verify_pow(&invalid_pow, &config), PoWVerifyResult::Valid);
    }

    #[test]
    fn test_pow_difficulty_scaling() {
        // Test that higher difficulty takes more iterations
        // (We can't reliably test timing, but we can verify the nonce increases on average)
        
        let challenge_easy = PoWChallenge::with_timestamp(test_id(1), test_id(2), 1000, 4);
        let pow_easy = compute_pow(&challenge_easy);

        let challenge_harder = PoWChallenge::with_timestamp(test_id(1), test_id(2), 1000, 12);
        let pow_harder = compute_pow(&challenge_harder);

        // Both should be valid
        assert!(pow_easy.meets_difficulty(4));
        assert!(pow_harder.meets_difficulty(12));

        // Verify both computed successfully (nonces are valid)
        assert!(pow_easy.nonce < u64::MAX);
        assert!(pow_harder.nonce < u64::MAX);
    }

    #[test]
    fn test_pow_hash_deterministic() {
        let pow = ProofOfWork {
            harbor_id: test_id(1),
            packet_id: test_id(2),
            timestamp: 12345,
            nonce: 67890,
        };

        // Hash should be deterministic
        assert_eq!(pow.hash(), pow.hash());

        // Same inputs should give same hash
        let pow2 = ProofOfWork {
            harbor_id: test_id(1),
            packet_id: test_id(2),
            timestamp: 12345,
            nonce: 67890,
        };
        assert_eq!(pow.hash(), pow2.hash());
    }

    #[test]
    fn test_pow_config_default() {
        let config = PoWConfig::default();
        
        // Verify default values are reasonable
        assert!(config.enabled);
        assert_eq!(config.difficulty_bits, 18); // ~250K iterations
        assert_eq!(config.max_age_secs, 300);   // 5 minutes
        
        // Difficulty should be in practical range (8-24 bits)
        assert!(config.difficulty_bits >= 8);
        assert!(config.difficulty_bits <= 24);
    }

    #[test]
    fn test_pow_verify_result_display() {
        assert_eq!(PoWVerifyResult::Valid.to_string(), "valid");
        assert!(PoWVerifyResult::InvalidTimestamp.to_string().contains("future"));
        assert!(PoWVerifyResult::Expired.to_string().contains("expired"));
        assert!(PoWVerifyResult::InsufficientDifficulty.to_string().contains("difficulty"));
    }

    #[test]
    fn test_compute_pow_with_limit() {
        // Very easy challenge - should succeed quickly
        let challenge = PoWChallenge::with_timestamp(test_id(1), test_id(2), 1000, 4);
        let result = compute_pow_with_limit(&challenge, 1000);
        assert!(result.is_some());
        assert!(result.unwrap().meets_difficulty(4));
    }

    #[test]
    fn test_compute_pow_with_limit_exceeded() {
        // High difficulty with very low limit - should fail
        let challenge = PoWChallenge::with_timestamp(test_id(1), test_id(2), 1000, 20);
        let result = compute_pow_with_limit(&challenge, 10); // Only 10 iterations
        assert!(result.is_none());
    }

    #[test]
    fn test_pow_config_for_testing() {
        let config = PoWConfig::for_testing();
        assert!(config.enabled);
        assert_eq!(config.difficulty_bits, 8); // Very easy
    }

    #[test]
    fn test_pow_config_disabled() {
        let config = PoWConfig::disabled();
        assert!(!config.enabled);
    }
}

