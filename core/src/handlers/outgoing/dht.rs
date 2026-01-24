//! DHT outgoing operations
//!
//! This module provides DHT network operations:
//! - send_find_node: Send FindNode RPC to a peer
//!
//! The actor manages state (routing table) and lookup algorithm,
//! while this module handles the actual network I/O.

use iroh::endpoint::Connection;

use crate::network::dht::Id;
use crate::network::dht::protocol::{FindNode, FindNodeResponse};
use crate::network::pool::PoolError;

/// Send a FindNode request to a connected peer
///
/// This is the low-level network call used by the DHT actor during lookups.
/// The actor manages connection pooling and routing table state; this function
/// just handles the wire protocol.
///
/// # Arguments
/// * `conn` - An established QUIC connection to the peer
/// * `target` - The ID we're looking for closest nodes to
/// * `requester` - Our node ID (so peer can add us to their routing table)
/// * `requester_relay_url` - Our relay URL for connectivity
///
/// # Returns
/// The peer's response containing their closest known nodes to the target
pub async fn send_find_node(
    conn: &Connection,
    target: Id,
    requester: Option<[u8; 32]>,
    requester_relay_url: Option<String>,
) -> Result<FindNodeResponse, PoolError> {
    // Open bidirectional stream
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to open stream: {}", e)))?;

    // Create and serialize request
    let request = FindNode { target, requester, requester_relay_url };
    let request_bytes = postcard::to_allocvec(&request)
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to serialize request: {}", e)))?;

    // Send length-prefixed request
    let len = request_bytes.len() as u32;
    send.write_all(&len.to_be_bytes())
        .await
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to send length: {}", e)))?;
    send.write_all(&request_bytes)
        .await
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to send request: {}", e)))?;
    send.finish()
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to finish send: {}", e)))?;

    // Read length-prefixed response
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to read response length: {}", e)))?;
    let response_len = u32::from_be_bytes(len_buf) as usize;

    // Sanity check response size (max 64KB should be plenty for node list)
    if response_len > 65536 {
        return Err(PoolError::ConnectionFailed("response too large".to_string()));
    }

    let mut response_bytes = vec![0u8; response_len];
    recv.read_exact(&mut response_bytes)
        .await
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to read response: {}", e)))?;

    // Deserialize response
    let response: FindNodeResponse = postcard::from_bytes(&response_bytes)
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to deserialize response: {}", e)))?;

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::dht::protocol::NodeInfo;

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
    fn test_length_prefix_format() {
        // Test the length-prefixed message format used in send_find_node
        let request = FindNode {
            target: make_id(42),
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
        assert_eq!(*parsed.target.as_bytes(), [42u8; 32]);
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
