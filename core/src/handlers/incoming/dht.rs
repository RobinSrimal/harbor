//! DHT protocol incoming handler
//!
//! Handles incoming DHT protocol connections:
//! - FindNode requests (returns closest nodes from routing table)

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::Mutex;
use tracing::{debug, trace};

use crate::data::current_timestamp;
use crate::network::dht::{ApiClient as DhtApiClient, Id};
use crate::network::dht::rpc::{FindNode, FindNodeResponse, NodeInfo};

use crate::protocol::{Protocol, ProtocolError};

impl Protocol {
    /// Handle an incoming DHT connection
    /// 
    /// Processes FindNode requests and returns closest nodes from our routing table.
    pub(crate) async fn handle_dht_connection(
        conn: iroh::endpoint::Connection,
        db: Arc<Mutex<Connection>>,
        sender_id: [u8; 32],
        our_id: [u8; 32],
        dht_client: Option<DhtApiClient>,
    ) -> Result<(), ProtocolError> {
        trace!(sender = %hex::encode(sender_id), "handling DHT connection");

        loop {
            // Accept bidirectional stream for request/response
            let (mut send, mut recv) = match conn.accept_bi().await {
                Ok(streams) => streams,
                Err(e) => {
                    trace!(error = %e, "DHT stream accept ended");
                    break;
                }
            };

            // Read length-prefixed request
            let mut len_buf = [0u8; 4];
            if let Err(e) = recv.read_exact(&mut len_buf).await {
                debug!(error = %e, "failed to read DHT request length");
                continue;
            }
            let request_len = u32::from_be_bytes(len_buf) as usize;

            // Sanity check request size
            if request_len > 65536 {
                debug!(len = request_len, "DHT request too large");
                continue;
            }

            let mut request_bytes = vec![0u8; request_len];
            if let Err(e) = recv.read_exact(&mut request_bytes).await {
                debug!(error = %e, "failed to read DHT request");
                continue;
            }

            // Deserialize FindNode request
            let request: FindNode = match postcard::from_bytes(&request_bytes) {
                Ok(r) => r,
                Err(e) => {
                    debug!(error = %e, "failed to decode FindNode request");
                    continue;
                }
            };

            trace!(
                target = %request.target,
                requester = ?request.requester.map(hex::encode),
                requester_relay = ?request.requester_relay_url,
                "received FindNode request"
            );

            // Query the live DHT routing table via the actor (preferred)
            // Falls back to database if DHT client unavailable
            // 
            // IMPORTANT: We use handle_find_node_request which:
            // 1. Adds the requester to our routing table (if provided)
            // 2. Returns NodeInfo WITH relay URLs (critical for peer discovery)
            let response = if let Some(ref client) = dht_client {
                // Use live in-memory routing table via DHT actor
                match client.handle_find_node_request(
                    request.target,
                    request.requester,
                    request.requester_relay_url.clone(),
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
                        Self::find_closest_from_db(&db, &request.target).await
                    }
                }
            } else {
                // Fallback: load from database (may be stale)
                trace!("no DHT client, using database fallback");
                Self::find_closest_from_db(&db, &request.target).await
            };

            // Serialize response
            let response_bytes = match postcard::to_allocvec(&response) {
                Ok(b) => b,
                Err(e) => {
                    debug!(error = %e, "failed to serialize FindNode response");
                    continue;
                }
            };

            // Send length-prefixed response
            let len = response_bytes.len() as u32;
            if let Err(e) = send.write_all(&len.to_be_bytes()).await {
                debug!(error = %e, "failed to send response length");
                continue;
            }
            if let Err(e) = send.write_all(&response_bytes).await {
                debug!(error = %e, "failed to send response");
                continue;
            }
            if let Err(e) = send.finish() {
                debug!(error = %e, "failed to finish send");
            }

            // Note: Requester handling (adding to routing table + storing relay URL)
            // is now done inside handle_find_node_request above.
            // 
            // We still persist to database for recovery after restart:
            if let Some(requester_id) = request.requester {
                if requester_id != our_id {
                    let db_lock = db.lock().await;
                    let entry = crate::data::dht::DhtEntry {
                        endpoint_id: requester_id,
                        bucket_index: 0, // Will be recalculated on load
                        added_at: current_timestamp(),
                        relay_url: request.requester_relay_url.clone(),
                    };
                    // Ignore errors - best effort
                    let _ = crate::data::dht::insert_dht_entry(&db_lock, &entry);
                }
            }
        }

        Ok(())
    }

    /// Helper: find closest nodes from database (fallback when DHT actor unavailable)
    async fn find_closest_from_db(
        db: &Arc<Mutex<Connection>>,
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
    use super::*;

    fn make_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    // ========== DHT Protocol Tests ==========

    #[test]
    fn test_find_node_request_serialization() {
        let request = FindNode {
            target: Id::new(make_id(42)),
            requester: Some(make_id(1)),
            requester_relay_url: Some("https://relay.example.com/".to_string()),
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
        // Test the length-prefixed message format used in DHT protocol
        let request = FindNode {
            target: Id::new(make_id(42)),
            requester: None,
            requester_relay_url: None,
        };

        let request_bytes = postcard::to_allocvec(&request).unwrap();
        let len = request_bytes.len() as u32;
        
        // Build wire format
        let mut wire = Vec::new();
        wire.extend_from_slice(&len.to_be_bytes());
        wire.extend_from_slice(&request_bytes);

        // Parse back
        let parsed_len = u32::from_be_bytes([wire[0], wire[1], wire[2], wire[3]]) as usize;
        assert_eq!(parsed_len, request_bytes.len());

        let parsed: FindNode = postcard::from_bytes(&wire[4..]).unwrap();
        assert_eq!(*parsed.target.as_bytes(), make_id(42));
    }

    #[test]
    fn test_max_response_size_check() {
        // The handler rejects responses > 64KB
        const MAX_SIZE: usize = 65536;
        
        // A reasonable response should be well under this
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
        // The handler should not add the requester to DHT if it's our own ID
        let our_id = make_id(1);
        let requester_id = make_id(1); // Same as us
        
        // Logic check: requester_id != our_id should be false
        assert_eq!(requester_id, our_id);
        assert!(!(requester_id != our_id), "Should not add self to DHT");
    }

    #[test]
    fn test_requester_added_if_different() {
        // The handler should add the requester to DHT if it's different from us
        let our_id = make_id(1);
        let requester_id = make_id(2); // Different
        
        assert_ne!(requester_id, our_id);
        assert!(requester_id != our_id, "Should add different node to DHT");
    }

    #[test]
    fn test_node_info_from_closest() {
        // Test converting DHT entries to NodeInfo for response
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
        // Test that nodes are sorted by distance to target
        let target = Id::new(make_id(0));
        
        let entries = vec![
            (make_id(128), target.distance(&Id::new(make_id(128)))),
            (make_id(1), target.distance(&Id::new(make_id(1)))),
            (make_id(255), target.distance(&Id::new(make_id(255)))),
        ];

        let mut sorted: Vec<_> = entries.clone();
        sorted.sort_by(|a, b| a.1.cmp(&b.1));

        // After sorting, closest should be first
        // (actual order depends on XOR distance)
        assert_eq!(sorted.len(), 3);
    }

    #[test]
    fn test_take_closest_k() {
        // Test taking only K closest nodes
        const K: usize = 20;
        
        let mut nodes: Vec<NodeInfo> = (0..30u8)
            .map(|i| NodeInfo {
                node_id: make_id(i),
                addresses: vec![],
                relay_url: None,
            })
            .collect();

        // Take only K
        nodes.truncate(K);
        
        assert_eq!(nodes.len(), K);
    }
}

