//! Stats & Monitoring API for the Protocol
//!
//! This module contains read-only methods for monitoring protocol state:
//! - get_stats: Comprehensive protocol statistics
//! - get_dht_buckets: DHT routing table details
//! - get_topic_details: Detailed topic information
//! - list_topic_summaries: Summary of all topics

mod types;

pub use types::{
    DhtBucketInfo, DhtNodeInfo, DhtStats, HarborStats, IdentityStats, NetworkStats, OutgoingStats,
    ProtocolStats, TopicDetails, TopicMemberInfo, TopicSummary, TopicsStats,
};

use crate::data::dht::{count_all_entries as count_dht_entries, get_bucket_entries};
use crate::data::harbor::{get_active_harbor_ids, get_harbor_cache_stats};
use crate::data::send::get_packets_needing_replication;
use crate::data::{
    current_timestamp, get_all_topics, get_topic_members, get_topic_members_with_info,
};
use crate::security::harbor_id_from_topic;

use super::core::Protocol;
use super::error::ProtocolError;

/// Time threshold for considering a DHT entry "fresh" (24 hours in seconds)
const DHT_FRESH_THRESHOLD_SECS: i64 = 24 * 60 * 60;

impl Protocol {
    /// Get comprehensive protocol statistics
    pub async fn get_stats(&self) -> Result<ProtocolStats, ProtocolError> {
        self.check_running().await?;

        let db = self.db.lock().await;

        // Identity stats
        let endpoint_id = hex::encode(self.identity.public_key);
        let relay_url = self.endpoint.addr().relay_urls().next().map(|u| u.to_string());

        // Network stats
        let is_online = relay_url.is_some();

        // DHT stats from database (persisted)
        let total_dht_nodes = count_dht_entries(&db)
            .map_err(|e| ProtocolError::Database(e.to_string()))? as u32;

        // Count non-empty buckets from database
        let mut active_buckets = 0u32;
        for bucket in 0..=255u8 {
            let entries = get_bucket_entries(&db, bucket)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;
            if !entries.is_empty() {
                active_buckets += 1;
            }
        }

        // DHT stats from in-memory routing table (real-time)
        // Drop db lock before async DHT call
        drop(db);
        let in_memory_nodes = if let Some(ref dht_client) = self.dht_client {
            match dht_client.get_routing_table().await {
                Ok(buckets) => buckets.iter().map(|b| b.len() as u32).sum(),
                Err(_) => 0,
            }
        } else {
            0
        };
        // Re-acquire db lock for remaining stats
        let db = self.db.lock().await;

        // Topics stats
        let topics = get_all_topics(&db).map_err(|e| ProtocolError::Database(e.to_string()))?;
        let subscribed_count = topics.len() as u32;

        let mut total_members = 0u32;
        for topic in &topics {
            let members = get_topic_members(&db, &topic.topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;
            total_members += members.len() as u32;
        }

        // Outgoing stats
        let pending_packets = get_packets_needing_replication(&db)
            .map_err(|e| ProtocolError::Database(e.to_string()))?;
        let pending_count = pending_packets.len() as u32;

        // Harbor stats
        let harbor_ids = get_active_harbor_ids(&db)
            .map_err(|e| ProtocolError::Database(e.to_string()))?;
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
                active_connections: 0, // Would need endpoint stats for this
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
                awaiting_receipts: pending_count, // Same for now
                replicated_to_harbor: 0,          // Would need to track this
            },
            harbor: HarborStats {
                packets_stored,
                storage_bytes,
                harbor_ids_served,
            },
        })
    }

    /// Get detailed DHT bucket information
    pub async fn get_dht_buckets(&self) -> Result<Vec<DhtBucketInfo>, ProtocolError> {
        self.check_running().await?;

        let db = self.db.lock().await;
        let mut buckets = Vec::new();

        for bucket_index in 0..=255u8 {
            let entries = get_bucket_entries(&db, bucket_index)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;

            if !entries.is_empty() {
                let now = current_timestamp();
                let nodes: Vec<DhtNodeInfo> = entries
                    .into_iter()
                    .map(|entry| DhtNodeInfo {
                        endpoint_id: hex::encode(entry.endpoint_id),
                        address: None,    // DHT entries in DB don't store addresses
                        relay_url: None,  // DHT entries don't store relay URLs
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
    ) -> Result<TopicDetails, ProtocolError> {
        self.check_running().await?;

        let db = self.db.lock().await;
        let our_id = self.identity.public_key;

        let members_info = get_topic_members_with_info(&db, topic_id)
            .map_err(|e| ProtocolError::Database(e.to_string()))?;

        if members_info.is_empty() {
            return Err(ProtocolError::TopicNotFound);
        }

        let members: Vec<TopicMemberInfo> = members_info
            .into_iter()
            .map(|m| TopicMemberInfo {
                endpoint_id: hex::encode(m.endpoint_id),
                relay_url: m.relay_url,
                is_self: m.endpoint_id == our_id,
            })
            .collect();

        let harbor_id = harbor_id_from_topic(topic_id);

        // Query DHT for nodes near the harbor_id
        // Drop db lock before async DHT call
        drop(db);

        let harbor_node_count = if let Some(ref dht_client) = self.dht_client {
            // Create an Id from the harbor_id for DHT lookup
            let harbor_dht_id = crate::network::dht::Id::new(harbor_id);

            // Find nodes closest to the harbor_id from local routing table
            match dht_client.find_closest(harbor_dht_id, Some(20)).await {
                Ok(nodes) => {
                    // Return count of nodes known to be near this harbor_id
                    nodes.len() as u32
                }
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
    pub async fn list_topic_summaries(&self) -> Result<Vec<TopicSummary>, ProtocolError> {
        self.check_running().await?;

        let db = self.db.lock().await;
        let topics = get_all_topics(&db).map_err(|e| ProtocolError::Database(e.to_string()))?;

        let mut summaries = Vec::new();
        for topic in topics {
            let members = get_topic_members(&db, &topic.topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;

            summaries.push(TopicSummary {
                topic_id: hex::encode(topic.topic_id),
                member_count: members.len() as u32,
            });
        }

        Ok(summaries)
    }
}

