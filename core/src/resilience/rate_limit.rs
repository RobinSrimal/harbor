//! Rate limiting for Harbor protocol
//!
//! Provides sliding window rate limiting:
//! - Per-connection limits (for all request types)
//! - Per-HarborID limits (prevent flooding one topic)

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Configuration for rate limiting
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum requests per window for a single connection.
    /// Set to 0 to deny all connection-scoped requests.
    pub max_requests_per_connection: u32,
    /// Maximum store requests per window for a single HarborID.
    /// Set to 0 to deny all store requests for every HarborID.
    pub max_stores_per_harbor_id: u32,
    /// Window duration
    pub window_duration: Duration,
    /// Whether rate limiting is enabled
    pub enabled: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests_per_connection: 100,
            max_stores_per_harbor_id: 50,
            window_duration: Duration::from_secs(60),
            enabled: true,
        }
    }
}

/// Result of a rate limit check
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitResult {
    /// Request is allowed
    Allowed,
    /// Request is rate limited
    Limited {
        /// Time until the limit resets
        retry_after: Duration,
    },
}

/// Sliding window counter for rate limiting
#[derive(Debug)]
struct SlidingWindow {
    /// Timestamps of requests in the current window
    requests: Vec<Instant>,
    /// Maximum allowed requests
    max_requests: u32,
    /// Window duration
    window_duration: Duration,
}

impl SlidingWindow {
    fn new(max_requests: u32, window_duration: Duration) -> Self {
        Self {
            requests: Vec::new(),
            max_requests,
            window_duration,
        }
    }

    /// Check if a request is allowed and record it if so
    fn check_and_record(&mut self, now: Instant) -> RateLimitResult {
        // Remove old requests outside the window
        let cutoff = now.checked_sub(self.window_duration).unwrap_or(now);
        self.requests.retain(|&t| t > cutoff);

        if self.requests.len() >= self.max_requests as usize {
            // Rate limited - calculate retry time
            if let Some(&oldest) = self.requests.first() {
                let retry_after = self
                    .window_duration
                    .saturating_sub(now.duration_since(oldest));
                return RateLimitResult::Limited { retry_after };
            }
            return RateLimitResult::Limited {
                retry_after: self.window_duration,
            };
        }

        // Allowed - record the request
        self.requests.push(now);
        RateLimitResult::Allowed
    }

    /// Check without recording (peek)
    ///
    /// Useful for read-only checks where you don't want to consume the rate limit.
    /// Currently unused but kept for future use cases like:
    /// - Checking if a request WOULD be allowed before doing expensive work
    /// - Monitoring/stats endpoints
    #[allow(dead_code)]
    fn check(&self, now: Instant) -> RateLimitResult {
        let cutoff = now.checked_sub(self.window_duration).unwrap_or(now);
        let active_count = self.requests.iter().filter(|&&t| t > cutoff).count();

        if active_count >= self.max_requests as usize {
            if let Some(&oldest) = self.requests.iter().find(|&&t| t > cutoff) {
                let retry_after = self
                    .window_duration
                    .saturating_sub(now.duration_since(oldest));
                return RateLimitResult::Limited { retry_after };
            }
            return RateLimitResult::Limited {
                retry_after: self.window_duration,
            };
        }

        RateLimitResult::Allowed
    }
}

/// Rate limiter for Harbor protocol
#[derive(Debug)]
pub struct RateLimiter {
    config: RateLimitConfig,
    /// Per-connection rate limits (keyed by EndpointID)
    connection_limits: HashMap<[u8; 32], SlidingWindow>,
    /// Per-HarborID rate limits for store operations
    harbor_limits: HashMap<[u8; 32], SlidingWindow>,
    /// Number of rate-limit checks since construction.
    check_counter: u64,
}

impl RateLimiter {
    /// Run opportunistic cleanup after this many checks.
    const AUTO_CLEANUP_CHECK_INTERVAL: u64 = 64;

    /// Create a new rate limiter with default config
    pub fn new() -> Self {
        Self::with_config(RateLimitConfig::default())
    }

    /// Create a new rate limiter with custom config
    pub fn with_config(config: RateLimitConfig) -> Self {
        Self {
            config,
            connection_limits: HashMap::new(),
            harbor_limits: HashMap::new(),
            check_counter: 0,
        }
    }

    fn maybe_cleanup(&mut self, now: Instant) {
        self.check_counter = self.check_counter.saturating_add(1);
        if self.check_counter % Self::AUTO_CLEANUP_CHECK_INTERVAL == 0 {
            self.cleanup_with_now(now);
        }
    }

    /// Check if a general request from a connection is allowed
    pub fn check_connection(&mut self, endpoint_id: &[u8; 32]) -> RateLimitResult {
        if !self.config.enabled {
            return RateLimitResult::Allowed;
        }

        let now = Instant::now();
        let window = self
            .connection_limits
            .entry(*endpoint_id)
            .or_insert_with(|| {
                SlidingWindow::new(
                    self.config.max_requests_per_connection,
                    self.config.window_duration,
                )
            });

        let result = window.check_and_record(now);
        self.maybe_cleanup(now);
        result
    }

    /// Check if a store request for a specific HarborID is allowed
    pub fn check_store(&mut self, harbor_id: &[u8; 32]) -> RateLimitResult {
        if !self.config.enabled {
            return RateLimitResult::Allowed;
        }

        let now = Instant::now();
        let window = self.harbor_limits.entry(*harbor_id).or_insert_with(|| {
            SlidingWindow::new(
                self.config.max_stores_per_harbor_id,
                self.config.window_duration,
            )
        });

        let result = window.check_and_record(now);
        self.maybe_cleanup(now);
        result
    }

    /// Check both connection and store limits for a store request
    ///
    /// Note: Connection checks run first and consume connection budget even if
    /// the subsequent HarborID check rejects the request.
    pub fn check_store_request(
        &mut self,
        endpoint_id: &[u8; 32],
        harbor_id: &[u8; 32],
    ) -> RateLimitResult {
        // Check connection limit first
        let conn_result = self.check_connection(endpoint_id);
        if conn_result != RateLimitResult::Allowed {
            return conn_result;
        }

        // Then check harbor limit
        self.check_store(harbor_id)
    }

    /// Clean up old entries to prevent memory bloat
    pub fn cleanup(&mut self) {
        self.cleanup_with_now(Instant::now());
    }

    fn cleanup_with_now(&mut self, now: Instant) {
        let stale_after = self.config.window_duration.saturating_mul(2);
        // Remove entries with no recent activity
        self.connection_limits.retain(|_, window| {
            window
                .requests
                .iter()
                .any(|&t| now.saturating_duration_since(t) < stale_after)
        });

        self.harbor_limits.retain(|_, window| {
            window
                .requests
                .iter()
                .any(|&t| now.saturating_duration_since(t) < stale_after)
        });
    }

    /// Get current stats
    pub fn stats(&self) -> RateLimitStats {
        RateLimitStats {
            tracked_connections: self.connection_limits.len(),
            tracked_harbors: self.harbor_limits.len(),
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about the rate limiter
#[derive(Debug, Clone)]
pub struct RateLimitStats {
    pub tracked_connections: usize,
    pub tracked_harbors: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn test_id_u16(seed: u16) -> [u8; 32] {
        let mut id = [0u8; 32];
        id[..2].copy_from_slice(&seed.to_le_bytes());
        id
    }

    #[test]
    fn test_rate_limiter_allows_under_limit() {
        let config = RateLimitConfig {
            max_requests_per_connection: 5,
            max_stores_per_harbor_id: 3,
            window_duration: Duration::from_secs(1),
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);

        // Should allow up to 5 requests
        for _ in 0..5 {
            assert_eq!(
                limiter.check_connection(&test_id(1)),
                RateLimitResult::Allowed
            );
        }
    }

    #[test]
    fn test_rate_limiter_blocks_over_limit() {
        let config = RateLimitConfig {
            max_requests_per_connection: 3,
            max_stores_per_harbor_id: 3,
            window_duration: Duration::from_secs(10),
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);

        // Use up the limit
        for _ in 0..3 {
            assert_eq!(
                limiter.check_connection(&test_id(1)),
                RateLimitResult::Allowed
            );
        }

        // Should be limited now
        let result = limiter.check_connection(&test_id(1));
        assert!(matches!(result, RateLimitResult::Limited { .. }));
    }

    #[test]
    fn test_rate_limiter_per_connection() {
        let config = RateLimitConfig {
            max_requests_per_connection: 2,
            max_stores_per_harbor_id: 10,
            window_duration: Duration::from_secs(10),
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);

        // Connection 1 uses its limit
        assert_eq!(
            limiter.check_connection(&test_id(1)),
            RateLimitResult::Allowed
        );
        assert_eq!(
            limiter.check_connection(&test_id(1)),
            RateLimitResult::Allowed
        );
        assert!(matches!(
            limiter.check_connection(&test_id(1)),
            RateLimitResult::Limited { .. }
        ));

        // Connection 2 should still be allowed
        assert_eq!(
            limiter.check_connection(&test_id(2)),
            RateLimitResult::Allowed
        );
    }

    #[test]
    fn test_rate_limiter_per_harbor() {
        let config = RateLimitConfig {
            max_requests_per_connection: 100,
            max_stores_per_harbor_id: 2,
            window_duration: Duration::from_secs(10),
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);

        // Harbor 1 uses its limit
        assert_eq!(limiter.check_store(&test_id(1)), RateLimitResult::Allowed);
        assert_eq!(limiter.check_store(&test_id(1)), RateLimitResult::Allowed);
        assert!(matches!(
            limiter.check_store(&test_id(1)),
            RateLimitResult::Limited { .. }
        ));

        // Harbor 2 should still be allowed
        assert_eq!(limiter.check_store(&test_id(2)), RateLimitResult::Allowed);
    }

    #[test]
    fn test_rate_limiter_window_reset() {
        let config = RateLimitConfig {
            max_requests_per_connection: 2,
            max_stores_per_harbor_id: 2,
            window_duration: Duration::from_millis(50),
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);

        // Use up the limit
        assert_eq!(
            limiter.check_connection(&test_id(1)),
            RateLimitResult::Allowed
        );
        assert_eq!(
            limiter.check_connection(&test_id(1)),
            RateLimitResult::Allowed
        );
        assert!(matches!(
            limiter.check_connection(&test_id(1)),
            RateLimitResult::Limited { .. }
        ));

        // Wait for window to pass
        sleep(Duration::from_millis(60));

        // Should be allowed again
        assert_eq!(
            limiter.check_connection(&test_id(1)),
            RateLimitResult::Allowed
        );
    }

    #[test]
    fn test_rate_limiter_disabled() {
        let config = RateLimitConfig {
            max_requests_per_connection: 1,
            max_stores_per_harbor_id: 1,
            window_duration: Duration::from_secs(10),
            enabled: false,
        };
        let mut limiter = RateLimiter::with_config(config);

        // Should always allow when disabled
        for _ in 0..100 {
            assert_eq!(
                limiter.check_connection(&test_id(1)),
                RateLimitResult::Allowed
            );
            assert_eq!(limiter.check_store(&test_id(1)), RateLimitResult::Allowed);
        }
    }

    #[test]
    fn test_check_store_request_combined() {
        let config = RateLimitConfig {
            max_requests_per_connection: 5,
            max_stores_per_harbor_id: 2,
            window_duration: Duration::from_secs(10),
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);

        // First two should pass (harbor limit = 2)
        assert_eq!(
            limiter.check_store_request(&test_id(1), &test_id(10)),
            RateLimitResult::Allowed
        );
        assert_eq!(
            limiter.check_store_request(&test_id(1), &test_id(10)),
            RateLimitResult::Allowed
        );

        // Third should fail due to harbor limit (not connection limit)
        assert!(matches!(
            limiter.check_store_request(&test_id(1), &test_id(10)),
            RateLimitResult::Limited { .. }
        ));

        // Different harbor should still work
        assert_eq!(
            limiter.check_store_request(&test_id(1), &test_id(20)),
            RateLimitResult::Allowed
        );
    }

    #[test]
    fn test_cleanup() {
        let config = RateLimitConfig {
            max_requests_per_connection: 10,
            max_stores_per_harbor_id: 10,
            window_duration: Duration::from_millis(10),
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);

        // Add some entries
        limiter.check_connection(&test_id(1));
        limiter.check_store(&test_id(10));

        assert_eq!(limiter.stats().tracked_connections, 1);
        assert_eq!(limiter.stats().tracked_harbors, 1);

        // Wait for window to pass
        sleep(Duration::from_millis(30));

        // Cleanup should remove old entries
        limiter.cleanup();

        assert_eq!(limiter.stats().tracked_connections, 0);
        assert_eq!(limiter.stats().tracked_harbors, 0);
    }

    #[test]
    fn test_auto_cleanup_runs_during_checks() {
        let config = RateLimitConfig {
            max_requests_per_connection: 10,
            max_stores_per_harbor_id: 10,
            window_duration: Duration::from_millis(5),
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);

        // Add many distinct connection keys.
        for i in 0..100 {
            assert_eq!(
                limiter.check_connection(&test_id_u16(i)),
                RateLimitResult::Allowed
            );
        }
        assert_eq!(limiter.stats().tracked_connections, 100);

        // Let those entries become stale.
        sleep(Duration::from_millis(15));

        // Trigger opportunistic cleanup via repeated checks.
        for _ in 0..RateLimiter::AUTO_CLEANUP_CHECK_INTERVAL {
            let _ = limiter.check_connection(&test_id(250));
        }

        // Stale entries should be gone; only actively-used key remains.
        assert_eq!(limiter.stats().tracked_connections, 1);
    }

    #[test]
    fn test_cleanup_handles_extreme_window_duration() {
        let config = RateLimitConfig {
            max_requests_per_connection: 10,
            max_stores_per_harbor_id: 10,
            window_duration: Duration::MAX,
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);

        let _ = limiter.check_connection(&test_id(1));
        let _ = limiter.check_store(&test_id(2));

        // Should not panic or overflow.
        limiter.cleanup();

        assert_eq!(limiter.stats().tracked_connections, 1);
        assert_eq!(limiter.stats().tracked_harbors, 1);
    }

    #[test]
    fn test_check_store_request_consumes_connection_budget_first() {
        let config = RateLimitConfig {
            max_requests_per_connection: 1,
            max_stores_per_harbor_id: 0, // Harbor limit denies all store requests.
            window_duration: Duration::from_secs(10),
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);
        let endpoint = test_id(7);

        // Harbor limit rejects, but connection budget is still consumed first.
        assert!(matches!(
            limiter.check_store_request(&endpoint, &test_id(10)),
            RateLimitResult::Limited { .. }
        ));
        assert_eq!(limiter.stats().tracked_connections, 1);
        assert!(matches!(
            limiter.check_connection(&endpoint),
            RateLimitResult::Limited { .. }
        ));
    }

    #[test]
    fn test_zero_limits_deny_all() {
        let config = RateLimitConfig {
            max_requests_per_connection: 0,
            max_stores_per_harbor_id: 0,
            window_duration: Duration::from_secs(1),
            enabled: true,
        };
        let mut limiter = RateLimiter::with_config(config);

        assert!(matches!(
            limiter.check_connection(&test_id(1)),
            RateLimitResult::Limited { .. }
        ));
        assert!(matches!(
            limiter.check_store(&test_id(2)),
            RateLimitResult::Limited { .. }
        ));
    }
}
