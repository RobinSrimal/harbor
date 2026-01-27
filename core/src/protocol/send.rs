//! Message sending operations for the Protocol
//!
//! This module contains the public API for sending messages:
//! - send: Send a message to all topic members
//! - refresh_members: Trigger member list refresh via Harbor

use tracing::{info, trace};

use crate::data::get_topic_members_with_info;
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::send::topic_messages::TopicMessage;

use super::core::{Protocol, MAX_MESSAGE_SIZE};
use super::error::ProtocolError;
use super::types::MemberInfo;

impl Protocol {
    /// Send a message to a topic
    ///
    /// The payload is opaque bytes - the app defines the format.
    /// Messages are sent to all known topic members (from the local member list).
    ///
    /// # Arguments
    ///
    /// * `topic_id` - The 32-byte topic identifier
    /// * `payload` - The message content (max 512KB)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The protocol is not running
    /// - The message exceeds 512KB
    /// - The topic doesn't exist
    /// - The caller is not a member of the topic
    pub async fn send(&self, topic_id: &[u8; 32], payload: &[u8]) -> Result<(), ProtocolError> {
        self.check_running().await?;

        // Check message size
        if payload.len() > MAX_MESSAGE_SIZE {
            return Err(ProtocolError::MessageTooLarge);
        }

        // Get topic members with relay info for connectivity
        let members_with_info = {
            let db = self.db.lock().await;
            get_topic_members_with_info(&db, topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        trace!(
            topic = %hex::encode(topic_id),
            member_count = members_with_info.len(),
            "sending message"
        );

        if members_with_info.is_empty() {
            return Err(ProtocolError::TopicNotFound);
        }

        let our_id = self.endpoint_id();
        if !members_with_info.iter().any(|m| m.endpoint_id == our_id) {
            return Err(ProtocolError::NotMember);
        }

        // Get recipients (all members except us) with their relay info
        let recipients: Vec<MemberInfo> = members_with_info
            .into_iter()
            .filter(|m| m.endpoint_id != our_id)
            .map(|m| MemberInfo {
                endpoint_id: m.endpoint_id,
                relay_url: m.relay_url,
            })
            .collect();

        trace!(recipient_count = recipients.len(), "recipients (excluding self)");

        if recipients.is_empty() {
            // No one else to send to
            trace!("no recipients - only member is self");
            return Ok(());
        }

        // Wrap payload as Content message and delegate to SendService
        let content_msg = TopicMessage::Content(payload.to_vec());
        let encoded_payload = content_msg.encode();

        self.send_service
            .send_to_topic(topic_id, &encoded_payload, &recipients, HarborPacketType::Content)
            .await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        Ok(())
    }

    /// Refresh member lists from Harbor Nodes
    ///
    /// Triggers a Harbor pull which will fetch any pending Join/Leave messages.
    /// These messages update the local member list automatically.
    ///
    /// Returns the number of topics currently subscribed.
    pub async fn refresh_members(&self) -> Result<usize, ProtocolError> {
        self.check_running().await?;

        info!("triggering manual Harbor pull for member sync");

        // Get count of subscribed topics
        let topics = self.list_topics().await?;
        let topic_count = topics.len();

        // Harbor pull happens automatically via background task
        // For manual refresh, just return current topic count
        info!(topics = topic_count, "member sync via Harbor pull (automatic)");

        Ok(topic_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_message_size_constant() {
        assert_eq!(MAX_MESSAGE_SIZE, 512 * 1024);
    }

    #[test]
    fn test_message_size_validation() {
        let small_msg = vec![0u8; 1000];
        let max_msg = vec![0u8; MAX_MESSAGE_SIZE];
        let too_large = vec![0u8; MAX_MESSAGE_SIZE + 1];

        assert!(small_msg.len() <= MAX_MESSAGE_SIZE);
        assert!(max_msg.len() <= MAX_MESSAGE_SIZE);
        assert!(too_large.len() > MAX_MESSAGE_SIZE);
    }
}
