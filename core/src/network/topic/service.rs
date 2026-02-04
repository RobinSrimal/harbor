//! Topic Service - Topic lifecycle management
//!
//! Handles:
//! - Creating topics
//! - Joining topics (with JOIN announcement)
//! - Leaving topics (with LEAVE announcement)
//! - Listing subscribed topics
//! - Generating invites with current membership

use std::sync::Arc;

use iroh::Endpoint;
use rusqlite::Connection as DbConnection;
use tokio::sync::Mutex;
use tracing::{debug, info, trace};

use crate::data::{
    add_topic_member, cleanup_pulled_for_topic, ensure_peer_exists, get_all_topics,
    get_topic_admin, get_topic_members, get_peer_relay_info, remove_topic_member,
    subscribe_topic_with_admin, unsubscribe_topic,
    LocalIdentity, WILDCARD_RECIPIENT,
};
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::send::topic_messages::{JoinMessage, LeaveMessage, TopicMessage};
use crate::network::send::{SendService, SendOptions};
use super::types::{MemberInfo, TopicInvite};

/// Error during Topic operations
#[derive(Debug)]
pub enum TopicError {
    /// Database error
    Database(String),
    /// Network/send error
    Network(String),
    /// Topic not found
    TopicNotFound,
}

impl std::fmt::Display for TopicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TopicError::Database(e) => write!(f, "database error: {}", e),
            TopicError::Network(e) => write!(f, "network error: {}", e),
            TopicError::TopicNotFound => write!(f, "topic not found"),
        }
    }
}

impl std::error::Error for TopicError {}

/// Topic service - manages topic lifecycle
pub struct TopicService {
    /// Iroh endpoint
    endpoint: Endpoint,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// Send service for announcements
    send_service: Arc<SendService>,
}

impl TopicService {
    /// Create a new Topic service
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        send_service: Arc<SendService>,
    ) -> Self {
        Self {
            endpoint,
            identity,
            db,
            send_service,
        }
    }

    /// Get our EndpointID
    fn endpoint_id(&self) -> [u8; 32] {
        self.identity.public_key
    }

    /// Get our current relay URL
    fn relay_url(&self) -> Option<String> {
        self.endpoint.addr().relay_urls().next().map(|u| u.to_string())
    }

    /// Create a new topic
    ///
    /// Creates a new topic with this node as the only member.
    /// Returns an invite that can be shared with others.
    pub async fn create_topic(&self) -> Result<TopicInvite, TopicError> {
        use rand::{rngs::OsRng, Rng};
        let mut topic_id = [0u8; 32];
        OsRng.fill(&mut topic_id);

        let our_id = self.endpoint_id();
        let relay_url = self.relay_url();

        let our_info = if let Some(relay) = relay_url {
            MemberInfo::with_relay(our_id, relay)
        } else {
            MemberInfo::new(our_id)
        };

        {
            let db = self.db.lock().await;
            // Ensure we're in the peers table (for FK constraint)
            ensure_peer_exists(&db, &our_id)
                .map_err(|e| TopicError::Database(e.to_string()))?;
            // Create topic with us as admin
            subscribe_topic_with_admin(&db, &topic_id, &our_id)
                .map_err(|e| TopicError::Database(e.to_string()))?;
            add_topic_member(&db, &topic_id, &our_id)
                .map_err(|e| TopicError::Database(e.to_string()))?;
        }

        Ok(TopicInvite::new_with_info(topic_id, our_id, vec![our_info]))
    }

    /// Join an existing topic
    ///
    /// Joins a topic using an invite received from another member.
    /// The invite contains all current topic members.
    /// Announces our presence to other members.
    pub async fn join_topic(&self, invite: TopicInvite) -> Result<(), TopicError> {
        let our_id = self.endpoint_id();
        let member_ids = invite.member_ids();
        // Get admin from invite (falls back to first member for legacy invites)
        let admin_id = invite.admin_id().unwrap_or(our_id);

        {
            let db = self.db.lock().await;
            // Ensure all peers exist in the peers table (for FK constraints)
            ensure_peer_exists(&db, &our_id)
                .map_err(|e| TopicError::Database(e.to_string()))?;
            ensure_peer_exists(&db, &admin_id)
                .map_err(|e| TopicError::Database(e.to_string()))?;
            for member_id in &member_ids {
                ensure_peer_exists(&db, member_id)
                    .map_err(|e| TopicError::Database(e.to_string()))?;
            }
            // Create topic subscription with the admin from invite
            subscribe_topic_with_admin(&db, &invite.topic_id, &admin_id)
                .map_err(|e| TopicError::Database(e.to_string()))?;

            add_topic_member(&db, &invite.topic_id, &our_id)
                .map_err(|e| TopicError::Database(e.to_string()))?;

            for member_id in &member_ids {
                add_topic_member(&db, &invite.topic_id, member_id)
                    .map_err(|e| TopicError::Database(e.to_string()))?;
            }
        }

        // Send join announcement
        let our_relay = self.relay_url();
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

        // Build recipient list with WILDCARD for Harbor storage
        let mut members_with_wildcard = invite.get_member_info().to_vec();
        members_with_wildcard.push(MemberInfo {
            endpoint_id: WILDCARD_RECIPIENT,
            relay_url: None,
        });

        let send_result = self
            .send_service
            .send_to_topic(
                &invite.topic_id,
                &payload,
                &members_with_wildcard,
                SendOptions::default().with_packet_type(HarborPacketType::Join),
            )
            .await
            .map_err(|e| TopicError::Network(e.to_string()));
        if let Err(e) = send_result {
            debug!(error = %e, "failed to send join announcement to some members");
        }

        info!(topic = hex::encode(invite.topic_id), "joined topic");
        Ok(())
    }

    /// Leave a topic
    pub async fn leave_topic(&self, topic_id: &[u8; 32]) -> Result<(), TopicError> {
        let our_id = self.endpoint_id();

        let members = {
            let db = self.db.lock().await;
            get_topic_members(&db, topic_id)
                .map_err(|e| TopicError::Database(e.to_string()))?
        };

        // Send leave announcement
        let leave_msg = LeaveMessage::new(our_id);
        let payload = TopicMessage::Leave(leave_msg).encode();

        let mut remaining_members: Vec<MemberInfo> = members
            .into_iter()
            .filter(|m| *m != our_id)
            .map(|endpoint_id| MemberInfo::new(endpoint_id))
            .collect();
        remaining_members.push(MemberInfo {
            endpoint_id: WILDCARD_RECIPIENT,
            relay_url: None,
        });

        let send_result = self
            .send_service
            .send_to_topic(topic_id, &payload, &remaining_members, SendOptions::default().with_packet_type(HarborPacketType::Leave))
            .await
            .map_err(|e| TopicError::Network(e.to_string()));
        if let Err(e) = send_result {
            debug!(error = %e, "failed to send leave announcement to some members");
        }

        // Remove ourselves and unsubscribe
        {
            let db = self.db.lock().await;
            remove_topic_member(&db, topic_id, &our_id)
                .map_err(|e| TopicError::Database(e.to_string()))?;
            unsubscribe_topic(&db, topic_id)
                .map_err(|e| TopicError::Database(e.to_string()))?;
            cleanup_pulled_for_topic(&db, topic_id)
                .map_err(|e| TopicError::Database(e.to_string()))?;
        }

        info!(topic = hex::encode(*topic_id), "left topic");
        Ok(())
    }

    /// List all topics we're subscribed to
    pub async fn list_topics(&self) -> Result<Vec<[u8; 32]>, TopicError> {
        let db = self.db.lock().await;
        let topics = get_all_topics(&db).map_err(|e| TopicError::Database(e.to_string()))?;
        Ok(topics.into_iter().map(|t| t.topic_id).collect())
    }

    /// Get an invite for an existing topic
    ///
    /// Returns a fresh invite containing ALL current topic members.
    pub async fn get_invite(&self, topic_id: &[u8; 32]) -> Result<TopicInvite, TopicError> {
        let db = self.db.lock().await;
        let members = get_topic_members(&db, topic_id)
            .map_err(|e| TopicError::Database(e.to_string()))?;

        if members.is_empty() {
            return Err(TopicError::TopicNotFound);
        }

        // Get the admin for this topic
        let admin_id = get_topic_admin(&db, topic_id)
            .map_err(|e| TopicError::Database(e.to_string()))?
            .ok_or(TopicError::TopicNotFound)?;

        let our_id = self.endpoint_id();
        let our_relay = self.relay_url();

        let member_info: Vec<MemberInfo> = members
            .into_iter()
            .map(|endpoint_id| {
                if endpoint_id == our_id {
                    // Use our current live relay URL
                    if let Some(ref relay) = our_relay {
                        MemberInfo::with_relay(endpoint_id, relay.clone())
                    } else {
                        MemberInfo::new(endpoint_id)
                    }
                } else {
                    // Look up relay URL from peers table
                    let relay_url = get_peer_relay_info(&db, &endpoint_id)
                        .ok()
                        .flatten()
                        .map(|(url, _)| url);
                    MemberInfo {
                        endpoint_id,
                        relay_url,
                    }
                }
            })
            .collect();

        Ok(TopicInvite::new_with_info(*topic_id, admin_id, member_info))
    }
}
