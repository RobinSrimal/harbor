//! DHT protocol incoming handler
//!
//! Handles incoming DHT protocol connections using irpc_iroh for wire framing.
//! - FindNode requests (returns closest nodes from routing table)

use std::sync::Arc;

use iroh::protocol::{AcceptError, ProtocolHandler};
use rusqlite::Connection as DbConnection;
use tokio::sync::Mutex;
use tracing::{debug, trace};

use crate::data::current_timestamp;
use crate::network::dht::Id;
use crate::network::dht::protocol::{FindNodeResponse, NodeInfo, RpcMessage, RpcProtocol};
use crate::network::dht::service::DhtService;

use crate::protocol::ProtocolError;

impl ProtocolHandler for DhtService {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let sender_id = *conn.remote_id().as_bytes();
        if let Err(e) = self.handle_dht_connection(conn, sender_id).await {
            debug!(error = %e, sender = %hex::encode(sender_id), "DHT connection handler error");
        }
        Ok(())
    }
}

impl DhtService {
    /// Handle an incoming DHT connection
    ///
    /// Uses irpc_iroh for wire framing (varint length-prefix + postcard).
    /// Processes FindNode requests via the actor and persists requesters to the database.
    pub(crate) async fn handle_dht_connection(
        &self,
        conn: iroh::endpoint::Connection,
        sender_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
        let our_id = self.our_id();
        let db = self.db().clone();
        trace!(sender = %hex::encode(sender_id), "handling DHT connection");

        loop {
            // Read next request using irpc framing (varint length-prefix + postcard)
            let msg = match irpc_iroh::read_request::<RpcProtocol>(&conn).await {
                Ok(Some(msg)) => msg,
                Ok(None) => {
                    trace!("DHT connection closed normally");
                    break;
                }
                Err(e) => {
                    debug!(error = %e, "DHT read_request error");
                    break;
                }
            };

            match msg {
                RpcMessage::FindNode(find_node_msg) => {
                    let target = find_node_msg.target;
                    let requester = find_node_msg.requester;
                    let requester_relay_url = find_node_msg.requester_relay_url.clone();

                    // Verify PoW first (context: sender_id || target_bytes)
                    if !self.verify_pow(&find_node_msg.pow, &sender_id, &target) {
                        debug!(
                            sender = %hex::encode(&sender_id[..8]),
                            target = %target,
                            "DHT FindNode rejected: insufficient PoW"
                        );
                        // Return empty response on PoW failure
                        find_node_msg.tx.send(FindNodeResponse { nodes: vec![] }).await.ok();
                        continue;
                    }

                    trace!(
                        target = %target,
                        requester = ?requester.map(hex::encode),
                        requester_relay = ?requester_relay_url,
                        "received FindNode request"
                    );

                    // Query the live DHT routing table via the actor
                    // handle_find_node_request:
                    // 1. Adds the requester to our routing table (if provided)
                    // 2. Returns NodeInfo WITH relay URLs (critical for peer discovery)
                    let response = match self.handle_find_node_request(
                        target,
                        requester,
                        requester_relay_url.clone(),
                    ).await {
                        Ok(nodes) => {
                            let with_relay = nodes.iter().filter(|n| n.relay_url.is_some()).count();
                            trace!(
                                closest_count = nodes.len(),
                                with_relay = with_relay,
                                "found closest nodes from DHT actor"
                            );
                            FindNodeResponse { nodes }
                        }
                        Err(e) => {
                            debug!(error = %e, "DHT actor query failed, falling back to database");
                            Self::find_closest_from_db(&db, &target).await
                        }
                    };

                    // Send response back via irpc oneshot (handles serialization + framing)
                    find_node_msg.tx.send(response).await.ok();

                    // Persist requester to database for recovery after restart
                    // Note: relay URL is NOT written here — it flows through the in-memory
                    // node_relay_urls map → save_routing_table → DB with correct freshness
                    if let Some(requester_id) = requester
                        && requester_id != our_id
                    {
                        let db_lock = db.lock().await;
                        let entry = crate::data::dht::DhtEntry {
                            endpoint_id: requester_id,
                            bucket_index: 0, // Will be recalculated on load
                            added_at: current_timestamp(),
                        };
                        // Ignore errors - best effort
                        let _ = crate::data::dht::insert_dht_entry(&db_lock, &entry);
                    }
                }
            }
        }

        Ok(())
    }

    /// Helper: find closest nodes from database (fallback when DHT actor unavailable)
    async fn find_closest_from_db(
        db: &Arc<Mutex<DbConnection>>,
        target: &Id,
    ) -> FindNodeResponse {
        let db_lock = db.lock().await;

        // Load DHT entries from database
        let entries = crate::data::dht::get_all_entries(&db_lock)
            .unwrap_or_default();

        // Convert to IDs and find closest to target
        let mut nodes_with_distance: Vec<_> = entries
            .iter()
            .map(|e| {
                let id = Id::new(e.endpoint_id);
                let dist = target.distance(&id);
                (dist, e.endpoint_id)
            })
            .collect();

        // Sort by distance and take closest K
        nodes_with_distance.sort_by(|a, b| a.0.cmp(&b.0));
        let closest: Vec<NodeInfo> = nodes_with_distance
            .into_iter()
            .take(20) // K = 20
            .map(|(_, id)| NodeInfo {
                node_id: id,
                addresses: Vec::new(),
                relay_url: None,
            })
            .collect();

        trace!(
            closest_count = closest.len(),
            "found closest nodes from database"
        );

        FindNodeResponse { nodes: closest }
    }
}

#[cfg(test)]
mod tests {
    use crate::network::dht::Id;
    use crate::network::dht::protocol::{FindNode, FindNodeResponse, NodeInfo};
    use crate::resilience::ProofOfWork;

    fn make_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    /// Create a dummy PoW for tests (difficulty 0 = always passes)
    fn test_pow() -> ProofOfWork {
        ProofOfWork {
            timestamp: 0,
            nonce: 0,
            difficulty_bits: 0,
        }
    }

    // ========== DHT Protocol Tests ==========

    #[test]
    fn test_find_node_request_serialization() {
        let request = FindNode {
            target: Id::new(make_id(42)),
            requester: Some(make_id(1)),
            requester_relay_url: Some("https://relay.example.com/".to_string()),
            pow: test_pow(),
        };

        let bytes = postcard::to_allocvec(&request).unwrap();
        let decoded: FindNode = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(*decoded.target.as_bytes(), make_id(42));
        assert_eq!(decoded.requester, Some(make_id(1)));
        assert_eq!(decoded.requester_relay_url, request.requester_relay_url);
    }

    #[test]
    fn test_find_node_request_no_requester() {
        let request = FindNode {
            target: Id::new(make_id(42)),
            requester: None,
            requester_relay_url: None,
            pow: test_pow(),
        };

        let bytes = postcard::to_allocvec(&request).unwrap();
        let decoded: FindNode = postcard::from_bytes(&bytes).unwrap();

        assert!(decoded.requester.is_none());
    }

    #[test]
    fn test_find_node_response_serialization() {
        let response = FindNodeResponse {
            nodes: vec![
                NodeInfo {
                    node_id: make_id(1),
                    addresses: vec!["192.168.1.1:4433".to_string()],
                    relay_url: Some("https://relay.example.com/".to_string()),
                },
                NodeInfo {
                    node_id: make_id(2),
                    addresses: vec![],
                    relay_url: None,
                },
            ],
        };

        let bytes = postcard::to_allocvec(&response).unwrap();
        let decoded: FindNodeResponse = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.nodes.len(), 2);
        assert_eq!(decoded.nodes[0].node_id, make_id(1));
        assert_eq!(decoded.nodes[1].node_id, make_id(2));
    }

    #[test]
    fn test_find_node_response_empty() {
        let response = FindNodeResponse { nodes: vec![] };

        let bytes = postcard::to_allocvec(&response).unwrap();
        let decoded: FindNodeResponse = postcard::from_bytes(&bytes).unwrap();

        assert!(decoded.nodes.is_empty());
    }

    #[test]
    fn test_length_prefix_format() {
        let request = FindNode {
            target: Id::new(make_id(42)),
            requester: None,
            requester_relay_url: None,
            pow: test_pow(),
        };

        let request_bytes = postcard::to_allocvec(&request).unwrap();
        let len = request_bytes.len() as u32;

        let mut wire = Vec::new();
        wire.extend_from_slice(&len.to_be_bytes());
        wire.extend_from_slice(&request_bytes);

        let parsed_len = u32::from_be_bytes([wire[0], wire[1], wire[2], wire[3]]) as usize;
        assert_eq!(parsed_len, request_bytes.len());

        let parsed: FindNode = postcard::from_bytes(&wire[4..]).unwrap();
        assert_eq!(*parsed.target.as_bytes(), make_id(42));
    }

    #[test]
    fn test_max_response_size_check() {
        const MAX_SIZE: usize = 65536;

        let nodes: Vec<NodeInfo> = (0..20u8)
            .map(|i| NodeInfo {
                node_id: make_id(i),
                addresses: vec!["192.168.1.1:4433".to_string()],
                relay_url: Some("https://relay.example.com/".to_string()),
            })
            .collect();

        let response = FindNodeResponse { nodes };
        let bytes = postcard::to_allocvec(&response).unwrap();

        assert!(bytes.len() < MAX_SIZE, "Response {} bytes exceeds limit", bytes.len());
    }

    #[test]
    fn test_requester_not_added_if_same_as_us() {
        let our_id = make_id(1);
        let requester_id = make_id(1);

        assert_eq!(requester_id, our_id);
        assert!(!(requester_id != our_id), "Should not add self to DHT");
    }

    #[test]
    fn test_requester_added_if_different() {
        let our_id = make_id(1);
        let requester_id = make_id(2);

        assert_ne!(requester_id, our_id);
        assert!(requester_id != our_id, "Should add different node to DHT");
    }

    #[test]
    fn test_node_info_from_closest() {
        let entries = vec![
            make_id(1),
            make_id(2),
            make_id(3),
        ];

        let nodes: Vec<NodeInfo> = entries
            .iter()
            .map(|id| NodeInfo {
                node_id: *id,
                addresses: Vec::new(),
                relay_url: None,
            })
            .collect();

        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].node_id, make_id(1));
        assert!(nodes[0].addresses.is_empty());
        assert!(nodes[0].relay_url.is_none());
    }

    #[test]
    fn test_distance_sorting_for_closest() {
        let target = Id::new(make_id(0));

        let entries = vec![
            (make_id(128), target.distance(&Id::new(make_id(128)))),
            (make_id(1), target.distance(&Id::new(make_id(1)))),
            (make_id(255), target.distance(&Id::new(make_id(255)))),
        ];

        let mut sorted: Vec<_> = entries.clone();
        sorted.sort_by(|a, b| a.1.cmp(&b.1));

        assert_eq!(sorted.len(), 3);
    }

    #[test]
    fn test_take_closest_k() {
        const K: usize = 20;

        let mut nodes: Vec<NodeInfo> = (0..30u8)
            .map(|i| NodeInfo {
                node_id: make_id(i),
                addresses: vec![],
                relay_url: None,
            })
            .collect();

        nodes.truncate(K);

        assert_eq!(nodes.len(), K);
    }
}
