//! DHT Bootstrap Nodes
//!
//! Provides well-known bootstrap nodes for joining the DHT network.
//! New nodes connect to these to discover other peers.

use iroh::EndpointId;

/// Bootstrap node information
#[derive(Debug, Clone)]
pub struct BootstrapNode {
    /// The node's EndpointID (EndpointId)
    pub node_id: EndpointId,
    /// Optional human-readable name
    pub name: Option<&'static str>,
    /// Optional relay URL for cross-network connectivity
    pub relay_url: Option<&'static str>,
}

impl BootstrapNode {
    /// Parse hex string to 32-byte array
    fn parse_hex(hex: &str) -> Option<[u8; 32]> {
        let bytes = hex::decode(hex).ok()?;
        if bytes.len() != 32 {
            return None;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Some(arr)
    }

    /// Create a new bootstrap node from hex-encoded EndpointId
    pub fn from_hex(hex: &str, name: Option<&'static str>) -> Option<Self> {
        let arr = Self::parse_hex(hex)?;
        let node_id = EndpointId::from_bytes(&arr).ok()?;
        Some(Self { node_id, name, relay_url: None })
    }

    /// Create a new bootstrap node with relay URL
    pub fn from_hex_with_relay(hex: &str, name: Option<&'static str>, relay_url: &'static str) -> Option<Self> {
        let arr = Self::parse_hex(hex)?;
        let node_id = EndpointId::from_bytes(&arr).ok()?;
        Some(Self { node_id, name, relay_url: Some(relay_url) })
    }

    /// Create from raw bytes
    pub fn from_bytes(bytes: &[u8; 32], name: Option<&'static str>) -> Option<Self> {
        let node_id = EndpointId::from_bytes(bytes).ok()?;
        Some(Self { node_id, name, relay_url: None })
    }

    /// Create from raw bytes with relay URL
    pub fn from_bytes_with_relay(bytes: &[u8; 32], name: Option<&'static str>, relay_url: &'static str) -> Option<Self> {
        let node_id = EndpointId::from_bytes(bytes).ok()?;
        Some(Self { node_id, name, relay_url: Some(relay_url) })
    }
}

/// Default bootstrap nodes for the Harbor network
/// 
/// These are maintained by the Harbor project and should be
/// reasonably stable and well-connected.
///
/// UPDATE THESE VALUES after running the test app and getting the endpoint ID and relay URL.
pub fn default_bootstrap_nodes() -> Vec<BootstrapNode> {
    vec![
        // Primary bootstrap node
        BootstrapNode::from_hex_with_relay(
            "1f5c436e4511ab2db15db837b827f237931bc722ede6f223c7f5d41b412c0cad",
            Some("harbor-bootstrap-1"),
            "https://euc1-1.relay.n0.iroh-canary.iroh.link./",
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Get bootstrap node IDs as raw bytes
pub fn bootstrap_node_ids() -> Vec<[u8; 32]> {
    default_bootstrap_nodes()
        .into_iter()
        .map(|n| *n.node_id.as_bytes())
        .collect()
}

/// Get bootstrap nodes with their relay URLs (for DialInfo registration)
pub fn bootstrap_dial_info() -> Vec<(EndpointId, Option<String>)> {
    default_bootstrap_nodes()
        .into_iter()
        .map(|n| (n.node_id, n.relay_url.map(|s| s.to_string())))
        .collect()
}

/// Bootstrap configuration
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    /// List of bootstrap nodes
    pub nodes: Vec<BootstrapNode>,
    /// Whether to use default bootstrap nodes
    pub use_defaults: bool,
    /// Additional custom bootstrap nodes
    pub custom_nodes: Vec<BootstrapNode>,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            nodes: default_bootstrap_nodes(),
            use_defaults: true,
            custom_nodes: Vec::new(),
        }
    }
}

impl BootstrapConfig {
    /// Create an empty config (no bootstrap nodes)
    pub fn empty() -> Self {
        Self {
            nodes: Vec::new(),
            use_defaults: false,
            custom_nodes: Vec::new(),
        }
    }

    /// Add a custom bootstrap node
    pub fn add_node(&mut self, node: BootstrapNode) {
        self.custom_nodes.push(node);
    }

    /// Add a bootstrap node from hex string
    pub fn add_from_hex(&mut self, hex: &str, name: Option<&'static str>) -> bool {
        if let Some(node) = BootstrapNode::from_hex(hex, name) {
            self.custom_nodes.push(node);
            true
        } else {
            false
        }
    }

    /// Get all bootstrap nodes (defaults + custom)
    pub fn all_nodes(&self) -> Vec<BootstrapNode> {
        let mut nodes = Vec::new();
        if self.use_defaults {
            nodes.extend(self.nodes.clone());
        }
        nodes.extend(self.custom_nodes.clone());
        nodes
    }

    /// Get all bootstrap node IDs as bytes
    pub fn all_node_ids(&self) -> Vec<[u8; 32]> {
        self.all_nodes()
            .into_iter()
            .map(|n| *n.node_id.as_bytes())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bootstrap_node_from_hex() {
        let hex = "d2248c8b350e861198eea13f5bd962257db9bca49a5fb063f9d2655e7b2f6f24";
        let node = BootstrapNode::from_hex(hex, Some("test"));
        assert!(node.is_some());
        
        let node = node.unwrap();
        assert_eq!(node.name, Some("test"));
    }

    #[test]
    fn test_bootstrap_node_from_invalid_hex() {
        let node = BootstrapNode::from_hex("invalid", None);
        assert!(node.is_none());

        let node = BootstrapNode::from_hex("aabbcc", None); // Too short
        assert!(node.is_none());
    }

    #[test]
    fn test_default_bootstrap_nodes() {
        let nodes = default_bootstrap_nodes();
        assert!(!nodes.is_empty());
        
        // First node should be our primary
        assert_eq!(nodes[0].name, Some("harbor-bootstrap-1"));
    }

    #[test]
    fn test_bootstrap_config_default() {
        let config = BootstrapConfig::default();
        assert!(config.use_defaults);
        assert!(!config.nodes.is_empty());
    }

    #[test]
    fn test_bootstrap_config_empty() {
        let config = BootstrapConfig::empty();
        assert!(!config.use_defaults);
        assert!(config.all_nodes().is_empty());
    }

    #[test]
    fn test_bootstrap_config_add_node() {
        let mut config = BootstrapConfig::empty();
        let added = config.add_from_hex(
            "d2248c8b350e861198eea13f5bd962257db9bca49a5fb063f9d2655e7b2f6f24",
            Some("custom"),
        );
        assert!(added);
        assert_eq!(config.all_nodes().len(), 1);
    }

    #[test]
    fn test_bootstrap_node_ids() {
        let ids = bootstrap_node_ids();
        assert!(!ids.is_empty());
        assert_eq!(ids[0].len(), 32);
    }

    #[test]
    fn test_bootstrap_node_from_bytes() {
        // Use a valid Ed25519 public key (derived from a known secret)
        let secret = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let bytes = *secret.public().as_bytes();
        
        let node = BootstrapNode::from_bytes(&bytes, Some("test-bytes"));
        assert!(node.is_some());
        
        let node = node.unwrap();
        assert_eq!(node.name, Some("test-bytes"));
        assert!(node.relay_url.is_none());
    }

    #[test]
    fn test_bootstrap_node_from_bytes_with_relay() {
        // Use a valid Ed25519 public key (derived from a known secret)
        let secret = iroh::SecretKey::from_bytes(&[2u8; 32]);
        let bytes = *secret.public().as_bytes();
        
        let node = BootstrapNode::from_bytes_with_relay(&bytes, Some("test"), "https://relay.example.com/");
        assert!(node.is_some());
        
        let node = node.unwrap();
        assert_eq!(node.relay_url, Some("https://relay.example.com/"));
    }

    #[test]
    fn test_bootstrap_dial_info() {
        let dial_info = bootstrap_dial_info();
        assert!(!dial_info.is_empty());
        
        // First node should have a relay URL
        let (node_id, relay) = &dial_info[0];
        assert_eq!(node_id.as_bytes().len(), 32);
        assert!(relay.is_some());
    }

    #[test]
    fn test_bootstrap_config_all_node_ids() {
        let config = BootstrapConfig::default();
        let ids = config.all_node_ids();
        assert!(!ids.is_empty());
        
        // All IDs should be 32 bytes
        for id in ids {
            assert_eq!(id.len(), 32);
        }
    }
}

