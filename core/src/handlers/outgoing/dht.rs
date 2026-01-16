//! DHT outgoing operations
//!
//! This module provides DHT-related utilities.
//! The actual DHT lookups are done via DhtApiClient which is called directly
//! by tasks (self_lookup, random_lookup) or via find_harbor_nodes in harbor.rs.

// Note: Protocol methods for DHT are in handlers/outgoing/harbor.rs (find_harbor_nodes)
// and tasks/mod.rs (run_dht_refresh_loop which calls self_lookup/random_lookup directly)

#[cfg(test)]
mod tests {
    use crate::network::dht::Id as DhtId;
    use crate::protocol::ProtocolError;

    #[test]
    fn test_dht_id_creation() {
        let target = [42u8; 32];
        let id = DhtId::new(target);
        assert_eq!(*id.as_bytes(), target);
    }

    #[test]
    fn test_dht_id_different_values() {
        let id1 = DhtId::new([1u8; 32]);
        let id2 = DhtId::new([2u8; 32]);
        assert_ne!(id1.as_bytes(), id2.as_bytes());
    }

    #[test]
    fn test_protocol_error_network_formatting() {
        let err = ProtocolError::Network("test error".to_string());
        let msg = format!("{}", err);
        assert!(msg.contains("test error"));
    }

    #[test]
    fn test_protocol_error_dht_unavailable() {
        let err = ProtocolError::Network("DHT client not available".to_string());
        let msg = format!("{}", err);
        assert!(msg.contains("DHT client not available"));
    }
}
