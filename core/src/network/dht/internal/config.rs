//! DHT configuration and factory functions

use std::time::Duration;

use super::routing::{K, ALPHA, Buckets};
use super::distance::Id;
use super::pool::DhtPool;
use super::api::ApiClient;
use crate::network::dht::protocol::RpcClient;
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

/// Create a DHT node and return clients for interaction
pub fn create_dht_node(
    local_id: Id,
    pool: DhtPool,
    config: DhtConfig,
    bootstrap: Vec<Id>,
) -> (RpcClient, ApiClient) {
    create_dht_node_with_buckets(local_id, pool, config, bootstrap, None)
}

/// Create a DHT node with pre-existing buckets
pub fn create_dht_node_with_buckets(
    local_id: Id,
    pool: DhtPool,
    config: DhtConfig,
    bootstrap: Vec<Id>,
    buckets: Option<Buckets>,
) -> (RpcClient, ApiClient) {
    let (actor, rpc_client, api_client) = DhtActor::new_with_buckets(
        local_id, pool, config, bootstrap, buckets
    );

    tokio::spawn(actor.run());

    (rpc_client, api_client)
}
