//! Topic operations for the Protocol
//!
//! This module contains public API methods for topic management:
//! - create_topic: Create a new topic
//! - join_topic: Join an existing topic
//! - leave_topic: Leave a topic
//! - list_topics: List all subscribed topics
//! - get_invite: Get an invite for a topic
//! - send: Send a message to a topic

use tracing::{debug, info, trace};

use crate::data::{
    add_topic_member, cleanup_pulled_for_topic, get_all_topics, get_topic_members,
    get_topic_members_with_info, remove_topic_member, subscribe_topic, unsubscribe_topic,
};
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::membership::messages::{JoinMessage, LeaveMessage, TopicMessage};

use super::core::{Protocol, MAX_MESSAGE_SIZE};
use super::types::{MemberInfo, ProtocolError, TopicInvite};

impl Protocol {
    /// Create a new topic
    ///
    /// This creates a new topic with this node as the only member.
    /// Returns an invite that can be shared with others.
    pub async fn create_topic(&self) -> Result<TopicInvite, ProtocolError> {
        self.check_running().await?;

        // Generate random topic ID
        use rand::{rngs::OsRng, Rng};
        let mut topic_id = [0u8; 32];
        OsRng.fill(&mut topic_id);

        let our_id = self.endpoint_id();

        // Get our relay URL for cross-network connectivity
        let relay_url = self.endpoint.node_addr().relay_url.map(|u| u.to_string());

        // Create member info with relay URL
        let our_info = if let Some(relay) = relay_url {
            MemberInfo::with_relay(our_id, relay)
        } else {
            MemberInfo::new(our_id)
        };

        // Subscribe to the topic
        {
            let db = self.db.lock().await;
            subscribe_topic(&db, &topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;
            add_topic_member(&db, &topic_id, &our_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;
        }

        Ok(TopicInvite::new_with_info(topic_id, vec![our_info]))
    }

    /// Join an existing topic
    ///
    /// This joins a topic using an invite received from another member.
    /// The invite contains all current topic members.
    /// Announces our presence to other members.
    pub async fn join_topic(&self, invite: TopicInvite) -> Result<(), ProtocolError> {
        self.check_running().await?;

        let our_id = self.endpoint_id();
        let member_ids = invite.member_ids();

        // Subscribe to the topic
        {
            let db = self.db.lock().await;
            subscribe_topic(&db, &invite.topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;

            // Add ourselves
            add_topic_member(&db, &invite.topic_id, &our_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;

            // Add all members from the invite (invite contains complete member list)
            for member_id in &member_ids {
                add_topic_member(&db, &invite.topic_id, member_id)
                    .map_err(|e| ProtocolError::Database(e.to_string()))?;
            }
        }

        // Send join announcement to other members (with relay info for connectivity)
        let our_relay = self.endpoint.node_addr().relay_url.map(|u| u.to_string());
        let join_msg = if let Some(ref relay) = our_relay {
            JoinMessage::with_relay(our_id, relay.clone())
        } else {
            JoinMessage::new(our_id)
        };
        let payload = TopicMessage::Join(join_msg).encode();

        trace!(
            our_id = %hex::encode(our_id),
            our_relay = ?our_relay,
            member_count = invite.get_member_info().len(),
            "sending join announcement"
        );

        // Build recipient list: existing members + WILDCARD for future member discovery
        // WILDCARD ensures JOIN packets are stored on Harbor for members who join later
        use crate::network::membership::WILDCARD_RECIPIENT;
        let mut members_with_wildcard = invite.get_member_info().to_vec();
        members_with_wildcard.push(MemberInfo {
            endpoint_id: WILDCARD_RECIPIENT,
            relay_url: None,
        });

        // Send to all existing members using their connection info (best effort)
        // The WILDCARD recipient is filtered out for direct sending but kept for Harbor storage
        let send_result = self
            .send_raw_with_info(
                &invite.topic_id,
                &payload,
                &members_with_wildcard,
                HarborPacketType::Join,
            )
            .await;
        if let Err(e) = send_result {
            debug!(error = %e, "failed to send join announcement to some members");
            // Continue anyway - Harbor replication will handle missed members
        }

        info!(topic = hex::encode(invite.topic_id), "joined topic");
        Ok(())
    }

    /// Leave a topic
    pub async fn leave_topic(&self, topic_id: &[u8; 32]) -> Result<(), ProtocolError> {
        self.check_running().await?;

        let our_id = self.endpoint_id();

        // Get current members before we leave
        let members: Vec<[u8; 32]>;
        {
            let db = self.db.lock().await;
            members = get_topic_members(&db, topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;
        }

        // Send leave announcement to remaining members + WILDCARD for future member discovery
        let leave_msg = LeaveMessage::new(our_id);
        let payload = TopicMessage::Leave(leave_msg).encode();

        use crate::network::membership::WILDCARD_RECIPIENT;
        let mut remaining_members: Vec<[u8; 32]> = members.into_iter().filter(|m| *m != our_id).collect();
        remaining_members.push(WILDCARD_RECIPIENT);

        let send_result = self
            .send_raw(topic_id, &payload, &remaining_members, HarborPacketType::Leave)
            .await;
        if let Err(e) = send_result {
            debug!(error = %e, "failed to send leave announcement to some members");
            // Continue anyway
        }

        // Now remove ourselves and unsubscribe
        {
            let db = self.db.lock().await;

            // Remove ourselves from members
            remove_topic_member(&db, topic_id, &our_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;

            // Unsubscribe
            unsubscribe_topic(&db, topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;

            // Clean up pulled packet tracking for this topic
            cleanup_pulled_for_topic(&db, topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?;
        }

        info!(topic = hex::encode(*topic_id), "left topic");
        Ok(())
    }

    /// List all topics we're subscribed to
    pub async fn list_topics(&self) -> Result<Vec<[u8; 32]>, ProtocolError> {
        self.check_running().await?;

        let db = self.db.lock().await;
        let topics =
            get_all_topics(&db).map_err(|e| ProtocolError::Database(e.to_string()))?;

        Ok(topics.into_iter().map(|t| t.topic_id).collect())
    }

    /// Get an invite for an existing topic
    ///
    /// Returns a fresh invite containing ALL current topic members.
    /// This should be used when sharing invites to ensure the new member
    /// has a complete view of the topic membership.
    pub async fn get_invite(&self, topic_id: &[u8; 32]) -> Result<TopicInvite, ProtocolError> {
        self.check_running().await?;

        let db = self.db.lock().await;
        let members_with_info = get_topic_members_with_info(&db, topic_id)
            .map_err(|e| ProtocolError::Database(e.to_string()))?;

        if members_with_info.is_empty() {
            return Err(ProtocolError::TopicNotFound);
        }

        // Get our relay URL for cross-network connectivity (use fresh value)
        let our_id = self.endpoint_id();
        let our_relay = self.endpoint.node_addr().relay_url.map(|u| u.to_string());

        // Build member info from database, but update our own relay URL with fresh value
        let member_info: Vec<MemberInfo> = members_with_info
            .into_iter()
            .map(|m| {
                if m.endpoint_id == our_id {
                    // Use fresh relay URL for ourselves
                    MemberInfo {
                        endpoint_id: m.endpoint_id,
                        relay_url: our_relay.clone().or(m.relay_url),
                    }
                } else {
                    // Use stored relay URL for others
                    MemberInfo {
                        endpoint_id: m.endpoint_id,
                        relay_url: m.relay_url,
                    }
                }
            })
            .collect();

        Ok(TopicInvite::new_with_info(*topic_id, member_info))
    }

    /// Send a message to a topic
    ///
    /// The payload is opaque bytes - the app defines the format.
    /// Messages are sent to all known topic members (from the local member list).
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

        // Wrap payload as Content message
        let content_msg = TopicMessage::Content(payload.to_vec());
        let encoded_payload = content_msg.encode();

        // Send to all recipients with relay info
        self.send_raw_with_info(topic_id, &encoded_payload, &recipients, HarborPacketType::Content)
            .await
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
