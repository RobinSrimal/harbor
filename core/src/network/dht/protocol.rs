//! DHT protocol wire format
//!
//! Defines the wire protocol for DHT communication:
//! - FindNode: Query routing table for closest nodes to a target
//! - FindNodeResponse: Response with closest known nodes
//! - NodeInfo: Information about a discovered node

use iroh::EndpointAddr;
use irpc::channel::oneshot;
use irpc::rpc_requests;
use serde::{Deserialize, Serialize};

use super::internal::distance::Id;

/// DHT RPC protocol version in ALPN
pub const RPC_ALPN: &[u8] = b"harbor/dht/0";

/// Maximum number of nodes to return in FindNode response
pub const MAX_FIND_NODE_RESULTS: usize = 20;

/// Request to find closest nodes to a target ID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindNode {
    /// The target ID to find closest nodes for
    pub target: Id,
    /// Optional requester node ID (if they want to be added to routing table)
    pub requester: Option<[u8; 32]>,
    /// Optional requester relay URL (for connectivity)
    #[serde(default)]
    pub requester_relay_url: Option<String>,
}

/// Response containing closest nodes
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FindNodeResponse {
    /// Closest nodes to the target, with optional address info
    pub nodes: Vec<NodeInfo>,
}

/// Information about a node returned in FindNode response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    /// The node's ID (32-byte public key)
    pub node_id: [u8; 32],
    /// Optional direct addresses for the node
    pub addresses: Vec<String>,
    /// Optional relay URL
    pub relay_url: Option<String>,
}

impl NodeInfo {
    /// Create NodeInfo from just a node ID
    pub fn from_id(id: &Id) -> Self {
        Self {
            node_id: *id.as_bytes(),
            addresses: Vec::new(),
            relay_url: None,
        }
    }

    /// Create NodeInfo from a node ID with optional relay URL
    pub fn from_id_with_relay(id: &Id, relay_url: Option<String>) -> Self {
        Self {
            node_id: *id.as_bytes(),
            addresses: Vec::new(),
            relay_url,
        }
    }

    /// Create NodeInfo from a EndpointAddr
    pub fn from_endpoint_addr(addr: &EndpointAddr) -> Self {
        Self {
            node_id: *addr.id.as_bytes(),
            addresses: addr.ip_addrs().map(|a| a.to_string()).collect(),
            relay_url: addr.relay_urls().next().map(|u| u.to_string()),
        }
    }

    /// Convert to Id
    pub fn to_id(&self) -> Id {
        Id::new(self.node_id)
    }
}

/// DHT RPC protocol definition using irpc
#[rpc_requests(message = RpcMessage)]
#[derive(Debug, Serialize, Deserialize)]
pub enum RpcProtocol {
    /// Find the closest nodes to a target ID
    #[rpc(tx = oneshot::Sender<FindNodeResponse>)]
    FindNode(FindNode),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id(seed: u8) -> Id {
        Id::new([seed; 32])
    }

    #[test]
    fn test_node_info_from_id() {
        let id = make_id(42);
        let info = NodeInfo::from_id(&id);
        
        assert_eq!(info.node_id, [42u8; 32]);
        assert!(info.addresses.is_empty());
        assert!(info.relay_url.is_none());
    }

    #[test]
    fn test_node_info_to_id() {
        let info = NodeInfo {
            node_id: [1u8; 32],
            addresses: vec!["192.168.1.1:4433".to_string()],
            relay_url: None,
        };
        
        let id = info.to_id();
        assert_eq!(id.as_bytes(), &[1u8; 32]);
    }

    #[test]
    fn test_find_node_serialization() {
        let request = FindNode {
            target: make_id(1),
            requester: Some([2u8; 32]),
            requester_relay_url: Some("https://relay.example.com/".to_string()),
        };
        
        // Test that it can be serialized/deserialized
        let bytes = postcard::to_allocvec(&request).unwrap();
        let decoded: FindNode = postcard::from_bytes(&bytes).unwrap();
        
        assert_eq!(decoded.target, request.target);
        assert_eq!(decoded.requester, request.requester);
        assert_eq!(decoded.requester_relay_url, request.requester_relay_url);
    }

    #[test]
    fn test_find_node_response_serialization() {
        let response = FindNodeResponse {
            nodes: vec![
                NodeInfo::from_id(&make_id(1)),
                NodeInfo::from_id(&make_id(2)),
            ],
        };
        
        let bytes = postcard::to_allocvec(&response).unwrap();
        let decoded: FindNodeResponse = postcard::from_bytes(&bytes).unwrap();
        
        assert_eq!(decoded.nodes.len(), 2);
        assert_eq!(decoded.nodes[0].node_id, [1u8; 32]);
        assert_eq!(decoded.nodes[1].node_id, [2u8; 32]);
    }

    #[test]
    fn test_id_serialization() {
        let id = make_id(42);
        
        let bytes = postcard::to_allocvec(&id).unwrap();
        let decoded: Id = postcard::from_bytes(&bytes).unwrap();
        
        assert_eq!(decoded, id);
    }

    #[test]
    fn test_find_node_response_default() {
        let response = FindNodeResponse::default();
        assert!(response.nodes.is_empty());
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
    fn test_max_response_size_check() {
        const MAX_SIZE: usize = 65536;

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
        use crate::network::pool::PoolError;
        let err = PoolError::ConnectionFailed("test error".to_string());
        let msg = format!("{}", err);
        assert!(msg.contains("test error"));
    }
}

