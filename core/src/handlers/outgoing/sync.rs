//! Sync operations for the Protocol
//!
//! This module contains methods for sync responses:
//! - respond_sync: Send CRDT snapshot to requester via SYNC_ALPN

use std::time::Duration;

use iroh::{NodeAddr, NodeId};
use tracing::info;

use crate::network::sync::{encode_sync_response, SYNC_ALPN};
use crate::protocol::{Protocol, ProtocolError};

/// Default timeout for sync connections
const SYNC_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

impl Protocol {
    /// Respond to a sync request with current state
    ///
    /// Call this when you receive `ProtocolEvent::SyncRequest`.
    /// Uses direct SYNC_ALPN connection - no size limit.
    pub async fn respond_sync(
        &self,
        topic_id: &[u8; 32],
        requester_id: &[u8; 32],
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        self.check_running().await?;

        // Build SYNC_ALPN message using service layer
        let message = encode_sync_response(topic_id, &data);

        // Connect to requester via SYNC_ALPN
        let node_id = NodeId::from_bytes(requester_id)
            .map_err(|e| ProtocolError::Network(format!("invalid node id: {}", e)))?;

        let conn = self
            .connect_for_sync(node_id)
            .await?;

        // Open unidirectional stream and send
        let mut send = conn
            .open_uni()
            .await
            .map_err(|e| ProtocolError::Network(format!("failed to open stream: {}", e)))?;

        tokio::io::AsyncWriteExt::write_all(&mut send, &message)
            .await
            .map_err(|e| ProtocolError::Network(format!("failed to write: {}", e)))?;

        // Finish the stream - this signals we're done sending
        send.finish()
            .map_err(|e| ProtocolError::Network(format!("failed to finish: {}", e)))?;

        // Wait for the stream to actually be transmitted before dropping connection
        // stopped() completes when the peer has received and acknowledged the data
        send.stopped()
            .await
            .map_err(|e| ProtocolError::Network(format!("stream error: {}", e)))?;

        info!(
            topic = %hex::encode(&topic_id[..8]),
            requester = %hex::encode(&requester_id[..8]),
            size = data.len(),
            "Sent sync response via SYNC_ALPN"
        );

        Ok(())
    }

    /// Connect to a peer for sync by NodeId
    async fn connect_for_sync(
        &self,
        node_id: NodeId,
    ) -> Result<iroh::endpoint::Connection, ProtocolError> {
        let node_addr: NodeAddr = node_id.into();

        tokio::time::timeout(SYNC_CONNECT_TIMEOUT, self.endpoint.connect(node_addr, SYNC_ALPN))
            .await
            .map_err(|_| ProtocolError::Network("sync connect timeout".to_string()))?
            .map_err(|e| ProtocolError::Network(format!("connection failed: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_node_id_from_bytes() {
        let id = make_id(42);
        let result = NodeId::from_bytes(&id);
        assert!(result.is_ok());
    }

    #[test]
    fn test_sync_connect_timeout_value() {
        assert_eq!(SYNC_CONNECT_TIMEOUT, Duration::from_secs(30));
    }
}
