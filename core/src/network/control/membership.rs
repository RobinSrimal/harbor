//! Topic membership management
//!
//! Handles topic-level membership operations:
//! - Outgoing: invite, accept/decline invite, leave, remove member
//! - Incoming: handle invite/join/leave/remove from remote peers

use tracing::{info, warn};

use crate::data::{
    add_topic_member, delete_pending_invite, get_current_epoch, get_current_epoch_key,
    get_pending_invite, get_topic, get_topic_admin, get_topic_members, is_topic_admin,
    store_epoch_key, store_pending_invite, subscribe_topic_with_admin,
};
use crate::data::membership::get_topic_by_harbor_id;
use crate::network::membership::{create_membership_proof, verify_membership_proof};
use crate::protocol::{
    ProtocolEvent, TopicEpochRotatedEvent, TopicInviteReceivedEvent,
    TopicMemberJoinedEvent, TopicMemberLeftEvent,
};
use crate::security::harbor_id_from_topic;

use super::protocol::{
    ControlAck, ControlPacketType, RemoveMember, TopicInvite as WireTopicInvite, TopicJoin,
    TopicLeave,
};
use super::service::{
    compute_control_pow, generate_id, verify_sender, ControlError, ControlResult, ControlService,
};

impl ControlService {
    // =========================================================================
    // Outgoing operations
    // =========================================================================

    /// Invite a peer to a topic
    pub async fn invite_to_topic(
        &self,
        peer_id: &[u8; 32],
        topic_id: &[u8; 32],
    ) -> ControlResult<[u8; 32]> {
        let message_id = generate_id();

        // Get topic info, admin, and current epoch key
        // Topics always have at least epoch 0 key (stored at creation time)
        let (epoch, epoch_key, admin_id, members) = {
            let db = self.db().lock().await;
            let _topic =
                get_topic(&db, topic_id).map_err(|e| ControlError::Database(e.to_string()))?;
            let _topic = _topic.ok_or(ControlError::TopicNotFound)?;
            let admin_id = get_topic_admin(&db, topic_id)
                .map_err(|e| ControlError::Database(e.to_string()))?
                .ok_or_else(|| ControlError::Database("topic missing admin".to_string()))?;
            let members =
                get_topic_members(&db, topic_id).map_err(|e| ControlError::Database(e.to_string()))?;
            // Get current epoch key - topics always have at least epoch 0
            let current = get_current_epoch_key(&db, topic_id)
                .map_err(|e| ControlError::Database(e.to_string()))?
                .ok_or_else(|| ControlError::Database("topic missing epoch key".to_string()))?;
            (current.epoch, current.key_data, admin_id, members)
        };

        let pow = compute_control_pow(&self.local_id(), ControlPacketType::TopicInvite)?;
        let wire_invite = WireTopicInvite {
            message_id,
            sender_id: self.local_id(),
            topic_id: *topic_id,
            topic_name: None, // Application concern, not stored at protocol level
            epoch,
            epoch_key,
            admin_id,
            members,
            pow,
        };

        // Try direct delivery
        if let Ok(client) = self.dial_peer(peer_id).await {
            if let Ok(ack) = client.rpc(wire_invite.clone()).await {
                if ack.success {
                    info!(
                        peer = %hex::encode(&peer_id[..8]),
                        topic = %hex::encode(&topic_id[..8]),
                        "topic invite sent directly"
                    );
                }
            }
        }

        // Store for harbor replication (point-to-point: harbor_id = recipient)
        self.store_control_packet(
            &message_id,
            peer_id,
            &[*peer_id],
            &postcard::to_allocvec(&wire_invite).unwrap(),
            ControlPacketType::TopicInvite,
        )
        .await?;

        Ok(message_id)
    }

    /// Accept a topic invite
    pub async fn accept_topic_invite(
        &self,
        message_id: &[u8; 32],
    ) -> ControlResult<()> {
        // Get the pending invite
        let invite = {
            let db = self.db().lock().await;
            get_pending_invite(&db, message_id)
                .map_err(|e| ControlError::Database(e.to_string()))?
                .ok_or(ControlError::RequestNotFound)?
        };

        // Subscribe to topic and add members
        {
            let db = self.db().lock().await;
            // Subscribe with the admin from the invite
            subscribe_topic_with_admin(&db, &invite.topic_id, &invite.admin_id)
                .map_err(|e| ControlError::Database(e.to_string()))?;
            // Add ourselves as a member (required for get_topics_for_member to find this topic)
            let _ = add_topic_member(&db, &invite.topic_id, &self.local_id());
            // Add other members from the invite
            for member in &invite.members {
                let _ = add_topic_member(&db, &invite.topic_id, member);
            }
            // Store the epoch key from the invite
            store_epoch_key(&db, &invite.topic_id, invite.epoch, &invite.epoch_key)
                .map_err(|e| ControlError::Database(e.to_string()))?;
        }

        // Update connection gate with all topic peers
        if let Some(gate) = self.connection_gate() {
            gate.add_topic_peers(&invite.topic_id, &invite.members).await;
        }

        // Send TopicJoin to all members
        let join_message_id = generate_id();
        let our_relay = self.endpoint().addr().relay_urls().next().map(|u| u.to_string());

        let harbor_id = harbor_id_from_topic(&invite.topic_id);
        let membership_proof = create_membership_proof(
            &invite.topic_id,
            &harbor_id,
            &self.local_id(),
        );
        let pow = compute_control_pow(&self.local_id(), ControlPacketType::TopicJoin)?;

        let join = TopicJoin {
            message_id: join_message_id,
            harbor_id,
            sender_id: self.local_id(),
            epoch: invite.epoch,
            relay_url: our_relay,
            membership_proof,
            pow,
        };

        // Try direct delivery to all members
        for member in &invite.members {
            if *member == self.local_id() {
                continue;
            }
            if let Ok(client) = self.dial_peer(member).await {
                let _ = client.rpc(join.clone()).await;
            }
        }

        // Store for harbor replication (topic-scoped: harbor_id = hash(topic_id))
        self.store_control_packet(
            &join_message_id,
            &harbor_id,
            &invite.members,
            &postcard::to_allocvec(&join).unwrap(),
            ControlPacketType::TopicJoin,
        )
        .await?;

        // Delete the pending invite
        {
            let db = self.db().lock().await;
            delete_pending_invite(&db, message_id)
                .map_err(|e| ControlError::Database(e.to_string()))?;
        }

        info!(
            topic = %hex::encode(&invite.topic_id[..8]),
            admin = %hex::encode(&invite.admin_id[..8]),
            "accepted topic invite"
        );

        Ok(())
    }

    /// Decline a topic invite
    pub async fn decline_topic_invite(
        &self,
        message_id: &[u8; 32],
    ) -> ControlResult<()> {
        // Just delete the pending invite
        let db = self.db().lock().await;
        delete_pending_invite(&db, message_id).map_err(|e| ControlError::Database(e.to_string()))?;

        info!(message_id = %hex::encode(&message_id[..8]), "declined topic invite");
        Ok(())
    }

    /// Leave a topic
    pub async fn leave_topic(&self, topic_id: &[u8; 32]) -> ControlResult<()> {
        let message_id = generate_id();

        // Get current members and epoch before leaving
        let (members, epoch) = {
            let db = self.db().lock().await;
            let members = get_topic_members(&db, topic_id).map_err(|e| ControlError::Database(e.to_string()))?;
            let epoch = get_current_epoch(&db, topic_id)
                .map_err(|e| ControlError::Database(e.to_string()))?
                .unwrap_or(1);
            (members, epoch)
        };

        let harbor_id = harbor_id_from_topic(topic_id);
        let membership_proof = create_membership_proof(topic_id, &harbor_id, &self.local_id());
        let pow = compute_control_pow(&self.local_id(), ControlPacketType::TopicLeave)?;

        let leave = TopicLeave {
            message_id,
            harbor_id,
            sender_id: self.local_id(),
            epoch,
            membership_proof,
            pow,
        };

        // Try direct delivery to all members
        for member in &members {
            if *member == self.local_id() {
                continue;
            }
            if let Ok(client) = self.dial_peer(member).await {
                let _ = client.rpc(leave.clone()).await;
            }
        }

        // Store for harbor replication (topic-scoped)
        self.store_control_packet(
            &message_id,
            &harbor_id,
            &members,
            &postcard::to_allocvec(&leave).unwrap(),
            ControlPacketType::TopicLeave,
        )
        .await?;

        // Unsubscribe from topic
        {
            let db = self.db().lock().await;
            crate::data::unsubscribe_topic(&db, topic_id)
                .map_err(|e| ControlError::Database(e.to_string()))?;
        }

        // Update connection gate - remove this topic
        if let Some(gate) = self.connection_gate() {
            gate.remove_topic(topic_id).await;
        }

        info!(topic = %hex::encode(&topic_id[..8]), "left topic");
        Ok(())
    }

    /// Remove a member from a topic (admin only)
    ///
    /// Generates a new epoch key and sends it to all remaining members.
    pub async fn remove_topic_member(
        &self,
        topic_id: &[u8; 32],
        member_id: &[u8; 32],
    ) -> ControlResult<()> {
        // Verify we are admin and get topic members
        let (members, current_epoch) = {
            let db = self.db().lock().await;
            let _topic =
                get_topic(&db, topic_id).map_err(|e| ControlError::Database(e.to_string()))?;
            let _topic = _topic.ok_or(ControlError::TopicNotFound)?;

            // Verify we are the admin
            let our_id = self.local_id();
            if !is_topic_admin(&db, topic_id, &our_id).map_err(|e| ControlError::Database(e.to_string()))? {
                return Err(ControlError::NotAuthorized("only admin can remove members".to_string()));
            }

            let members = get_topic_members(&db, topic_id).map_err(|e| ControlError::Database(e.to_string()))?;
            let current_epoch = get_current_epoch(&db, topic_id)
                .map_err(|e| ControlError::Database(e.to_string()))?
                .unwrap_or(1);
            (members, current_epoch)
        };

        // Generate new epoch key (increment epoch)
        let new_epoch = current_epoch + 1;
        let new_epoch_key = generate_id();

        // Get remaining members (excluding the removed one)
        let remaining: Vec<[u8; 32]> = members
            .iter()
            .filter(|m| *m != member_id)
            .copied()
            .collect();

        // Send RemoveMember to each remaining member individually (DM encrypted)
        for member in &remaining {
            if *member == self.local_id() {
                continue;
            }

            let msg_id = generate_id();
            let harbor_id = harbor_id_from_topic(topic_id);
            let membership_proof =
                create_membership_proof(topic_id, &harbor_id, &self.local_id());
            let pow = compute_control_pow(&self.local_id(), ControlPacketType::RemoveMember)?;

            let remove = RemoveMember {
                message_id: msg_id,
                harbor_id,
                sender_id: self.local_id(),
                removed_member: *member_id,
                new_epoch,
                new_epoch_key,
                membership_proof,
                pow,
            };

            // Try direct delivery
            if let Ok(client) = self.dial_peer(member).await {
                let _ = client.rpc(remove.clone()).await;
            }

            // Store for harbor replication (point-to-point: harbor_id = recipient)
            // Each RemoveMember goes to a specific member
            self.store_control_packet(
                &msg_id,
                member, // harbor_id = this specific recipient
                &[*member],
                &postcard::to_allocvec(&remove).unwrap(),
                ControlPacketType::RemoveMember,
            )
            .await?;
        }

        // Remove member from our local membership and store new epoch key
        {
            let db = self.db().lock().await;
            crate::data::remove_topic_member(&db, topic_id, member_id)
                .map_err(|e| ControlError::Database(e.to_string()))?;
            // Store the new epoch key
            store_epoch_key(&db, topic_id, new_epoch, &new_epoch_key)
                .map_err(|e| ControlError::Database(e.to_string()))?;
        }

        // Update connection gate
        if let Some(gate) = self.connection_gate() {
            gate.remove_topic_peer(member_id, topic_id).await;
        }

        info!(
            topic = %hex::encode(&topic_id[..8]),
            removed = %hex::encode(&member_id[..8]),
            new_epoch = new_epoch,
            "removed member from topic"
        );

        Ok(())
    }

    // =========================================================================
    // Incoming handlers
    // =========================================================================

    /// Handle an incoming TopicInvite
    pub async fn handle_topic_invite(
        &self,
        invite: &WireTopicInvite,
        sender_id: [u8; 32],
    ) -> ControlAck {
        // Verify PoW first
        if let Err(e) = self.verify_pow(&invite.pow, &sender_id, ControlPacketType::TopicInvite) {
            warn!(
                sender = %hex::encode(&sender_id[..8]),
                error = %e,
                "topic invite rejected: insufficient PoW"
            );
            return ControlAck::failure(invite.message_id, &e.to_string());
        }

        // Verify sender
        if !verify_sender(&invite.sender_id, &sender_id) {
            warn!("topic invite sender mismatch");
            return ControlAck::failure(invite.message_id, "sender mismatch");
        }

        // Store the pending invite
        {
            let db = self.db().lock().await;
            if let Err(e) = store_pending_invite(
                &db,
                &invite.message_id,
                &invite.topic_id,
                &sender_id,
                invite.topic_name.as_deref(),
                invite.epoch,
                &invite.epoch_key,
                &invite.admin_id,
                &invite.members,
            ) {
                warn!(error = %e, "failed to store pending invite");
                return ControlAck::failure(invite.message_id, "internal error");
            }
        }

        info!(
            sender = %hex::encode(&sender_id[..8]),
            topic = %hex::encode(&invite.topic_id[..8]),
            admin = %hex::encode(&invite.admin_id[..8]),
            message_id = %hex::encode(&invite.message_id[..8]),
            "topic invite received"
        );

        // Emit event for app to decide
        let event = ProtocolEvent::TopicInviteReceived(TopicInviteReceivedEvent {
            message_id: invite.message_id,
            topic_id: invite.topic_id,
            sender_id,
            topic_name: invite.topic_name.clone(),
            admin_id: invite.admin_id,
            member_count: invite.members.len(),
        });
        let _ = self.event_tx().send(event).await;

        ControlAck::success(invite.message_id)
    }

    /// Handle an incoming TopicJoin
    pub async fn handle_topic_join(
        &self,
        join: &TopicJoin,
        sender_id: [u8; 32],
    ) -> ControlAck {
        // Verify PoW first
        if let Err(e) = self.verify_pow(&join.pow, &sender_id, ControlPacketType::TopicJoin) {
            warn!(
                sender = %hex::encode(&sender_id[..8]),
                error = %e,
                "topic join rejected: insufficient PoW"
            );
            return ControlAck::failure(join.message_id, &e.to_string());
        }

        // Verify sender
        if !verify_sender(&join.sender_id, &sender_id) {
            warn!("topic join sender mismatch");
            return ControlAck::failure(join.message_id, "sender mismatch");
        }

        // Resolve topic_id from harbor_id
        let topic_id = {
            let db = self.db().lock().await;
            match get_topic_by_harbor_id(&db, &join.harbor_id) {
                Ok(Some(topic)) => topic.topic_id,
                Ok(None) => {
                    warn!(harbor_id = %hex::encode(&join.harbor_id[..8]), "unknown topic for join");
                    return ControlAck::failure(join.message_id, "unknown topic");
                }
                Err(e) => {
                    warn!(error = %e, "failed to resolve topic from harbor_id");
                    return ControlAck::failure(join.message_id, "internal error");
                }
            }
        };

        // Verify membership proof
        if !verify_membership_proof(&topic_id, &join.harbor_id, &sender_id, &join.membership_proof) {
            warn!("topic join invalid membership proof");
            return ControlAck::failure(join.message_id, "invalid membership proof");
        }

        // Add member to topic and store relay URL
        {
            let db = self.db().lock().await;
            if let Err(e) = add_topic_member(&db, &topic_id, &sender_id) {
                warn!(error = %e, "failed to add topic member");
                // Don't fail the ack - member might already exist
            }
            // Store relay URL if provided (for later connections)
            if let Some(ref relay_url) = join.relay_url {
                let _ = crate::data::dht::update_peer_relay_url(
                    &db,
                    &sender_id,
                    relay_url,
                    crate::data::current_timestamp(),
                );
            }
        }

        // Update connection gate with new topic peer
        if let Some(gate) = self.connection_gate() {
            gate.add_topic_peer(&sender_id, &topic_id).await;
        }

        info!(
            member = %hex::encode(&sender_id[..8]),
            topic = %hex::encode(&topic_id[..8]),
            epoch = join.epoch,
            "topic member joined"
        );

        // Emit event
        let event = ProtocolEvent::TopicMemberJoined(TopicMemberJoinedEvent {
            topic_id,
            member_id: sender_id,
            relay_url: join.relay_url.clone(),
        });
        let _ = self.event_tx().send(event).await;

        ControlAck::success(join.message_id)
    }

    /// Handle an incoming TopicLeave
    pub async fn handle_topic_leave(
        &self,
        leave: &TopicLeave,
        sender_id: [u8; 32],
    ) -> ControlAck {
        // Verify PoW first
        if let Err(e) = self.verify_pow(&leave.pow, &sender_id, ControlPacketType::TopicLeave) {
            warn!(
                sender = %hex::encode(&sender_id[..8]),
                error = %e,
                "topic leave rejected: insufficient PoW"
            );
            return ControlAck::failure(leave.message_id, &e.to_string());
        }

        // Verify sender
        if !verify_sender(&leave.sender_id, &sender_id) {
            warn!("topic leave sender mismatch");
            return ControlAck::failure(leave.message_id, "sender mismatch");
        }

        // Resolve topic_id from harbor_id
        let topic_id = {
            let db = self.db().lock().await;
            match get_topic_by_harbor_id(&db, &leave.harbor_id) {
                Ok(Some(topic)) => topic.topic_id,
                Ok(None) => {
                    warn!(harbor_id = %hex::encode(&leave.harbor_id[..8]), "unknown topic for leave");
                    return ControlAck::failure(leave.message_id, "unknown topic");
                }
                Err(e) => {
                    warn!(error = %e, "failed to resolve topic from harbor_id");
                    return ControlAck::failure(leave.message_id, "internal error");
                }
            }
        };

        // Verify membership proof
        if !verify_membership_proof(&topic_id, &leave.harbor_id, &sender_id, &leave.membership_proof) {
            warn!("topic leave invalid membership proof");
            return ControlAck::failure(leave.message_id, "invalid membership proof");
        }

        // Remove member from topic
        {
            let db = self.db().lock().await;
            if let Err(e) = crate::data::remove_topic_member(&db, &topic_id, &sender_id) {
                warn!(error = %e, "failed to remove topic member");
            }
        }

        // Update connection gate
        if let Some(gate) = self.connection_gate() {
            gate.remove_topic_peer(&sender_id, &topic_id).await;
        }

        info!(
            member = %hex::encode(&sender_id[..8]),
            topic = %hex::encode(&topic_id[..8]),
            epoch = leave.epoch,
            "topic member left"
        );

        // Emit event
        let event = ProtocolEvent::TopicMemberLeft(TopicMemberLeftEvent {
            topic_id,
            member_id: sender_id,
        });
        let _ = self.event_tx().send(event).await;

        ControlAck::success(leave.message_id)
    }

    /// Handle an incoming RemoveMember (admin epoch rotation)
    pub async fn handle_remove_member(
        &self,
        remove: &RemoveMember,
        sender_id: [u8; 32],
    ) -> ControlAck {
        // Verify PoW first
        if let Err(e) = self.verify_pow(&remove.pow, &sender_id, ControlPacketType::RemoveMember) {
            warn!(
                sender = %hex::encode(&sender_id[..8]),
                error = %e,
                "remove member rejected: insufficient PoW"
            );
            return ControlAck::failure(remove.message_id, &e.to_string());
        }

        // Verify sender
        if !verify_sender(&remove.sender_id, &sender_id) {
            warn!("remove member sender mismatch");
            return ControlAck::failure(remove.message_id, "sender mismatch");
        }

        // Resolve topic_id from harbor_id
        let topic_id = {
            let db = self.db().lock().await;
            match get_topic_by_harbor_id(&db, &remove.harbor_id) {
                Ok(Some(topic)) => topic.topic_id,
                Ok(None) => {
                    warn!(harbor_id = %hex::encode(&remove.harbor_id[..8]), "unknown topic for remove member");
                    return ControlAck::failure(remove.message_id, "unknown topic");
                }
                Err(e) => {
                    warn!(error = %e, "failed to resolve topic from harbor_id");
                    return ControlAck::failure(remove.message_id, "internal error");
                }
            }
        };

        // Verify membership proof
        if !verify_membership_proof(&topic_id, &remove.harbor_id, &sender_id, &remove.membership_proof) {
            warn!("remove member invalid membership proof");
            return ControlAck::failure(remove.message_id, "invalid membership proof");
        }

        // Verify sender is admin of the topic
        {
            let db = self.db().lock().await;
            match is_topic_admin(&db, &topic_id, &sender_id) {
                Ok(true) => {}
                Ok(false) => {
                    warn!(
                        sender = %hex::encode(&sender_id[..8]),
                        topic = %hex::encode(&topic_id[..8]),
                        "remove member rejected: sender is not admin"
                    );
                    return ControlAck::failure(remove.message_id, "not authorized");
                }
                Err(e) => {
                    warn!(error = %e, "failed to check admin status");
                    return ControlAck::failure(remove.message_id, "internal error");
                }
            }
        }

        // Remove the member from our local membership
        {
            let db = self.db().lock().await;
            if let Err(e) = crate::data::remove_topic_member(&db, &topic_id, &remove.removed_member) {
                warn!(error = %e, "failed to remove topic member");
            }
        }

        // Update connection gate
        if let Some(gate) = self.connection_gate() {
            gate.remove_topic_peer(&remove.removed_member, &topic_id).await;
        }

        // Store the new epoch key
        {
            let db = self.db().lock().await;
            if let Err(e) = store_epoch_key(&db, &topic_id, remove.new_epoch, &remove.new_epoch_key) {
                warn!(error = %e, "failed to store new epoch key");
            }
        }

        info!(
            admin = %hex::encode(&sender_id[..8]),
            topic = %hex::encode(&topic_id[..8]),
            removed = %hex::encode(&remove.removed_member[..8]),
            new_epoch = remove.new_epoch,
            "received new epoch key after member removal"
        );

        // Emit event
        let event = ProtocolEvent::TopicEpochRotated(TopicEpochRotatedEvent {
            topic_id,
            new_epoch: remove.new_epoch,
            removed_member: remove.removed_member,
        });
        let _ = self.event_tx().send(event).await;

        ControlAck::success(remove.message_id)
    }
}
