//! DHT RPC protocol using irpc
//!
//! Defines the low-level RPC operations between DHT nodes:
//! - FindNode: Query routing table for closest nodes to a target
//! - Ping: Basic liveness check (implemented as FindNode with random ID)

use std::sync::Arc;

use iroh::NodeAddr;
use irpc::channel::oneshot;
use irpc::rpc_requests;
use serde::{Deserialize, Serialize};

use super::distance::Id;

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

    /// Create NodeInfo from a NodeAddr
    pub fn from_node_addr(addr: &NodeAddr) -> Self {
        Self {
            node_id: *addr.node_id.as_bytes(),
            addresses: addr.direct_addresses.iter().map(|a| a.to_string()).collect(),
            relay_url: addr.relay_url.as_ref().map(|u| u.to_string()),
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

/// RPC client for DHT operations
#[derive(Debug, Clone)]
pub struct RpcClient {
    inner: Arc<irpc::Client<RpcProtocol>>,
}

impl RpcClient {
    /// Create a new RPC client from an irpc client
    pub fn new(client: irpc::Client<RpcProtocol>) -> Self {
        Self {
            inner: Arc::new(client),
        }
    }

    /// Find the closest nodes to a target ID
    pub async fn find_node(
        &self,
        target: Id,
        requester: Option<[u8; 32]>,
        requester_relay_url: Option<String>,
    ) -> Result<FindNodeResponse, irpc::Error> {
        self.inner.rpc(FindNode { target, requester, requester_relay_url }).await
    }
}

/// Weak reference to RPC client
#[derive(Debug, Clone)]
pub struct WeakRpcClient {
    inner: std::sync::Weak<irpc::Client<RpcProtocol>>,
}

impl WeakRpcClient {
    /// Try to upgrade to a strong reference
    pub fn upgrade(&self) -> Option<RpcClient> {
        self.inner.upgrade().map(|inner| RpcClient { inner })
    }
}

impl RpcClient {
    /// Create a weak reference
    pub fn downgrade(&self) -> WeakRpcClient {
        WeakRpcClient {
            inner: Arc::downgrade(&self.inner),
        }
    }
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
}

