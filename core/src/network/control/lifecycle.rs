//! Topic lifecycle operations
//!
//! Local/initiator-side topic management:
//! - Create a new topic
//! - Join an existing topic via invite
//! - List subscribed topics
//! - Get a fresh invite for sharing

use tracing::{info, warn};

use crate::data::{
    add_topic_member, ensure_peer_exists, get_all_topics, get_current_epoch, get_peer_relay_info,
    get_topic_admin, get_topic_members, subscribe_topic_with_admin,
};
use crate::network::membership::create_membership_proof;
use crate::security::harbor_id_from_topic;

use super::protocol::{ControlPacketType, TopicJoin};
use super::service::{
    ControlError, ControlResult, ControlService, compute_control_pow, generate_id,
};
use super::types::{MemberInfo, TopicInvite};

fn encode_control_message<T: serde::Serialize>(
    message: &T,
    packet_type: ControlPacketType,
) -> ControlResult<Vec<u8>> {
    postcard::to_allocvec(message).map_err(|e| {
        ControlError::Rpc(format!(
            "failed to encode {:?} control message: {}",
            packet_type, e
        ))
    })
}

fn resolve_join_invite(invite: &TopicInvite) -> ControlResult<([u8; 32], Vec<[u8; 32]>)> {
    let member_ids = invite.member_ids();
    if member_ids.is_empty() {
        return Err(ControlError::InvalidState(
            "invite has no members".to_string(),
        ));
    }

    let admin_id = invite.admin_id().ok_or_else(|| {
        ControlError::InvalidState("invite missing admin and member fallback".to_string())
    })?;

    Ok((admin_id, member_ids))
}

fn resolve_join_epoch(current_epoch: Option<u64>) -> u64 {
    current_epoch.unwrap_or(0)
}

impl ControlService {
    /// Create a new topic
    ///
    /// Creates a new topic with this node as the only member.
    /// Returns an invite that can be shared with others.
    pub async fn create_topic(&self) -> ControlResult<TopicInvite> {
        use rand::{Rng, rngs::OsRng};
        let mut topic_id = [0u8; 32];
        OsRng.fill(&mut topic_id);

        let our_id = self.local_id();
        let relay_url = self
            .endpoint()
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string());

        let our_info = if let Some(relay) = relay_url {
            MemberInfo::with_relay(our_id, relay)
        } else {
            MemberInfo::new(our_id)
        };

        {
            let db = self.db().lock().await;
            // Ensure we're in the peers table (for FK constraint)
            ensure_peer_exists(&db, &our_id).map_err(|e| ControlError::Database(e.to_string()))?;
            // Create topic with us as admin
            subscribe_topic_with_admin(&db, &topic_id, &our_id)
                .map_err(|e| ControlError::Database(e.to_string()))?;
            add_topic_member(&db, &topic_id, &our_id)
                .map_err(|e| ControlError::Database(e.to_string()))?;
        }

        info!(topic = %hex::encode(&topic_id[..8]), "created topic");
        Ok(TopicInvite::new_with_info(topic_id, our_id, vec![our_info]))
    }

    /// Join an existing topic using an invite
    ///
    /// Joins a topic using an invite received from another member.
    /// The invite contains all current topic members.
    /// Announces our presence to other members via Control ALPN.
    pub async fn join_topic(&self, invite: TopicInvite) -> ControlResult<()> {
        let our_id = self.local_id();
        // Resolve admin + members (supports legacy fallback while rejecting malformed invites).
        let (admin_id, member_ids) = resolve_join_invite(&invite)?;
        let join_epoch = {
            let db = self.db().lock().await;
            get_current_epoch(&db, &invite.topic_id)
                .map(resolve_join_epoch)
                .map_err(|e| ControlError::Database(e.to_string()))?
        };

        {
            let db = self.db().lock().await;
            // Ensure all peers exist in the peers table (for FK constraints)
            ensure_peer_exists(&db, &our_id).map_err(|e| ControlError::Database(e.to_string()))?;
            ensure_peer_exists(&db, &admin_id)
                .map_err(|e| ControlError::Database(e.to_string()))?;
            for member_id in &member_ids {
                ensure_peer_exists(&db, member_id)
                    .map_err(|e| ControlError::Database(e.to_string()))?;
            }
            // Create topic subscription with the admin from invite
            subscribe_topic_with_admin(&db, &invite.topic_id, &admin_id)
                .map_err(|e| ControlError::Database(e.to_string()))?;

            add_topic_member(&db, &invite.topic_id, &our_id)
                .map_err(|e| ControlError::Database(e.to_string()))?;

            for member_id in &member_ids {
                add_topic_member(&db, &invite.topic_id, member_id)
                    .map_err(|e| ControlError::Database(e.to_string()))?;
            }
        }

        // Update connection gate with all topic members
        if let Some(gate) = self.connection_gate() {
            for member_id in &member_ids {
                if *member_id != our_id {
                    gate.add_topic_peer(member_id, &invite.topic_id).await;
                }
            }
        }

        // Send join announcement via CONTROL ALPN
        let join_message_id = generate_id();
        let our_relay = self
            .endpoint()
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string());
        let harbor_id = harbor_id_from_topic(&invite.topic_id);
        let membership_proof = create_membership_proof(&invite.topic_id, &harbor_id, &our_id);
        let pow = compute_control_pow(&our_id, ControlPacketType::TopicJoin)?;

        let join = TopicJoin {
            message_id: join_message_id,
            harbor_id,
            sender_id: our_id,
            epoch: join_epoch,
            relay_url: our_relay,
            membership_proof,
            pow,
        };

        // Try direct delivery to all members via CONTROL ALPN
        for member_info in invite.effective_member_info() {
            if member_info.endpoint_id == our_id {
                continue;
            }
            if let Ok(client) = self.dial_peer(&member_info.endpoint_id).await {
                let _ = client.rpc(join.clone()).await;
            }
        }

        // Store for harbor replication (topic-scoped: harbor_id = hash(topic_id))
        self.store_control_packet(
            &join_message_id,
            &harbor_id,
            &member_ids,
            &encode_control_message(&join, ControlPacketType::TopicJoin)?,
            ControlPacketType::TopicJoin,
        )
        .await?;

        info!(topic = %hex::encode(&invite.topic_id[..8]), "joined topic");
        Ok(())
    }

    /// List all topics we're subscribed to
    pub async fn list_topics(&self) -> ControlResult<Vec<[u8; 32]>> {
        let db = self.db().lock().await;
        let topics = get_all_topics(&db).map_err(|e| ControlError::Database(e.to_string()))?;
        Ok(topics.into_iter().map(|t| t.topic_id).collect())
    }

    /// Get an invite for an existing topic
    ///
    /// Returns a fresh invite containing ALL current topic members.
    pub async fn get_invite(&self, topic_id: &[u8; 32]) -> ControlResult<TopicInvite> {
        let db = self.db().lock().await;
        let members =
            get_topic_members(&db, topic_id).map_err(|e| ControlError::Database(e.to_string()))?;

        if members.is_empty() {
            return Err(ControlError::TopicNotFound);
        }

        // Get the admin for this topic
        let admin_id = get_topic_admin(&db, topic_id)
            .map_err(|e| ControlError::Database(e.to_string()))?
            .ok_or(ControlError::TopicNotFound)?;

        let our_id = self.local_id();
        let our_relay = self
            .endpoint()
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string());

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
                    let relay_url = match get_peer_relay_info(&db, &endpoint_id) {
                        Ok(info) => info.map(|(url, _)| url),
                        Err(e) => {
                            warn!(
                                peer = %hex::encode(&endpoint_id[..8]),
                                error = %e,
                                "failed to look up peer relay info for invite"
                            );
                            None
                        }
                    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::ser::{Error as _, Serializer};

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    struct FailingSerialize;

    impl serde::Serialize for FailingSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(S::Error::custom("serialize failed"))
        }
    }

    #[test]
    fn test_encode_control_message_maps_errors() {
        let err = encode_control_message(&FailingSerialize, ControlPacketType::TopicJoin)
            .expect_err("encoding should fail");

        match err {
            ControlError::Rpc(message) => {
                assert!(message.contains("TopicJoin"));
                assert!(message.contains("failed to encode"));
            }
            other => panic!("unexpected error variant: {:?}", other),
        }
    }

    #[test]
    fn test_resolve_join_invite_accepts_legacy_admin_fallback() {
        let invite = TopicInvite {
            topic_id: test_id(1),
            admin_id: None,
            member_info: vec![],
            members: vec![test_id(7), test_id(8)],
        };

        let (admin_id, member_ids) = resolve_join_invite(&invite).unwrap();
        assert_eq!(admin_id, test_id(7));
        assert_eq!(member_ids, vec![test_id(7), test_id(8)]);
    }

    #[test]
    fn test_resolve_join_invite_rejects_empty_members() {
        let invite = TopicInvite {
            topic_id: test_id(1),
            admin_id: None,
            member_info: vec![],
            members: vec![],
        };

        match resolve_join_invite(&invite).expect_err("invite should be rejected") {
            ControlError::InvalidState(message) => assert!(message.contains("no members")),
            other => panic!("unexpected error variant: {:?}", other),
        }
    }

    #[test]
    fn test_resolve_join_epoch() {
        assert_eq!(resolve_join_epoch(None), 0);
        assert_eq!(resolve_join_epoch(Some(3)), 3);
    }
}
