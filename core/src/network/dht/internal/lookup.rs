//! Kademlia iterative lookup algorithm
//!
//! Implements the iterative node lookup used for peer discovery.

use std::collections::{BTreeSet, HashSet};

use tokio::task::JoinSet;
use tracing::{debug, trace};

use super::config::DhtConfig;
use super::distance::{Distance, Id};
use super::pool::{DhtPool, DialInfo};
use crate::network::dht::service::{DhtService, WeakDhtService};

/// Perform iterative node lookup (Kademlia algorithm)
///
/// Starting from an initial set of nodes, iteratively queries nodes
/// for closer peers until convergence. Returns the K closest nodes found.
pub async fn iterative_find_node(
    target: Id,
    initial: Vec<Id>,
    local_id: Id,
    pool: DhtPool,
    config: DhtConfig,
    service: WeakDhtService,
) -> Vec<Id> {
    // Track candidates sorted by distance
    let mut candidates: BTreeSet<(Distance, Id)> = initial
        .into_iter()
        .filter(|id| *id != local_id)
        .map(|id| (target.distance(&id), id))
        .collect();

    // Track which nodes we've queried
    let mut queried: HashSet<Id> = HashSet::new();
    queried.insert(local_id);

    // Track results
    let mut results: BTreeSet<(Distance, Id)> = BTreeSet::new();
    results.insert((target.distance(&local_id), local_id));

    loop {
        // Get alpha unqueried candidates to query
        let to_query: Vec<Id> = candidates
            .iter()
            .filter(|(_, id)| !queried.contains(id))
            .take(config.alpha)
            .map(|(_, id)| *id)
            .collect();

        if to_query.is_empty() {
            break;
        }

        // Mark as queried
        for id in &to_query {
            queried.insert(*id);
        }

        // Get our relay URL to include in requests
        let home_relay_url = pool.home_relay_url();

        // Query nodes in parallel
        let mut query_tasks = JoinSet::new();
        for id in to_query {
            let pool = pool.clone();
            let target_for_rpc = target;
            let requester_for_rpc = if config.transient {
                None
            } else {
                Some(*local_id.as_bytes())
            };
            let relay_url_for_rpc = home_relay_url.clone();

            query_tasks.spawn(async move {
                // Create dial info - pool will merge with known relay URLs
                let dial_info = DialInfo::from_node_id(
                    iroh::EndpointId::from_bytes(id.as_bytes()).expect("valid node id")
                );

                match pool.get_connection(&dial_info).await {
                    Ok(conn) => {
                        // Send actual FindNode RPC over the connection
                        match DhtService::send_find_node_on_conn(conn.connection(), target_for_rpc, requester_for_rpc, relay_url_for_rpc).await {
                            Ok(response) => {
                                // Return full NodeInfo so caller can extract relay URLs
                                trace!(
                                    peer = %id,
                                    nodes_returned = response.nodes.len(),
                                    "FindNode RPC succeeded"
                                );
                                (id, Ok(response.nodes))
                            }
                            Err(e) => {
                                debug!(peer = %id, error = %e, "FindNode RPC failed");
                                (id, Err(e))
                            }
                        }
                    }
                    Err(e) => {
                        debug!(peer = %id, error = %e, "Failed to connect for FindNode");
                        (id, Err(e))
                    }
                }
            });
        }

        // Collect results
        while let Some(result) = query_tasks.join_next().await {
            let Ok((id, query_result)) = result else {
                continue;
            };

            match query_result {
                Ok(node_infos) => {
                    // Add to results
                    let dist = target.distance(&id);
                    results.insert((dist, id));

                    // Notify service that responding node is alive (verified by successful query)
                    if let Some(svc) = service.upgrade() {
                        svc.nodes_seen(&[*id.as_bytes()]).await.ok();
                    }

                    // Add new candidates for lookup AND notify DHT about discovered nodes
                    // Also register relay URLs for future connectivity AND store for sharing
                    let mut discovered: Vec<[u8; 32]> = Vec::new();
                    let mut discovered_relay_urls: Vec<([u8; 32], String)> = Vec::new();
                    for info in node_infos {
                        let node = Id::new(info.node_id);
                        if node != local_id && !queried.contains(&node) {
                            candidates.insert((target.distance(&node), node));
                            discovered.push(info.node_id);

                            // Register relay URL with pool for future connections
                            // AND store in node_relay_urls so we can share with others
                            if let Some(relay_url) = info.relay_url {
                                let node_id = iroh::EndpointId::from_bytes(&info.node_id)
                                    .expect("valid node id");
                                let dial_info = DialInfo::from_node_id_with_relay(node_id, relay_url.clone());
                                pool.register_dial_info(dial_info).await;
                                // Store for sharing with other nodes in FindNode responses
                                discovered_relay_urls.push((info.node_id, relay_url));
                            }
                        }
                    }

                    // Store discovered relay URLs in our node_relay_urls map
                    // This allows us to share them with other nodes in FindNode responses
                    if !discovered_relay_urls.is_empty() {
                        if let Some(svc) = service.upgrade() {
                            svc.set_node_relay_urls(discovered_relay_urls).await.ok();
                        }
                    }

                    // Add discovered nodes to DHT candidates for verification
                    // This enables proper peer discovery propagation - nodes learn
                    // about each other through query responses, not just direct contact
                    if !discovered.is_empty() {
                        debug!(
                            peer = %id,
                            discovered_count = discovered.len(),
                            "discovered nodes from FindNode response"
                        );
                        if let Some(svc) = service.upgrade() {
                            svc.add_candidates(&discovered).await.ok();
                        }
                    } else {
                        debug!(peer = %id, "FindNode response contained no new nodes");
                    }
                }
                Err(_) => {
                    // Node is dead
                    if let Some(svc) = service.upgrade() {
                        svc.nodes_dead(&[*id.as_bytes()]).await.ok();
                    }
                }
            }
        }

        // Truncate results to K
        while results.len() > config.k {
            results.pop_last();
        }

        // Check if we have K results and no better candidates
        // Guard against k=0 (would cause underflow)
        if config.k == 0 {
            break;
        }
        let kth_best = results.iter().nth(config.k - 1).map(|(d, _)| *d);
        let best_candidate = candidates.first().map(|(d, _)| *d);

        match (kth_best, best_candidate) {
            (Some(kth), Some(best)) if best >= kth => break,
            (Some(_), None) => break,
            _ => continue,
        }
    }

    results.into_iter().map(|(_, id)| id).collect()
}
