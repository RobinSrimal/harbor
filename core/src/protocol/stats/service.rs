//! Stats Service - Read-only protocol state aggregation
//!
//! Queries database and DHT to build stats/monitoring responses.

use std::sync::Arc;

use iroh::Endpoint;
use rusqlite::Connection as DbConnection;
use tokio::sync::Mutex;

use crate::data::dht::{count_all_entries as count_dht_entries, get_bucket_entries};
use crate::data::harbor::{get_active_harbor_ids, get_harbor_cache_stats};
use crate::data::send::get_packets_needing_replication;
use crate::data::{
    current_timestamp, get_all_topics, get_topic_members, get_peer_relay_info,
    LocalIdentity,
};
use crate::network::dht::DhtService;
use crate::security::harbor_id_from_topic;

use super::types::{
    DhtBucketInfo, DhtNodeInfo, DhtStats, HarborStats, IdentityStats, NetworkStats, OutgoingStats,
    ProtocolStats, TopicDetails, TopicMemberInfo, TopicSummary, TopicsStats,
};

/// Error during Stats operations
#[derive(Debug)]
pub enum StatsError {
    /// Database error
    Database(String),
    /// Topic not found
    TopicNotFound,
}

impl std::fmt::Display for StatsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StatsError::Database(e) => write!(f, "database error: {}", e),
            StatsError::TopicNotFound => write!(f, "topic not found"),
        }
    }
}

impl std::error::Error for StatsError {}

/// Time threshold for considering a DHT entry "fresh" (24 hours in seconds)
const DHT_FRESH_THRESHOLD_SECS: i64 = 24 * 60 * 60;

/// Stats service - read-only protocol state aggregation
pub struct StatsService {
    /// Iroh endpoint
    endpoint: Endpoint,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// DHT service (optional - may be disabled)
    dht_service: Option<Arc<DhtService>>,
}

impl StatsService {
    /// Create a new Stats service
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        dht_service: Option<Arc<DhtService>>,
    ) -> Self {
        Self {
            endpoint,
            identity,
            db,
            dht_service,
        }
    }

    /// Get comprehensive protocol statistics
    pub async fn get_stats(&self) -> Result<ProtocolStats, StatsError> {
        let db = self.db.lock().await;

        // Identity stats
        let endpoint_id = hex::encode(self.identity.public_key);
        let relay_url = self.endpoint.addr().relay_urls().next().map(|u| u.to_string());

        // Network stats
        let is_online = relay_url.is_some();

        // DHT stats from database (persisted)
        let total_dht_nodes = count_dht_entries(&db)
            .map_err(|e| StatsError::Database(e.to_string()))? as u32;

        // Count non-empty buckets from database
        let mut active_buckets = 0u32;
        for bucket in 0..=255u8 {
            let entries = get_bucket_entries(&db, bucket)
                .map_err(|e| StatsError::Database(e.to_string()))?;
            if !entries.is_empty() {
                active_buckets += 1;
            }
        }

        // DHT stats from in-memory routing table (real-time)
        // Drop db lock before async DHT call
        drop(db);
        let in_memory_nodes = if let Some(ref dht) = self.dht_service {
            match dht.get_routing_table().await {
                Ok(buckets) => buckets.iter().map(|b| b.len() as u32).sum(),
                Err(_) => 0,
            }
        } else {
            0
        };
        // Re-acquire db lock for remaining stats
        let db = self.db.lock().await;

        // Topics stats
        let topics = get_all_topics(&db).map_err(|e| StatsError::Database(e.to_string()))?;
        let subscribed_count = topics.len() as u32;

        let mut total_members = 0u32;
        for topic in &topics {
            let members = get_topic_members(&db, &topic.topic_id)
                .map_err(|e| StatsError::Database(e.to_string()))?;
            total_members += members.len() as u32;
        }

        // Outgoing stats
        let pending_packets = get_packets_needing_replication(&db)
            .map_err(|e| StatsError::Database(e.to_string()))?;
        let pending_count = pending_packets.len() as u32;

        // Harbor stats
        let harbor_ids = get_active_harbor_ids(&db)
            .map_err(|e| StatsError::Database(e.to_string()))?;
        let harbor_ids_served = harbor_ids.len() as u32;

        // Get storage stats from harbor cache
        let (packets_stored, storage_bytes) = get_harbor_cache_stats(&db).unwrap_or((0, 0));

        Ok(ProtocolStats {
            identity: IdentityStats {
                endpoint_id,
                relay_url,
            },
            network: NetworkStats {
                is_online,
                active_connections: 0,
            },
            dht: DhtStats {
                total_nodes: total_dht_nodes,
                in_memory_nodes,
                active_buckets,
                bootstrap_connected: in_memory_nodes > 0,
            },
            topics: TopicsStats {
                subscribed_count,
                total_members,
            },
            outgoing: OutgoingStats {
                pending_count,
                awaiting_receipts: pending_count,
                replicated_to_harbor: 0,
            },
            harbor: HarborStats {
                packets_stored,
                storage_bytes,
                harbor_ids_served,
            },
        })
    }

    /// Get detailed DHT bucket information
    pub async fn get_dht_buckets(&self) -> Result<Vec<DhtBucketInfo>, StatsError> {
        let db = self.db.lock().await;
        let mut buckets = Vec::new();

        for bucket_index in 0..=255u8 {
            let entries = get_bucket_entries(&db, bucket_index)
                .map_err(|e| StatsError::Database(e.to_string()))?;

            if !entries.is_empty() {
                let now = current_timestamp();
                let nodes: Vec<DhtNodeInfo> = entries
                    .into_iter()
                    .map(|entry| DhtNodeInfo {
                        endpoint_id: hex::encode(entry.endpoint_id),
                        address: None,
                        relay_url: None,
                        is_fresh: (now - entry.added_at) < DHT_FRESH_THRESHOLD_SECS,
                    })
                    .collect();

                buckets.push(DhtBucketInfo {
                    bucket_index,
                    node_count: nodes.len() as u32,
                    nodes,
                });
            }
        }

        Ok(buckets)
    }

    /// Get detailed information about a specific topic
    pub async fn get_topic_details(
        &self,
        topic_id: &[u8; 32],
    ) -> Result<TopicDetails, StatsError> {
        let db = self.db.lock().await;
        let our_id = self.identity.public_key;

        let member_ids = get_topic_members(&db, topic_id)
            .map_err(|e| StatsError::Database(e.to_string()))?;

        if member_ids.is_empty() {
            return Err(StatsError::TopicNotFound);
        }

        let members: Vec<TopicMemberInfo> = member_ids
            .into_iter()
            .map(|endpoint_id| {
                let relay_url = get_peer_relay_info(&db, &endpoint_id)
                    .ok()
                    .flatten()
                    .map(|(url, _)| url);
                TopicMemberInfo {
                    endpoint_id: hex::encode(endpoint_id),
                    relay_url,
                    is_self: endpoint_id == our_id,
                }
            })
            .collect();

        let harbor_id = harbor_id_from_topic(topic_id);

        // Drop db lock before async DHT call
        drop(db);

        let harbor_node_count = if let Some(ref dht) = self.dht_service {
            let harbor_dht_id = crate::network::dht::Id::new(harbor_id);
            match dht.find_closest(harbor_dht_id, Some(20)).await {
                Ok(nodes) => nodes.len() as u32,
                Err(e) => {
                    tracing::debug!(error = %e, "failed to query DHT for harbor nodes");
                    0u32
                }
            }
        } else {
            0u32
        };

        Ok(TopicDetails {
            topic_id: hex::encode(topic_id),
            harbor_id: hex::encode(harbor_id),
            members,
            harbor_node_count,
        })
    }

    /// List all topics with summary info
    pub async fn list_topic_summaries(&self) -> Result<Vec<TopicSummary>, StatsError> {
        let db = self.db.lock().await;
        let topics = get_all_topics(&db).map_err(|e| StatsError::Database(e.to_string()))?;

        let mut summaries = Vec::new();
        for topic in topics {
            let members = get_topic_members(&db, &topic.topic_id)
                .map_err(|e| StatsError::Database(e.to_string()))?;

            summaries.push(TopicSummary {
                topic_id: hex::encode(topic.topic_id),
                member_count: members.len() as u32,
            });
        }

        Ok(summaries)
    }
}
