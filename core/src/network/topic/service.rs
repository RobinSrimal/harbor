//! Topic Service - Topic lifecycle management
//!
//! Handles:
//! - Creating topics
//! - Joining topics (with JOIN announcement)
//! - Leaving topics (with LEAVE announcement)
//! - Listing subscribed topics
//! - Generating invites with current membership

use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, EndpointId};
use rusqlite::Connection as DbConnection;
use tokio::sync::Mutex;
use tracing::{debug, info, trace, warn};

use crate::data::{
    add_topic_member, cleanup_pulled_for_topic, ensure_peer_exists, get_all_topics,
    get_topic_admin, get_topic_members, get_peer_relay_info, remove_topic_member,
    subscribe_topic_with_admin, unsubscribe_topic,
    LocalIdentity, WILDCARD_RECIPIENT,
};
use crate::network::control::protocol::{ControlPacketType, ControlRpcProtocol, TopicJoin, TopicLeave, CONTROL_ALPN};
use crate::network::membership::create_membership_proof;
use crate::network::connect;
use crate::resilience::{ProofOfWork, PoWConfig, build_context};
use crate::security::harbor_id_from_topic;
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

        // Update our connection gate with all topic members
        // This allows us to RECEIVE messages from them via SEND ALPN
        if let Some(gate) = self.send_service.connection_gate() {
            for member_id in &member_ids {
                if *member_id != our_id {
                    gate.add_topic_peer(member_id, &invite.topic_id).await;
                }
            }
        }

        // Send join announcement via CONTROL ALPN (no gate checking)
        let our_relay = self.relay_url();
        let message_id = {
            use rand::RngCore;
            let mut id = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut id);
            id
        };
        let harbor_id = harbor_id_from_topic(&invite.topic_id);
        let membership_proof = create_membership_proof(&invite.topic_id, &harbor_id, &our_id);
        let pow_context = build_context(&[&our_id, &[ControlPacketType::TopicJoin as u8]]);
        let pow = ProofOfWork::compute(&pow_context, PoWConfig::control().base_difficulty)
            .ok_or_else(|| TopicError::Network("failed to compute PoW".to_string()))?;

        let join_msg = TopicJoin {
            message_id,
            harbor_id,
            sender_id: our_id,
            epoch: 0, // Initial join is always epoch 0
            relay_url: our_relay.clone(),
            membership_proof,
            pow,
        };

        trace!(
            our_id = %hex::encode(our_id),
            our_relay = ?our_relay,
            member_count = invite.get_member_info().len(),
            "sending join announcement via Control ALPN"
        );

        // Send TopicJoin to each member via CONTROL ALPN (bypasses gate check)
        // Use short timeout (3s) since offline members are handled by Harbor replication
        for member_info in invite.get_member_info() {
            if member_info.endpoint_id == our_id {
                continue;
            }

            // Look up relay URL from invite or database
            let relay_url = if let Some(ref url) = member_info.relay_url {
                url.parse::<iroh::RelayUrl>().ok()
            } else {
                let db = self.db.lock().await;
                get_peer_relay_info(&db, &member_info.endpoint_id)
                    .ok()
                    .flatten()
                    .and_then(|(url, _)| url.parse::<iroh::RelayUrl>().ok())
            };

            let node_id = match EndpointId::from_bytes(&member_info.endpoint_id) {
                Ok(id) => id,
                Err(_) => continue,
            };

            // Connect via CONTROL ALPN and send TopicJoin
            match connect::connect(
                &self.endpoint,
                node_id,
                relay_url.as_ref(),
                None,
                CONTROL_ALPN,
                Duration::from_secs(3), // Short timeout - offline members handled by Harbor
            ).await {
                Ok(result) => {
                    let client = irpc::Client::<ControlRpcProtocol>::boxed(
                        crate::network::rpc::ExistingConnection::new(&result.connection),
                    );
                    if let Err(e) = client.rpc(join_msg.clone()).await {
                        debug!(
                            member = %hex::encode(&member_info.endpoint_id[..8]),
                            error = %e,
                            "failed to send TopicJoin via Control"
                        );
                    }
                }
                Err(e) => {
                    debug!(
                        member = %hex::encode(&member_info.endpoint_id[..8]),
                        error = %e,
                        "failed to connect to member via Control ALPN"
                    );
                }
            }
        }

        // Also store for Harbor replication (for offline members)
        let serialized = postcard::to_allocvec(&join_msg).expect("serialization should succeed");
        let mut payload = Vec::with_capacity(1 + serialized.len());
        payload.push(ControlPacketType::TopicJoin as u8);
        payload.extend_from_slice(&serialized);

        let wildcard_only = vec![MemberInfo {
            endpoint_id: WILDCARD_RECIPIENT,
            relay_url: None,
        }];

        let _ = self
            .send_service
            .send_to_topic(
                &invite.topic_id,
                &payload,
                &wildcard_only,
                SendOptions::default(),
            )
            .await;

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

        // Send leave announcement (Control protocol TopicLeave)
        let message_id = {
            use rand::RngCore;
            let mut id = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut id);
            id
        };
        let harbor_id = harbor_id_from_topic(topic_id);
        let membership_proof = create_membership_proof(topic_id, &harbor_id, &our_id);
        let pow_context = build_context(&[&our_id, &[ControlPacketType::TopicLeave as u8]]);
        let pow = ProofOfWork::compute(&pow_context, PoWConfig::control().base_difficulty)
            .ok_or_else(|| TopicError::Network("failed to compute PoW".to_string()))?;

        let leave_msg = TopicLeave {
            message_id,
            harbor_id,
            sender_id: our_id,
            epoch: 0, // TODO: use current epoch
            membership_proof,
            pow,
        };
        // Encode as Control packet: [type_byte][serialized_data]
        let serialized = postcard::to_allocvec(&leave_msg).expect("serialization should succeed");
        let mut payload = Vec::with_capacity(1 + serialized.len());
        payload.push(ControlPacketType::TopicLeave as u8);
        payload.extend_from_slice(&serialized);

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
            .send_to_topic(topic_id, &payload, &remaining_members, SendOptions::default())
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
