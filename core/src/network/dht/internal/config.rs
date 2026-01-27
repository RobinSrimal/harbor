//! DHT configuration and factory functions

use std::time::Duration;

use super::routing::{K, ALPHA, Buckets};
use super::distance::Id;
use super::pool::DhtPool;
use super::api::ApiProtocol;
use crate::network::dht::actor::DhtActor;

/// Default interval for verifying candidate nodes (30 seconds)
pub const DEFAULT_CANDIDATE_VERIFY_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum candidates to verify per interval
pub const MAX_CANDIDATES_PER_VERIFY: usize = 10;

/// DHT configuration
#[derive(Debug, Clone)]
pub struct DhtConfig {
    /// K parameter (bucket size, default: 20)
    pub k: usize,
    /// Alpha parameter (concurrency, default: 3)
    pub alpha: usize,
    /// Whether this node is transient (won't be added to others' routing tables)
    pub transient: bool,
    /// Optional RNG seed for deterministic testing
    pub rng_seed: Option<[u8; 32]>,
    /// Interval for verifying candidate nodes
    pub candidate_verify_interval: Duration,
    /// Maximum candidates to verify per interval
    pub max_candidates_per_verify: usize,
}

impl Default for DhtConfig {
    fn default() -> Self {
        Self {
            k: K,
            alpha: ALPHA,
            transient: false,
            rng_seed: None,
            candidate_verify_interval: DEFAULT_CANDIDATE_VERIFY_INTERVAL,
            max_candidates_per_verify: MAX_CANDIDATES_PER_VERIFY,
        }
    }
}

impl DhtConfig {
    /// Configuration for a transient node
    pub fn transient() -> Self {
        Self {
            transient: true,
            ..Default::default()
        }
    }

    /// Configuration for a persistent node
    pub fn persistent() -> Self {
        Self {
            transient: false,
            ..Default::default()
        }
    }
}

/// Create a DHT actor (not yet running) and return it with clients
///
/// The caller must call `actor.set_service(weak_service)` and then
/// `tokio::spawn(actor.run())` after creating the DhtService.
pub fn create_dht_actor(
    local_id: Id,
    pool: DhtPool,
    config: DhtConfig,
    bootstrap: Vec<Id>,
) -> (DhtActor, irpc::Client<ApiProtocol>) {
    create_dht_actor_with_buckets(local_id, pool, config, bootstrap, None)
}

/// Create a DHT actor with pre-existing buckets (not yet running)
///
/// The caller must call `actor.set_service(weak_service)` and then
/// `tokio::spawn(actor.run())` after creating the DhtService.
pub fn create_dht_actor_with_buckets(
    local_id: Id,
    pool: DhtPool,
    config: DhtConfig,
    bootstrap: Vec<Id>,
    buckets: Option<Buckets>,
) -> (DhtActor, irpc::Client<ApiProtocol>) {
    DhtActor::new_with_buckets(local_id, pool, config, bootstrap, buckets)
}
