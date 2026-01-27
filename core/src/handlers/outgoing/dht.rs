//! DHT outgoing operations - serialization tests
//!
//! The actual send_find_node logic has moved to DhtService.
//! These tests verify the wire protocol serialization format.

#[cfg(test)]
mod tests {
    use crate::network::dht::Id;
    use crate::network::dht::protocol::{FindNode, FindNodeResponse, NodeInfo};
    use crate::network::pool::PoolError;

    fn make_id(seed: u8) -> Id {
        Id::new([seed; 32])
    }

    #[test]
    fn test_find_node_request_serialization() {
        let request = FindNode {
            target: make_id(42),
            requester: Some([1u8; 32]),
            requester_relay_url: Some("https://relay.example.com/".to_string()),
        };

        let bytes = postcard::to_allocvec(&request).unwrap();
        let decoded: FindNode = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(*decoded.target.as_bytes(), [42u8; 32]);
        assert_eq!(decoded.requester, Some([1u8; 32]));
        assert_eq!(decoded.requester_relay_url, request.requester_relay_url);
    }

    #[test]
    fn test_find_node_request_no_requester() {
        let request = FindNode {
            target: make_id(42),
            requester: None,
            requester_relay_url: None,
        };

        let bytes = postcard::to_allocvec(&request).unwrap();
        let decoded: FindNode = postcard::from_bytes(&bytes).unwrap();

        assert!(decoded.requester.is_none());
        assert!(decoded.requester_relay_url.is_none());
    }

    #[test]
    fn test_find_node_response_serialization() {
        let response = FindNodeResponse {
            nodes: vec![
                NodeInfo {
                    node_id: [1u8; 32],
                    addresses: vec!["192.168.1.1:4433".to_string()],
                    relay_url: Some("https://relay.example.com/".to_string()),
                },
                NodeInfo {
                    node_id: [2u8; 32],
                    addresses: vec![],
                    relay_url: None,
                },
            ],
        };

        let bytes = postcard::to_allocvec(&response).unwrap();
        let decoded: FindNodeResponse = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.nodes.len(), 2);
        assert_eq!(decoded.nodes[0].node_id, [1u8; 32]);
        assert_eq!(decoded.nodes[1].node_id, [2u8; 32]);
    }

    #[test]
    fn test_find_node_response_empty() {
        let response = FindNodeResponse { nodes: vec![] };

        let bytes = postcard::to_allocvec(&response).unwrap();
        let decoded: FindNodeResponse = postcard::from_bytes(&bytes).unwrap();

        assert!(decoded.nodes.is_empty());
    }

    #[test]
    fn test_max_response_size_check() {
        // The handler rejects responses > 64KB
        const MAX_SIZE: usize = 65536;

        // A reasonable response should be well under this
        let nodes: Vec<NodeInfo> = (0..20u8)
            .map(|i| NodeInfo {
                node_id: [i; 32],
                addresses: vec!["192.168.1.1:4433".to_string()],
                relay_url: Some("https://relay.example.com/".to_string()),
            })
            .collect();

        let response = FindNodeResponse { nodes };
        let bytes = postcard::to_allocvec(&response).unwrap();

        assert!(bytes.len() < MAX_SIZE, "Response {} bytes exceeds limit", bytes.len());
    }

    #[test]
    fn test_pool_error_formatting() {
        let err = PoolError::ConnectionFailed("test error".to_string());
        let msg = format!("{}", err);
        assert!(msg.contains("test error"));
    }
}
