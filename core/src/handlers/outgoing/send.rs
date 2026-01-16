//! Send operations for the Protocol
//!
//! This module contains methods for sending messages:
//! - send_raw: Send to recipients by endpoint ID
//! - send_to_node: Low-level send to a single node
//! - send_raw_with_info: Send using MemberInfo (includes relay URLs)
//! - send_to_member: Low-level send to a member with relay info

use std::time::Duration;

use futures::future::join_all;
use iroh::{NodeId, NodeAddr};
use tracing::{debug, trace, info};

use crate::data::outgoing::store_outgoing_packet;
use crate::data::WILDCARD_RECIPIENT;
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::send::protocol::{SEND_ALPN, SendMessage};
use crate::security::{create_packet, harbor_id_from_topic};

use crate::protocol::{Protocol, MemberInfo, ProtocolError};

impl Protocol {
    /// Internal: Send raw payload to specific recipients (by endpoint ID only)
    pub(crate) async fn send_raw(
        &self,
        topic_id: &[u8; 32],
        payload: &[u8],
        recipients: &[[u8; 32]],
        packet_type: HarborPacketType,
    ) -> Result<(), ProtocolError> {
        // Create packet
        let packet = create_packet(
            topic_id,
            &self.identity.private_key,
            &self.identity.public_key,
            payload,
        ).map_err(|e| ProtocolError::Network(e.to_string()))?;

        let packet_id = packet.packet_id;
        let harbor_id = harbor_id_from_topic(topic_id);
        let packet_bytes = packet.to_bytes()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Store in outgoing table for tracking
        {
            let mut db = self.db.lock().await;
            store_outgoing_packet(
                &mut db,
                &packet_id,
                topic_id,
                &harbor_id,
                &packet_bytes,
                recipients,
                packet_type as u8,
            ).map_err(|e| ProtocolError::Database(e.to_string()))?;
        }

        // Encode for wire
        let message = SendMessage::Packet(packet);
        let encoded = message.encode();

        if recipients.is_empty() {
            debug!(packet_id = hex::encode(packet_id), "no recipients");
            return Ok(());
        }

        info!(
            packet_id = hex::encode(&packet_id[..8]),
            recipient_count = recipients.len(),
            "sending to recipients in parallel"
        );

        // Send to ALL recipients in parallel
        let send_futures = recipients.iter().map(|recipient| {
            let encoded = encoded.clone();
            let recipient_id = *recipient;
            async move {
                let result = self.send_to_node(&recipient_id, &encoded).await;
                (recipient_id, result)
            }
        });

        let results = join_all(send_futures).await;

        // Count successes/failures
        let mut delivered = 0;
        let mut failed = 0;
        for (recipient_id, result) in results {
            match result {
                Ok(()) => {
                    delivered += 1;
                    trace!(recipient = hex::encode(recipient_id), "packet sent");
                }
                Err(e) => {
                    failed += 1;
                    debug!(
                        recipient = hex::encode(recipient_id),
                        error = %e,
                        "send failed"
                    );
                }
            }
        }

        info!(
            packet_id = hex::encode(&packet_id[..8]),
            delivered = delivered,
            failed = failed,
            "parallel send complete"
        );

        // If all sends failed, return error
        if delivered == 0 && failed > 0 {
            return Err(ProtocolError::Network("all sends failed".to_string()));
        }

        // Receipts will be handled by incoming handler
        // Harbor replication will be handled by background task if receipts don't arrive

        Ok(())
    }

    /// Send encoded data to a single node (by endpoint ID only)
    pub(crate) async fn send_to_node(
        &self,
        recipient: &[u8; 32],
        data: &[u8],
    ) -> Result<(), ProtocolError> {
        let node_id = NodeId::from_bytes(recipient)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        let node_addr: NodeAddr = node_id.into();

        // Connect with timeout
        let conn = tokio::time::timeout(
            Duration::from_secs(5),
            self.endpoint.connect(node_addr, SEND_ALPN),
        )
        .await
        .map_err(|_| ProtocolError::Network("connect timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Open stream and send
        let mut send_stream = conn
            .open_uni()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        tokio::io::AsyncWriteExt::write_all(&mut send_stream, data)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Mark stream as finished
        send_stream
            .finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Wait for stream to actually be sent before dropping connection
        send_stream
            .stopped()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        Ok(())
    }

    /// Send raw payload to members using their connection info (with relay URLs)
    pub(crate) async fn send_raw_with_info(
        &self,
        topic_id: &[u8; 32],
        payload: &[u8],
        members: &[MemberInfo],
        packet_type: HarborPacketType,
    ) -> Result<(), ProtocolError> {
        // Create packet
        let packet = create_packet(
            topic_id,
            &self.identity.private_key,
            &self.identity.public_key,
            payload,
        ).map_err(|e| ProtocolError::Network(e.to_string()))?;

        let packet_id = packet.packet_id;
        let harbor_id = harbor_id_from_topic(topic_id);
        let packet_bytes = packet.to_bytes()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Get recipient IDs for storage
        let recipient_ids: Vec<[u8; 32]> = members.iter().map(|m| m.endpoint_id).collect();

        // Store in outgoing table for tracking
        {
            let mut db = self.db.lock().await;
            store_outgoing_packet(
                &mut db,
                &packet_id,
                topic_id,
                &harbor_id,
                &packet_bytes,
                &recipient_ids,
                packet_type as u8,
            ).map_err(|e| ProtocolError::Database(e.to_string()))?;
        }

        // Encode for wire
        let message = SendMessage::Packet(packet);
        let encoded = message.encode();

        // Filter out self and wildcard (wildcard is only for Harbor storage, not actual sending)
        let our_id = self.identity.public_key;
        let recipients: Vec<&MemberInfo> = members
            .iter()
            .filter(|m| m.endpoint_id != our_id && m.endpoint_id != WILDCARD_RECIPIENT)
            .collect();

        if recipients.is_empty() {
            debug!(packet_id = hex::encode(packet_id), "no recipients (only self)");
            return Ok(());
        }

        info!(
            packet_id = hex::encode(&packet_id[..8]),
            recipient_count = recipients.len(),
            "sending to recipients in parallel"
        );

        // Send to ALL recipients in parallel
        let send_futures = recipients.iter().map(|member| {
            let encoded = encoded.clone();
            let endpoint_id = member.endpoint_id;
            let member = (*member).clone();
            async move {
                let result = self.send_to_member(&member, &encoded).await;
                (endpoint_id, result)
            }
        });

        let results = join_all(send_futures).await;

        // Count successes/failures
        let mut delivered = 0;
        let mut failed = 0;
        for (endpoint_id, result) in results {
            match result {
                Ok(()) => {
                    delivered += 1;
                    trace!(recipient = hex::encode(endpoint_id), "packet sent");
                }
                Err(e) => {
                    failed += 1;
                    debug!(
                        recipient = hex::encode(endpoint_id),
                        error = %e,
                        "send failed"
                    );
                }
            }
        }

        info!(
            packet_id = hex::encode(&packet_id[..8]),
            delivered = delivered,
            failed = failed,
            "parallel send complete"
        );

        // If all sends failed, return error
        if delivered == 0 && failed > 0 {
            return Err(ProtocolError::Network("all sends failed".to_string()));
        }

        Ok(())
    }

    /// Send data to a member using their connection info (includes relay URL if available)
    pub(crate) async fn send_to_member(
        &self,
        member: &MemberInfo,
        data: &[u8],
    ) -> Result<(), ProtocolError> {
        let node_id = NodeId::from_bytes(&member.endpoint_id)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Build NodeAddr with relay URL if available
        let node_addr = if let Some(ref relay_url) = member.relay_url {
            // Parse relay URL and create NodeAddr with it
            if let Ok(relay) = relay_url.parse::<iroh::RelayUrl>() {
                NodeAddr::new(node_id).with_relay_url(relay)
            } else {
                // Fall back to just NodeId if URL parsing fails
                NodeAddr::from(node_id)
            }
        } else {
            // No relay URL, just use NodeId (relies on DNS discovery)
            NodeAddr::from(node_id)
        };

        debug!(
            target = %node_id,
            relay = ?member.relay_url,
            "connecting to member"
        );

        // Connect with timeout
        let conn = tokio::time::timeout(
            Duration::from_secs(10), // Longer timeout for relay connections
            self.endpoint.connect(node_addr, SEND_ALPN),
        )
        .await
        .map_err(|_| ProtocolError::Network("connect timeout".to_string()))?
        .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Open stream and send
        let mut send_stream = conn
            .open_uni()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        tokio::io::AsyncWriteExt::write_all(&mut send_stream, data)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Finish the stream - this signals we're done sending
        // In iroh 0.93+, finish() is sync but we need stopped() to wait for transmission
        send_stream
            .finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Wait for the stream to actually be transmitted before dropping connection
        // stopped() completes when the peer has received and acknowledged the data
        send_stream
            .stopped()
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_member_info_creation() {
        let id = make_id(1);
        let info = MemberInfo::new(id);
        assert_eq!(info.endpoint_id, id);
        assert!(info.relay_url.is_none());
    }

    #[test]
    fn test_member_info_with_relay() {
        let id = make_id(1);
        let relay = "https://relay.example.com/".to_string();
        let info = MemberInfo::with_relay(id, relay.clone());
        assert_eq!(info.endpoint_id, id);
        assert_eq!(info.relay_url, Some(relay));
    }

    #[test]
    fn test_recipient_ids_extraction() {
        let members = vec![
            MemberInfo::new(make_id(1)),
            MemberInfo::new(make_id(2)),
            MemberInfo::new(make_id(3)),
        ];
        
        let recipient_ids: Vec<[u8; 32]> = members.iter().map(|m| m.endpoint_id).collect();
        
        assert_eq!(recipient_ids.len(), 3);
        assert_eq!(recipient_ids[0], make_id(1));
        assert_eq!(recipient_ids[1], make_id(2));
        assert_eq!(recipient_ids[2], make_id(3));
    }

    #[test]
    fn test_filter_self_from_recipients() {
        let our_id = make_id(1);
        let members = vec![
            MemberInfo::new(make_id(1)), // self
            MemberInfo::new(make_id(2)),
            MemberInfo::new(make_id(3)),
        ];
        
        let filtered: Vec<_> = members
            .into_iter()
            .filter(|m| m.endpoint_id != our_id)
            .collect();
        
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|m| m.endpoint_id != our_id));
    }

    #[test]
    fn test_node_id_from_bytes() {
        let id = make_id(42);
        let result = iroh::NodeId::from_bytes(&id);
        // This should succeed for any valid 32-byte array
        assert!(result.is_ok());
    }

    #[test]
    fn test_relay_url_parsing() {
        let valid_url = "https://relay.example.com/";
        let parsed: Result<iroh::RelayUrl, _> = valid_url.parse();
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_relay_url_parsing_invalid() {
        let invalid_url = "not-a-url";
        let parsed: Result<iroh::RelayUrl, _> = invalid_url.parse();
        assert!(parsed.is_err());
    }
}

