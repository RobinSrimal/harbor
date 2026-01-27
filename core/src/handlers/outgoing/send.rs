//! Send operations for the Protocol
//!
//! This module contains methods for sending messages:
//! - send_raw: Send to members using MemberInfo (includes relay URLs)
//! - send_to_member: Low-level send to a member with relay info

use futures::future::join_all;
use tracing::{debug, trace, info};

use crate::data::WILDCARD_RECIPIENT;
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::send::outgoing::prepare_packet;

use crate::protocol::{Protocol, MemberInfo, ProtocolError};

impl Protocol {
    /// Internal: Send raw payload to members using their connection info (with relay URLs)
    ///
    /// Delegates packet creation and storage to the service layer,
    /// then handles parallel transport to all members.
    pub(crate) async fn send_raw(
        &self,
        topic_id: &[u8; 32],
        payload: &[u8],
        members: &[MemberInfo],
        packet_type: HarborPacketType,
    ) -> Result<(), ProtocolError> {
        // Get recipient IDs for storage
        let recipient_ids: Vec<[u8; 32]> = members.iter().map(|m| m.endpoint_id).collect();

        // Delegate packet creation and storage to service layer
        let (packet_id, encoded) = {
            let mut db = self.db.lock().await;
            prepare_packet(
                topic_id,
                payload,
                &recipient_ids,
                packet_type,
                &self.identity.private_key,
                &self.identity.public_key,
                &mut db,
            )
            .map_err(|e| ProtocolError::Network(e.to_string()))?
        };

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

        // Send to ALL recipients in parallel (transport layer)
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

        if delivered == 0 && failed > 0 {
            return Err(ProtocolError::Network("all sends failed".to_string()));
        }

        Ok(())
    }

    /// Send data to a member using their connection info (includes relay URL if available)
    ///
    /// Delegates to SendService for connection management and transport.
    pub(crate) async fn send_to_member(
        &self,
        member: &MemberInfo,
        data: &[u8],
    ) -> Result<(), ProtocolError> {
        // Delegate to SendService which manages its own connection cache
        self.send_service
            .send_to_member(member, data)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))
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
        let result = iroh::EndpointId::from_bytes(&id);
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
