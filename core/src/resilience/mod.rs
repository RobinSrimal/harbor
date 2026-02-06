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
    ProofOfWork, PoWConfig, PoWResult, PoWStats, PoWVerifier, ScalingConfig, build_context,
};
pub use rate_limit::{RateLimiter, RateLimitConfig, RateLimitResult};
pub use storage::{StorageManager, StorageConfig, StorageStats, StorageCheckResult};

