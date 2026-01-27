//! Topic operations for the Protocol
//!
//! This module contains public API methods for topic management:
//! - create_topic: Create a new topic
//! - join_topic: Join an existing topic
//! - leave_topic: Leave a topic
//! - list_topics: List all subscribed topics
//! - get_invite: Get an invite for a topic

use tracing::{debug, info, trace};

use crate::data::{
    add_topic_member, cleanup_pulled_for_topic, get_all_topics,
    get_topic_members_with_info, remove_topic_member, subscribe_topic, unsubscribe_topic,
};
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::send::topic_messages::{JoinMessage, LeaveMessage, TopicMessage};

use super::core::Protocol;
use super::error::ProtocolError;
use super::types::{MemberInfo, TopicInvite};

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
        let relay_url = self.endpoint.addr().relay_urls().next().map(|u| u.to_string());

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
        let our_relay = self.endpoint.addr().relay_urls().next().map(|u| u.to_string());
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
        use crate::data::WILDCARD_RECIPIENT;
        let mut members_with_wildcard = invite.get_member_info().to_vec();
        members_with_wildcard.push(MemberInfo {
            endpoint_id: WILDCARD_RECIPIENT,
            relay_url: None,
        });

        // Send to all existing members using their connection info (best effort)
        // The WILDCARD recipient is filtered out for direct sending but kept for Harbor storage
        let send_result = self
            .send_raw(
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

        // Get current members with relay info before we leave
        let members_with_info = {
            let db = self.db.lock().await;
            get_topic_members_with_info(&db, topic_id)
                .map_err(|e| ProtocolError::Database(e.to_string()))?
        };

        // Send leave announcement to remaining members + WILDCARD for future member discovery
        let leave_msg = LeaveMessage::new(our_id);
        let payload = TopicMessage::Leave(leave_msg).encode();

        use crate::data::WILDCARD_RECIPIENT;
        let mut remaining_members: Vec<MemberInfo> = members_with_info
            .into_iter()
            .filter(|m| m.endpoint_id != our_id)
            .map(|m| MemberInfo {
                endpoint_id: m.endpoint_id,
                relay_url: m.relay_url,
            })
            .collect();
        remaining_members.push(MemberInfo {
            endpoint_id: WILDCARD_RECIPIENT,
            relay_url: None,
        });

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
        let topics = get_all_topics(&db).map_err(|e| ProtocolError::Database(e.to_string()))?;

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
        let our_relay = self.endpoint.addr().relay_urls().next().map(|u| u.to_string());

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
}
