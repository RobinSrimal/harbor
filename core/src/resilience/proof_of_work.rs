//! Proof of Work for Harbor protocol
//!
//! Provides generalized PoW verification with:
//! - Configurable difficulty per ALPN
//! - Adaptive scaling (byte-based or request-based)
//! - Per-peer tracking for scaling state
//! - Context-bound challenges to prevent cross-ALPN replay
//!
//! # Hash Function
//!
//! `BLAKE3(context || timestamp || nonce)`
//!
//! Where `context` is ALPN-specific:
//! - Harbor: `harbor_id || packet_id`
//! - Control: `sender_id || message_type_byte`
//! - DHT: `sender_id || query_hash`
//! - Send: `sender_id || harbor_id`

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Get current Unix timestamp in seconds
fn current_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

// =============================================================================
// ProofOfWork - Wire format
// =============================================================================

/// Proof of Work solution attached to requests
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProofOfWork {
    /// Unix timestamp when PoW was computed (seconds)
    pub timestamp: i64,
    /// Nonce that produces the required leading zeros
    pub nonce: u64,
    /// Number of leading zero bits achieved
    pub difficulty_bits: u8,
}

impl ProofOfWork {
    /// Maximum iterations before giving up
    const MAX_ITERATIONS: u64 = 1 << 30; // ~1 billion

    /// Compute a new PoW for the given context and difficulty
    ///
    /// # Arguments
    /// * `context` - ALPN-specific context bytes
    /// * `difficulty_bits` - Required number of leading zero bits
    ///
    /// # Returns
    /// The computed ProofOfWork or None if max iterations exceeded
    pub fn compute(context: &[u8], difficulty_bits: u8) -> Option<Self> {
        let timestamp = current_timestamp();

        for nonce in 0..Self::MAX_ITERATIONS {
            let hash = Self::compute_hash_raw(context, timestamp, nonce);
            let leading_zeros = Self::count_leading_zeros_raw(&hash);

            if leading_zeros >= difficulty_bits {
                return Some(Self {
                    timestamp,
                    nonce,
                    difficulty_bits: leading_zeros,
                });
            }
        }

        None
    }

    /// Verify this PoW against the given context and freshness window
    ///
    /// # Arguments
    /// * `context` - ALPN-specific context bytes
    /// * `required_difficulty` - Minimum required leading zero bits
    /// * `max_age_secs` - Maximum age of timestamp in seconds
    ///
    /// # Returns
    /// `true` if hash has required leading zeros and timestamp is fresh
    pub fn verify(&self, context: &[u8], required_difficulty: u8, max_age_secs: i64) -> bool {
        // Check timestamp freshness
        let now = current_timestamp();
        let age = now - self.timestamp;
        if age < 0 || age > max_age_secs {
            return false;
        }

        // Check difficulty
        let hash = self.compute_hash(context);
        let leading_zeros = Self::count_leading_zeros_raw(&hash);
        leading_zeros >= required_difficulty
    }

    /// Compute the hash: BLAKE3(context || timestamp || nonce)
    fn compute_hash(&self, context: &[u8]) -> [u8; 32] {
        Self::compute_hash_raw(context, self.timestamp, self.nonce)
    }

    fn compute_hash_raw(context: &[u8], timestamp: i64, nonce: u64) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(context);
        hasher.update(&timestamp.to_le_bytes());
        hasher.update(&nonce.to_le_bytes());
        *hasher.finalize().as_bytes()
    }

    /// Count leading zero bits in a hash
    fn count_leading_zeros_raw(hash: &[u8; 32]) -> u8 {
        let mut zeros = 0u8;
        for byte in hash {
            if *byte == 0 {
                zeros += 8;
            } else {
                zeros += byte.leading_zeros() as u8;
                break;
            }
        }
        zeros
    }

    /// Get the actual difficulty achieved by this PoW
    pub fn achieved_difficulty(&self, context: &[u8]) -> u8 {
        let hash = self.compute_hash(context);
        Self::count_leading_zeros_raw(&hash)
    }
}

// =============================================================================
// Configuration
// =============================================================================

/// Adaptive scaling configuration
#[derive(Debug, Clone)]
pub enum ScalingConfig {
    /// Scale based on cumulative bytes (for Harbor, Send)
    ByteBased {
        /// Whether adaptive scaling is enabled
        enabled: bool,
        /// Time window in seconds
        window_secs: u64,
        /// Bytes before scaling kicks in
        byte_threshold: u64,
        /// Difficulty bits added per MB above threshold
        bits_per_mb: u8,
    },
    /// Scale based on request count (for Control, DHT)
    RequestBased {
        /// Whether adaptive scaling is enabled
        enabled: bool,
        /// Time window in seconds
        window_secs: u64,
        /// Requests before scaling kicks in
        threshold: u64,
        /// Difficulty bits added per request above threshold
        bits_per_request: u8,
    },
    /// No scaling (static difficulty)
    None,
}

impl ScalingConfig {
    /// Default byte-based scaling (for Harbor)
    pub fn byte_based_default() -> Self {
        Self::ByteBased {
            enabled: true,
            window_secs: 60,
            byte_threshold: 1024 * 1024, // 1 MB
            bits_per_mb: 2,
        }
    }

    /// Gentle byte-based scaling (for Send)
    pub fn byte_based_gentle() -> Self {
        Self::ByteBased {
            enabled: true,
            window_secs: 60,
            byte_threshold: 10 * 1024 * 1024, // 10 MB
            bits_per_mb: 1,
        }
    }

    /// Default request-based scaling (for Control/DHT)
    pub fn request_based_default() -> Self {
        Self::RequestBased {
            enabled: true,
            window_secs: 60,
            threshold: 10,
            bits_per_request: 2,
        }
    }

    /// Moderate request-based scaling (for DHT)
    pub fn request_based_moderate() -> Self {
        Self::RequestBased {
            enabled: true,
            window_secs: 60,
            threshold: 20,
            bits_per_request: 1,
        }
    }
}

/// Per-ALPN Proof of Work configuration
#[derive(Debug, Clone)]
pub struct PoWConfig {
    /// Base difficulty (leading zero bits required)
    pub base_difficulty: u8,
    /// Maximum difficulty after scaling (cap)
    pub max_difficulty: u8,
    /// Freshness window - max age of PoW timestamp in seconds
    pub max_age_secs: i64,
    /// Whether PoW is required for this ALPN
    pub enabled: bool,
    /// Scaling configuration
    pub scaling: ScalingConfig,
}

impl Default for PoWConfig {
    fn default() -> Self {
        Self {
            base_difficulty: 18,
            max_difficulty: 28,
            max_age_secs: 300, // 5 minutes
            enabled: true,
            scaling: ScalingConfig::request_based_default(),
        }
    }
}

impl PoWConfig {
    /// Harbor ALPN preset (byte-based, aggressive for storage protection)
    pub fn harbor() -> Self {
        Self {
            base_difficulty: 18,
            max_difficulty: 32,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::byte_based_default(),
        }
    }

    /// Control ALPN preset (request-based, aggressive)
    pub fn control() -> Self {
        Self {
            base_difficulty: 18,
            max_difficulty: 28,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::request_based_default(),
        }
    }

    /// DHT ALPN preset (request-based, moderate)
    pub fn dht() -> Self {
        Self {
            base_difficulty: 18,
            max_difficulty: 28,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::request_based_moderate(),
        }
    }

    /// Send ALPN preset (byte-based, gentle - high-frequency sync expected)
    pub fn send() -> Self {
        Self {
            base_difficulty: 8,
            max_difficulty: 20,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::byte_based_gentle(),
        }
    }

    /// Disabled preset (for Share/Sync/Stream or testing)
    pub fn disabled() -> Self {
        Self {
            base_difficulty: 0,
            max_difficulty: 0,
            max_age_secs: 300,
            enabled: false,
            scaling: ScalingConfig::None,
        }
    }

    /// Testing preset (low difficulty, no scaling)
    pub fn for_testing() -> Self {
        Self {
            base_difficulty: 4,
            max_difficulty: 8,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::None,
        }
    }
}

// =============================================================================
// Per-peer tracking
// =============================================================================

/// Sliding window for byte-based scaling
#[derive(Debug, Default)]
struct ByteWindow {
    /// (timestamp_secs, bytes) pairs in the window
    entries: Vec<(i64, u64)>,
}

impl ByteWindow {
    /// Add bytes and return total bytes in window
    fn add_and_total(&mut self, now: i64, window_secs: u64, bytes: u64) -> u64 {
        let cutoff = now - window_secs as i64;
        self.entries.retain(|(ts, _)| *ts > cutoff);
        self.entries.push((now, bytes));
        self.entries
            .iter()
            .fold(0u64, |acc, (_, b)| acc.saturating_add(*b))
    }

    /// Get total bytes in window (without adding)
    fn total(&self, now: i64, window_secs: u64) -> u64 {
        let cutoff = now - window_secs as i64;
        self.entries
            .iter()
            .filter(|(ts, _)| *ts > cutoff)
            .fold(0u64, |acc, (_, b)| acc.saturating_add(*b))
    }

    /// Check if window has any recent activity
    fn has_recent_activity(&self, now: i64, window_secs: u64) -> bool {
        let cutoff = now - (window_secs as i64 * 2);
        self.entries.iter().any(|(ts, _)| *ts > cutoff)
    }
}

/// Sliding window for request-based scaling
#[derive(Debug, Default)]
struct RequestWindow {
    /// Request timestamps in the window
    timestamps: Vec<i64>,
}

impl RequestWindow {
    /// Add request and return count in window
    fn add_and_count(&mut self, now: i64, window_secs: u64) -> u64 {
        let cutoff = now - window_secs as i64;
        self.timestamps.retain(|ts| *ts > cutoff);
        self.timestamps.push(now);
        self.timestamps.len() as u64
    }

    /// Get count in window (without adding)
    fn count(&self, now: i64, window_secs: u64) -> u64 {
        let cutoff = now - window_secs as i64;
        self.timestamps.iter().filter(|ts| **ts > cutoff).count() as u64
    }

    /// Check if window has any recent activity
    fn has_recent_activity(&self, now: i64, window_secs: u64) -> bool {
        let cutoff = now - (window_secs as i64 * 2);
        self.timestamps.iter().any(|ts| *ts > cutoff)
    }
}

/// Per-peer tracking for adaptive scaling
#[derive(Debug, Default)]
struct PeerScalingState {
    byte_window: ByteWindow,
    request_window: RequestWindow,
}

// =============================================================================
// PoWVerifier
// =============================================================================

/// Result of PoW verification
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoWResult {
    /// Request is allowed
    Allowed,
    /// PoW difficulty is insufficient
    InsufficientDifficulty {
        /// Required difficulty bits
        required: u8,
        /// Provided difficulty bits
        provided: u8,
    },
    /// PoW timestamp is too old or in the future
    Expired {
        /// Age in seconds (negative if in future)
        age_secs: i64,
        /// Maximum allowed age
        max_age_secs: i64,
    },
}

impl PoWResult {
    pub fn is_allowed(&self) -> bool {
        matches!(self, PoWResult::Allowed)
    }

    /// Get the required difficulty if rejection includes a hint
    pub fn required_difficulty(&self) -> Option<u8> {
        match self {
            PoWResult::InsufficientDifficulty { required, .. } => Some(*required),
            _ => None,
        }
    }
}

/// Statistics about the PoW verifier
#[derive(Debug, Clone)]
pub struct PoWStats {
    pub tracked_peers: usize,
}

/// Verifies Proof of Work and manages per-peer scaling state
pub struct PoWVerifier {
    config: PoWConfig,
    /// Per-peer scaling state (keyed by endpoint_id)
    peer_state: HashMap<[u8; 32], PeerScalingState>,
}

impl PoWVerifier {
    /// Create a new verifier with the given config
    pub fn new(config: PoWConfig) -> Self {
        Self {
            config,
            peer_state: HashMap::new(),
        }
    }

    /// Get the configuration
    pub fn config(&self) -> &PoWConfig {
        &self.config
    }

    /// Verify PoW for a request
    ///
    /// # Arguments
    /// * `pow` - The ProofOfWork from the request
    /// * `context` - The ALPN-specific context bytes
    /// * `peer_id` - The sender's endpoint ID (for scaling)
    /// * `bytes` - For ByteBased scaling: size of the payload
    ///
    /// # Returns
    /// * `PoWResult::Allowed` - PoW is valid
    /// * `PoWResult::InsufficientDifficulty` - PoW too weak
    /// * `PoWResult::Expired` - Timestamp out of range
    pub fn verify(
        &self,
        pow: &ProofOfWork,
        context: &[u8],
        peer_id: &[u8; 32],
        bytes: Option<u64>,
    ) -> PoWResult {
        if !self.config.enabled {
            return PoWResult::Allowed;
        }

        let now = current_timestamp();

        // Check timestamp freshness
        let age = now - pow.timestamp;
        if age < 0 || age > self.config.max_age_secs {
            return PoWResult::Expired {
                age_secs: age,
                max_age_secs: self.config.max_age_secs,
            };
        }

        // Get required difficulty (with scaling)
        let required = self.effective_difficulty(peer_id, bytes);

        // Verify the hash
        let achieved = pow.achieved_difficulty(context);
        if achieved < required {
            return PoWResult::InsufficientDifficulty {
                required,
                provided: achieved,
            };
        }

        PoWResult::Allowed
    }

    /// Get the current effective difficulty for a peer
    pub fn effective_difficulty(&self, peer_id: &[u8; 32], bytes: Option<u64>) -> u8 {
        if !self.config.enabled {
            return 0;
        }

        let now = current_timestamp();
        let base = self.config.base_difficulty;
        let max = self.config.max_difficulty;

        let scaling_add = match &self.config.scaling {
            ScalingConfig::ByteBased {
                enabled: true,
                window_secs,
                byte_threshold,
                bits_per_mb,
            } => {
                let state = self.peer_state.get(peer_id);
                let existing = state
                    .map(|s| s.byte_window.total(now, *window_secs))
                    .unwrap_or(0);
                let total_bytes = existing.saturating_add(bytes.unwrap_or(0));

                if total_bytes > *byte_threshold {
                    let excess_mb = total_bytes.saturating_sub(*byte_threshold) / (1024 * 1024);
                    let scaled = excess_mb.saturating_mul(u64::from(*bits_per_mb));
                    scaled.min(u64::from(u8::MAX)) as u8
                } else {
                    0
                }
            }
            ScalingConfig::RequestBased {
                enabled: true,
                window_secs,
                threshold,
                bits_per_request,
            } => {
                let state = self.peer_state.get(peer_id);
                let count = state
                    .map(|s| s.request_window.count(now, *window_secs).saturating_add(1)) // +1 for current
                    .unwrap_or(1);

                if count > *threshold {
                    let excess = count.saturating_sub(*threshold);
                    let scaled = excess.saturating_mul(u64::from(*bits_per_request));
                    scaled.min(u64::from(u8::MAX)) as u8
                } else {
                    0
                }
            }
            _ => 0,
        };

        base.saturating_add(scaling_add).min(max)
    }

    /// Record a successful request for scaling purposes
    /// Call this after verify() returns Allowed
    pub fn record_request(&mut self, peer_id: &[u8; 32], bytes: Option<u64>) {
        if !self.config.enabled {
            return;
        }

        let now = current_timestamp();

        match &self.config.scaling {
            ScalingConfig::ByteBased {
                enabled: true,
                window_secs,
                ..
            } => {
                if let Some(b) = bytes {
                    let state = self.peer_state.entry(*peer_id).or_default();
                    state.byte_window.add_and_total(now, *window_secs, b);
                }
            }
            ScalingConfig::RequestBased {
                enabled: true,
                window_secs,
                ..
            } => {
                let state = self.peer_state.entry(*peer_id).or_default();
                state.request_window.add_and_count(now, *window_secs);
            }
            _ => {}
        }
    }

    /// Clean up old entries to prevent memory bloat
    pub fn cleanup(&mut self) {
        let now = current_timestamp();
        let window_secs = match &self.config.scaling {
            ScalingConfig::ByteBased { window_secs, .. } => *window_secs,
            ScalingConfig::RequestBased { window_secs, .. } => *window_secs,
            ScalingConfig::None => return,
        };

        self.peer_state.retain(|_, state| {
            state.byte_window.has_recent_activity(now, window_secs)
                || state.request_window.has_recent_activity(now, window_secs)
        });
    }

    /// Get stats about tracked peers
    pub fn stats(&self) -> PoWStats {
        PoWStats {
            tracked_peers: self.peer_state.len(),
        }
    }
}

impl Default for PoWVerifier {
    fn default() -> Self {
        Self::new(PoWConfig::default())
    }
}

// =============================================================================
// Helper function for building context
// =============================================================================

/// Build PoW context from multiple byte slices
pub fn build_context(parts: &[&[u8]]) -> Vec<u8> {
    let total_len: usize = parts.iter().map(|p| p.len()).sum();
    let mut context = Vec::with_capacity(total_len);
    for part in parts {
        context.extend_from_slice(part);
    }
    context
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    // === ProofOfWork Tests ===

    #[test]
    fn test_pow_compute_and_verify() {
        let context = b"test_context";
        let pow = ProofOfWork::compute(context, 8).unwrap();
        assert!(pow.verify(context, 8, 300));
        assert!(pow.difficulty_bits >= 8);
    }

    #[test]
    fn test_pow_wrong_context_fails() {
        let context = b"test_context";
        let wrong_context = b"wrong_context";
        let pow = ProofOfWork::compute(context, 8).unwrap();

        // Should fail verification with wrong context
        let achieved = pow.achieved_difficulty(wrong_context);
        // Very unlikely to have 8+ leading zeros with wrong context
        assert!(achieved < 8 || !pow.verify(wrong_context, 8, 300));
    }

    #[test]
    fn test_pow_expired_fails() {
        let context = b"test_context";
        let mut pow = ProofOfWork::compute(context, 4).unwrap();
        pow.timestamp = current_timestamp() - 400; // 400 seconds ago
        assert!(!pow.verify(context, 4, 300)); // max_age is 300
    }

    #[test]
    fn test_pow_future_timestamp_fails() {
        let context = b"test_context";
        let mut pow = ProofOfWork::compute(context, 4).unwrap();
        pow.timestamp = current_timestamp() + 100; // 100 seconds in future
        assert!(!pow.verify(context, 4, 300));
    }

    #[test]
    fn test_pow_insufficient_difficulty() {
        let context = b"test_context";
        let pow = ProofOfWork::compute(context, 4).unwrap();
        // Request higher difficulty than achieved
        assert!(!pow.verify(context, 32, 300));
    }

    #[test]
    fn test_pow_serialization() {
        let pow = ProofOfWork {
            timestamp: 1234567890,
            nonce: 42,
            difficulty_bits: 16,
        };
        let bytes = postcard::to_allocvec(&pow).unwrap();
        let decoded: ProofOfWork = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.timestamp, pow.timestamp);
        assert_eq!(decoded.nonce, pow.nonce);
        assert_eq!(decoded.difficulty_bits, pow.difficulty_bits);
    }

    // === PoWConfig Tests ===

    #[test]
    fn test_config_presets() {
        let harbor = PoWConfig::harbor();
        assert_eq!(harbor.base_difficulty, 18);
        assert!(harbor.enabled);
        assert!(matches!(harbor.scaling, ScalingConfig::ByteBased { .. }));

        let control = PoWConfig::control();
        assert_eq!(control.base_difficulty, 18);
        assert!(matches!(
            control.scaling,
            ScalingConfig::RequestBased { .. }
        ));

        let send = PoWConfig::send();
        assert_eq!(send.base_difficulty, 8);
        assert!(matches!(send.scaling, ScalingConfig::ByteBased { .. }));

        let disabled = PoWConfig::disabled();
        assert!(!disabled.enabled);

        let testing = PoWConfig::for_testing();
        assert_eq!(testing.base_difficulty, 4);
        assert!(matches!(testing.scaling, ScalingConfig::None));
    }

    // === PoWVerifier Tests ===

    #[test]
    fn test_verifier_allows_valid_pow() {
        let config = PoWConfig::for_testing();
        let verifier = PoWVerifier::new(config);
        let context = b"test";
        let pow = ProofOfWork::compute(context, 4).unwrap();

        let result = verifier.verify(&pow, context, &test_id(1), None);
        assert!(result.is_allowed());
    }

    #[test]
    fn test_verifier_rejects_insufficient_difficulty() {
        let config = PoWConfig {
            base_difficulty: 16,
            max_difficulty: 20, // Must be >= base_difficulty
            ..PoWConfig::for_testing()
        };
        let verifier = PoWVerifier::new(config);
        let context = b"test";
        let pow = ProofOfWork::compute(context, 4).unwrap(); // Only 4 bits

        let result = verifier.verify(&pow, context, &test_id(1), None);
        assert!(!result.is_allowed());
        assert_eq!(result.required_difficulty(), Some(16));
    }

    #[test]
    fn test_verifier_disabled() {
        let config = PoWConfig::disabled();
        let verifier = PoWVerifier::new(config);

        // Even with a "bad" PoW, should pass when disabled
        let pow = ProofOfWork {
            timestamp: 0,
            nonce: 0,
            difficulty_bits: 0,
        };
        let result = verifier.verify(&pow, b"test", &test_id(1), None);
        assert!(result.is_allowed());
    }

    #[test]
    fn test_verifier_expired_pow() {
        let config = PoWConfig::for_testing();
        let verifier = PoWVerifier::new(config);
        let context = b"test";
        let mut pow = ProofOfWork::compute(context, 4).unwrap();
        pow.timestamp = current_timestamp() - 400;

        let result = verifier.verify(&pow, context, &test_id(1), None);
        assert!(matches!(result, PoWResult::Expired { .. }));
    }

    // === Scaling Tests ===

    #[test]
    fn test_byte_based_scaling() {
        let config = PoWConfig {
            base_difficulty: 8,
            max_difficulty: 20,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::ByteBased {
                enabled: true,
                window_secs: 60,
                byte_threshold: 1000,
                bits_per_mb: 4,
            },
        };
        let mut verifier = PoWVerifier::new(config);
        let peer_id = test_id(1);

        // First request: base difficulty
        assert_eq!(verifier.effective_difficulty(&peer_id, Some(500)), 8);

        // Record some bytes
        verifier.record_request(&peer_id, Some(500_000));

        // Should still be under threshold
        assert_eq!(verifier.effective_difficulty(&peer_id, Some(400_000)), 8);

        // Record more bytes to exceed threshold
        verifier.record_request(&peer_id, Some(600_000));

        // Now above 1MB threshold, should scale
        let eff = verifier.effective_difficulty(&peer_id, Some(100_000));
        assert!(eff > 8);
    }

    #[test]
    fn test_request_based_scaling() {
        let config = PoWConfig {
            base_difficulty: 8,
            max_difficulty: 20,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::RequestBased {
                enabled: true,
                window_secs: 60,
                threshold: 5,
                bits_per_request: 2,
            },
        };
        let mut verifier = PoWVerifier::new(config);
        let peer_id = test_id(1);

        // First 5 requests: base difficulty
        for _ in 0..5 {
            assert_eq!(verifier.effective_difficulty(&peer_id, None), 8);
            verifier.record_request(&peer_id, None);
        }

        // 6th request: scaled (1 above threshold * 2 bits = +2)
        let eff = verifier.effective_difficulty(&peer_id, None);
        assert_eq!(eff, 10); // 8 + 2
    }

    #[test]
    fn test_difficulty_capped_at_max() {
        let config = PoWConfig {
            base_difficulty: 8,
            max_difficulty: 12,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::RequestBased {
                enabled: true,
                window_secs: 60,
                threshold: 1,
                bits_per_request: 10, // Would exceed max
            },
        };
        let mut verifier = PoWVerifier::new(config);
        let peer_id = test_id(1);

        verifier.record_request(&peer_id, None);
        verifier.record_request(&peer_id, None);

        assert_eq!(verifier.effective_difficulty(&peer_id, None), 12); // Capped at max
    }

    #[test]
    fn test_different_peers_tracked_separately() {
        let config = PoWConfig {
            base_difficulty: 8,
            max_difficulty: 20,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::RequestBased {
                enabled: true,
                window_secs: 60,
                threshold: 2,
                bits_per_request: 2,
            },
        };
        let mut verifier = PoWVerifier::new(config);
        let peer1 = test_id(1);
        let peer2 = test_id(2);

        // Peer 1 makes many requests
        for _ in 0..5 {
            verifier.record_request(&peer1, None);
        }

        // Peer 1 should have scaled difficulty
        assert!(verifier.effective_difficulty(&peer1, None) > 8);

        // Peer 2 should still have base difficulty
        assert_eq!(verifier.effective_difficulty(&peer2, None), 8);
    }

    #[test]
    fn test_request_scaling_saturates_instead_of_wrapping() {
        let config = PoWConfig {
            base_difficulty: 1,
            max_difficulty: 250,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::RequestBased {
                enabled: true,
                window_secs: 60,
                threshold: 0,
                bits_per_request: 1,
            },
        };
        let mut verifier = PoWVerifier::new(config);
        let peer_id = test_id(9);

        for _ in 0..300 {
            verifier.record_request(&peer_id, None);
        }

        // Without saturating conversion this wrapped to a much smaller value.
        assert_eq!(verifier.effective_difficulty(&peer_id, None), 250);
    }

    #[test]
    fn test_byte_scaling_saturates_instead_of_wrapping() {
        let config = PoWConfig {
            base_difficulty: 1,
            max_difficulty: 250,
            max_age_secs: 300,
            enabled: true,
            scaling: ScalingConfig::ByteBased {
                enabled: true,
                window_secs: 60,
                byte_threshold: 0,
                bits_per_mb: 1,
            },
        };
        let mut verifier = PoWVerifier::new(config);
        let peer_id = test_id(10);

        // 300 MB recorded in-window.
        for _ in 0..300 {
            verifier.record_request(&peer_id, Some(1024 * 1024));
        }

        // Without saturating conversion this wrapped to a much smaller value.
        assert_eq!(verifier.effective_difficulty(&peer_id, Some(0)), 250);
    }

    // === Context Builder Tests ===

    #[test]
    fn test_build_context() {
        let context = build_context(&[&[1u8; 32], &[2u8; 32]]);
        assert_eq!(context.len(), 64);
        assert_eq!(&context[0..32], &[1u8; 32]);
        assert_eq!(&context[32..64], &[2u8; 32]);
    }

    #[test]
    fn test_build_context_with_byte() {
        let sender_id = [1u8; 32];
        let msg_type = 0x80u8;
        let context = build_context(&[&sender_id, &[msg_type]]);
        assert_eq!(context.len(), 33);
    }

    // === Stats and Cleanup Tests ===

    #[test]
    fn test_stats() {
        let config = PoWConfig {
            scaling: ScalingConfig::request_based_default(),
            ..PoWConfig::for_testing()
        };
        let mut verifier = PoWVerifier::new(config);

        assert_eq!(verifier.stats().tracked_peers, 0);

        verifier.record_request(&test_id(1), None);
        verifier.record_request(&test_id(2), None);

        assert_eq!(verifier.stats().tracked_peers, 2);
    }

    #[test]
    fn test_cleanup_disabled_scaling() {
        let config = PoWConfig {
            scaling: ScalingConfig::None,
            ..PoWConfig::for_testing()
        };
        let mut verifier = PoWVerifier::new(config);

        // Cleanup should be a no-op
        verifier.cleanup();
        assert_eq!(verifier.stats().tracked_peers, 0);
    }

    #[test]
    fn test_record_request_no_scaling_does_not_track_peer() {
        let config = PoWConfig {
            scaling: ScalingConfig::None,
            ..PoWConfig::for_testing()
        };
        let mut verifier = PoWVerifier::new(config);

        verifier.record_request(&test_id(1), None);
        assert_eq!(verifier.stats().tracked_peers, 0);
    }

    #[test]
    fn test_record_request_byte_scaling_ignores_missing_size() {
        let config = PoWConfig {
            scaling: ScalingConfig::ByteBased {
                enabled: true,
                window_secs: 60,
                byte_threshold: 1024,
                bits_per_mb: 1,
            },
            ..PoWConfig::for_testing()
        };
        let mut verifier = PoWVerifier::new(config);

        verifier.record_request(&test_id(2), None);
        assert_eq!(verifier.stats().tracked_peers, 0);
    }
}
