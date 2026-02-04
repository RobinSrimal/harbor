//! Resilience module
//!
//! Provides protection against abuse:
//! - Rate limiting per connection and per HarborID
//! - Storage management with quotas and eviction

pub mod rate_limit;
pub mod storage;

pub use rate_limit::{RateLimiter, RateLimitConfig, RateLimitResult};
pub use storage::{StorageManager, StorageConfig, StorageStats, StorageCheckResult};

