//! Resilience module
//!
//! Provides protection against abuse:
//! - Rate limiting per connection and per HarborID
//! - Proof of Work for Store requests (topic-bound)
//! - Storage management with quotas and eviction

pub mod rate_limit;
pub mod proof_of_work;
pub mod storage;

pub use rate_limit::{RateLimiter, RateLimitConfig, RateLimitResult};
pub use proof_of_work::{ProofOfWork, PoWChallenge, PoWConfig, PoWVerifyResult, verify_pow, compute_pow};
pub use storage::{StorageManager, StorageConfig, StorageStats, StorageCheckResult};

