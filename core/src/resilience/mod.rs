//! Resilience module
//!
//! Provides protection against abuse:
//! - Proof of Work with adaptive scaling per ALPN
//! - Rate limiting per connection and per HarborID
//! - Storage management with quotas and eviction

pub mod proof_of_work;
pub mod rate_limit;
pub mod storage;

pub use proof_of_work::{
    PoWConfig, PoWResult, PoWStats, PoWVerifier, ProofOfWork, ScalingConfig, build_context,
};
pub use rate_limit::{RateLimitConfig, RateLimitResult, RateLimiter};
pub use storage::{StorageCheckResult, StorageConfig, StorageManager, StorageStats};
