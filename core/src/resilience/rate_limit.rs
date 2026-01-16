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
    /// Maximum requests per window for a single connection
    pub max_requests_per_connection: u32,
    /// Maximum store requests per window for a single HarborID
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
                let retry_after = self.window_duration
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
                let retry_after = self.window_duration
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
}

impl RateLimiter {
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
        }
    }

    /// Check if a general request from a connection is allowed
    pub fn check_connection(&mut self, endpoint_id: &[u8; 32]) -> RateLimitResult {
        if !self.config.enabled {
            return RateLimitResult::Allowed;
        }

        let now = Instant::now();
        let window = self.connection_limits
            .entry(*endpoint_id)
            .or_insert_with(|| SlidingWindow::new(
                self.config.max_requests_per_connection,
                self.config.window_duration,
            ));

        window.check_and_record(now)
    }

    /// Check if a store request for a specific HarborID is allowed
    pub fn check_store(&mut self, harbor_id: &[u8; 32]) -> RateLimitResult {
        if !self.config.enabled {
            return RateLimitResult::Allowed;
        }

        let now = Instant::now();
        let window = self.harbor_limits
            .entry(*harbor_id)
            .or_insert_with(|| SlidingWindow::new(
                self.config.max_stores_per_harbor_id,
                self.config.window_duration,
            ));

        window.check_and_record(now)
    }

    /// Check both connection and store limits for a store request
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
        let now = Instant::now();
        let window_duration = self.config.window_duration;

        // Remove entries with no recent activity
        self.connection_limits.retain(|_, window| {
            window.requests.iter().any(|&t| {
                now.duration_since(t) < window_duration * 2
            })
        });

        self.harbor_limits.retain(|_, window| {
            window.requests.iter().any(|&t| {
                now.duration_since(t) < window_duration * 2
            })
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
        assert_eq!(limiter.check_connection(&test_id(1)), RateLimitResult::Allowed);
        assert_eq!(limiter.check_connection(&test_id(1)), RateLimitResult::Allowed);
        assert!(matches!(
            limiter.check_connection(&test_id(1)),
            RateLimitResult::Limited { .. }
        ));

        // Connection 2 should still be allowed
        assert_eq!(limiter.check_connection(&test_id(2)), RateLimitResult::Allowed);
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
        assert_eq!(limiter.check_connection(&test_id(1)), RateLimitResult::Allowed);
        assert_eq!(limiter.check_connection(&test_id(1)), RateLimitResult::Allowed);
        assert!(matches!(
            limiter.check_connection(&test_id(1)),
            RateLimitResult::Limited { .. }
        ));

        // Wait for window to pass
        sleep(Duration::from_millis(60));

        // Should be allowed again
        assert_eq!(limiter.check_connection(&test_id(1)), RateLimitResult::Allowed);
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
            assert_eq!(limiter.check_connection(&test_id(1)), RateLimitResult::Allowed);
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
}

