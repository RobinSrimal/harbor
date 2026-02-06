//! Incoming control message handling
//!
//! Processes incoming control messages from the control ALPN.
//! Emits protocol events and updates database state.

use tracing::{info, warn};

use crate::data::{
    consume_token, get_connection, is_peer_blocked, is_topic_admin,
    store_epoch_key, store_pending_invite, token_exists, update_connection_state,
    upsert_connection, ConnectionState,
};
use crate::data::membership::get_topic_by_harbor_id;
use crate::network::membership::verify_membership_proof;
use crate::protocol::{
    ConnectionAcceptedEvent, ConnectionDeclinedEvent, ConnectionRequestEvent,
    PeerSuggestedEvent, ProtocolEvent, TopicInviteReceivedEvent, TopicMemberJoinedEvent,
    TopicMemberLeftEvent, TopicEpochRotatedEvent,
};

use super::protocol::{
    ControlAck, ControlPacketType,
    ConnectAccept, ConnectDecline, ConnectRequest,
    RemoveMember, Suggest, TopicInvite, TopicJoin, TopicLeave,
};
use super::service::ControlService;

/// Verify sender_id matches the QUIC connection peer
fn verify_sender(claimed_sender: &[u8; 32], quic_sender: &[u8; 32]) -> bool {
    claimed_sender == quic_sender
}

// =============================================================================
// Connection handlers
// =============================================================================

pub async fn handle_connect_request(
    service: &ControlService,
    req: &ConnectRequest,
    sender_id: [u8; 32],
) -> ControlAck {
    // Verify PoW first
    if let Err(e) = service.verify_pow(&req.pow, &sender_id, ControlPacketType::ConnectRequest) {
        warn!(
            sender = %hex::encode(&sender_id[..8]),
            error = %e,
            "connect request rejected: insufficient PoW"
        );
        return ControlAck::failure(req.request_id, &e.to_string());
    }

    // Verify sender
    if !verify_sender(&req.sender_id, &sender_id) {
        warn!(
            claimed = %hex::encode(&req.sender_id[..8]),
            actual = %hex::encode(&sender_id[..8]),
            "connect request sender mismatch"
        );
        return ControlAck::failure(req.request_id, "sender mismatch");
    }

    // Check if peer is blocked
    let is_blocked = {
        let db = service.db().lock().await;
        is_peer_blocked(&db, &sender_id).unwrap_or(false)
    };
    if is_blocked {
        info!(peer = %hex::encode(&sender_id[..8]), "rejected connect request from blocked peer");
        return ControlAck::failure(req.request_id, "blocked");
    }

    // Check for valid token (auto-accept flow)
    let auto_accept = if let Some(token) = req.token {
        let db = service.db().lock().await;
        if token_exists(&db, &token).unwrap_or(false) {
            // Consume the token
            consume_token(&db, &token).ok();
            true
        } else {
            info!(peer = %hex::encode(&sender_id[..8]), "rejected connect request with invalid token");
            return ControlAck::failure(req.request_id, "invalid token");
        }
    } else {
        false
    };

    // Store connection info
    {
        let db = service.db().lock().await;
        let state = if auto_accept {
            ConnectionState::Connected
        } else {
            ConnectionState::PendingIncoming
        };
        if let Err(e) = upsert_connection(
            &db,
            &sender_id,
            state,
            req.display_name.as_deref(),
            req.relay_url.as_deref(),
            Some(&req.request_id),
        ) {
            warn!(error = %e, "failed to store connection");
            return ControlAck::failure(req.request_id, "internal error");
        }
    }

    if auto_accept {
        info!(
            peer = %hex::encode(&sender_id[..8]),
            request_id = %hex::encode(&req.request_id[..8]),
            "connection auto-accepted via token"
        );

        // Update connection gate
        if let Some(gate) = service.connection_gate() {
            gate.mark_dm_connected(&sender_id).await;
        }

        // Send ConnectAccept back to the requester
        if let Err(e) = super::outgoing::send_connect_accept(
            service,
            &sender_id,
            req.relay_url.as_deref(),
            &req.request_id,
        ).await {
            warn!(error = %e, "failed to send connect accept");
        }

        // Emit accepted event
        let event = ProtocolEvent::ConnectionAccepted(ConnectionAcceptedEvent {
            peer_id: sender_id,
            request_id: req.request_id,
        });
        let _ = service.event_tx().send(event).await;
    } else {
        info!(
            peer = %hex::encode(&sender_id[..8]),
            request_id = %hex::encode(&req.request_id[..8]),
            "connection request received"
        );

        // Emit request event for app to decide
        let event = ProtocolEvent::ConnectionRequest(ConnectionRequestEvent {
            peer_id: sender_id,
            request_id: req.request_id,
            display_name: req.display_name.clone(),
            relay_url: req.relay_url.clone(),
        });
        let _ = service.event_tx().send(event).await;
    }

    ControlAck::success(req.request_id)
}

pub async fn handle_connect_accept(
    service: &ControlService,
    accept: &ConnectAccept,
    sender_id: [u8; 32],
) -> ControlAck {
    // Verify PoW first
    if let Err(e) = service.verify_pow(&accept.pow, &sender_id, ControlPacketType::ConnectAccept) {
        warn!(
            sender = %hex::encode(&sender_id[..8]),
            error = %e,
            "connect accept rejected: insufficient PoW"
        );
        return ControlAck::failure(accept.request_id, &e.to_string());
    }

    // Verify sender
    if !verify_sender(&accept.sender_id, &sender_id) {
        warn!("connect accept sender mismatch");
        return ControlAck::failure(accept.request_id, "sender mismatch");
    }

    // Update our outgoing request state
    {
        let db = service.db().lock().await;
        // Check if we have a pending outgoing request for this peer
        if let Ok(Some(conn)) = get_connection(&db, &sender_id) {
            if conn.state == ConnectionState::PendingOutgoing
                && conn.request_id == Some(accept.request_id)
            {
                // Update to connected, store their display name and relay URL
                if let Err(e) = upsert_connection(
                    &db,
                    &sender_id,
                    ConnectionState::Connected,
                    accept.display_name.as_deref(),
                    accept.relay_url.as_deref(),
                    Some(&accept.request_id),
                ) {
                    warn!(error = %e, "failed to update connection state");
                    return ControlAck::failure(accept.request_id, "internal error");
                }
            } else {
                return ControlAck::failure(accept.request_id, "no pending request");
            }
        } else {
            return ControlAck::failure(accept.request_id, "no pending request");
        }
    }

    // Update connection gate
    if let Some(gate) = service.connection_gate() {
        gate.mark_dm_connected(&sender_id).await;
    }

    info!(
        peer = %hex::encode(&sender_id[..8]),
        request_id = %hex::encode(&accept.request_id[..8]),
        "connection accepted"
    );

    // Emit accepted event
    let event = ProtocolEvent::ConnectionAccepted(ConnectionAcceptedEvent {
        peer_id: sender_id,
        request_id: accept.request_id,
    });
    let _ = service.event_tx().send(event).await;

    ControlAck::success(accept.request_id)
}

pub async fn handle_connect_decline(
    service: &ControlService,
    decline: &ConnectDecline,
    sender_id: [u8; 32],
) -> ControlAck {
    // Verify PoW first
    if let Err(e) = service.verify_pow(&decline.pow, &sender_id, ControlPacketType::ConnectDecline) {
        warn!(
            sender = %hex::encode(&sender_id[..8]),
            error = %e,
            "connect decline rejected: insufficient PoW"
        );
        return ControlAck::failure(decline.request_id, &e.to_string());
    }

    // Verify sender
    if !verify_sender(&decline.sender_id, &sender_id) {
        warn!("connect decline sender mismatch");
        return ControlAck::failure(decline.request_id, "sender mismatch");
    }

    // Update our outgoing request state
    {
        let db = service.db().lock().await;
        if let Ok(Some(conn)) = get_connection(&db, &sender_id) {
            if conn.state == ConnectionState::PendingOutgoing
                && conn.request_id == Some(decline.request_id)
            {
                let _ = update_connection_state(&db, &sender_id, ConnectionState::Declined);
            }
        }
    }

    info!(
        peer = %hex::encode(&sender_id[..8]),
        request_id = %hex::encode(&decline.request_id[..8]),
        reason = ?decline.reason,
        "connection declined"
    );

    // Emit declined event
    let event = ProtocolEvent::ConnectionDeclined(ConnectionDeclinedEvent {
        peer_id: sender_id,
        request_id: decline.request_id,
        reason: decline.reason.clone(),
    });
    let _ = service.event_tx().send(event).await;

    ControlAck::success(decline.request_id)
}

// =============================================================================
// Topic handlers
// =============================================================================

pub async fn handle_topic_invite(
    service: &ControlService,
    invite: &TopicInvite,
    sender_id: [u8; 32],
) -> ControlAck {
    // Verify PoW first
    if let Err(e) = service.verify_pow(&invite.pow, &sender_id, ControlPacketType::TopicInvite) {
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
        let db = service.db().lock().await;
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
    let _ = service.event_tx().send(event).await;

    ControlAck::success(invite.message_id)
}

pub async fn handle_topic_join(
    service: &ControlService,
    join: &TopicJoin,
    sender_id: [u8; 32],
) -> ControlAck {
    // Verify PoW first
    if let Err(e) = service.verify_pow(&join.pow, &sender_id, ControlPacketType::TopicJoin) {
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
        let db = service.db().lock().await;
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
        let db = service.db().lock().await;
        if let Err(e) = crate::data::add_topic_member(&db, &topic_id, &sender_id) {
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
    if let Some(gate) = service.connection_gate() {
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
    let _ = service.event_tx().send(event).await;

    ControlAck::success(join.message_id)
}

pub async fn handle_topic_leave(
    service: &ControlService,
    leave: &TopicLeave,
    sender_id: [u8; 32],
) -> ControlAck {
    // Verify PoW first
    if let Err(e) = service.verify_pow(&leave.pow, &sender_id, ControlPacketType::TopicLeave) {
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
        let db = service.db().lock().await;
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
        let db = service.db().lock().await;
        if let Err(e) = crate::data::remove_topic_member(&db, &topic_id, &sender_id) {
            warn!(error = %e, "failed to remove topic member");
        }
    }

    // Update connection gate
    if let Some(gate) = service.connection_gate() {
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
    let _ = service.event_tx().send(event).await;

    ControlAck::success(leave.message_id)
}

pub async fn handle_remove_member(
    service: &ControlService,
    remove: &RemoveMember,
    sender_id: [u8; 32],
) -> ControlAck {
    // Verify PoW first
    if let Err(e) = service.verify_pow(&remove.pow, &sender_id, ControlPacketType::RemoveMember) {
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
        let db = service.db().lock().await;
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
        let db = service.db().lock().await;
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
        let db = service.db().lock().await;
        if let Err(e) = crate::data::remove_topic_member(&db, &topic_id, &remove.removed_member) {
            warn!(error = %e, "failed to remove topic member");
        }
    }

    // Update connection gate
    if let Some(gate) = service.connection_gate() {
        gate.remove_topic_peer(&remove.removed_member, &topic_id).await;
    }

    // Store the new epoch key
    {
        let db = service.db().lock().await;
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
    let _ = service.event_tx().send(event).await;

    ControlAck::success(remove.message_id)
}

// =============================================================================
// Introduction handlers
// =============================================================================

pub async fn handle_suggest(
    service: &ControlService,
    suggest: &Suggest,
    sender_id: [u8; 32],
) -> ControlAck {
    // Verify PoW first
    if let Err(e) = service.verify_pow(&suggest.pow, &sender_id, ControlPacketType::Suggest) {
        warn!(
            sender = %hex::encode(&sender_id[..8]),
            error = %e,
            "suggest rejected: insufficient PoW"
        );
        return ControlAck::failure(suggest.message_id, &e.to_string());
    }

    // Verify sender
    if !verify_sender(&suggest.sender_id, &sender_id) {
        warn!("suggest sender mismatch");
        return ControlAck::failure(suggest.message_id, "sender mismatch");
    }

    info!(
        introducer = %hex::encode(&sender_id[..8]),
        suggested = %hex::encode(&suggest.suggested_peer[..8]),
        note = ?suggest.note,
        "peer suggestion received"
    );

    // Emit event for app to decide
    let event = ProtocolEvent::PeerSuggested(PeerSuggestedEvent {
        introducer_id: sender_id,
        suggested_peer_id: suggest.suggested_peer,
        relay_url: suggest.relay_url.clone(),
        note: suggest.note.clone(),
    });
    let _ = service.event_tx().send(event).await;

    ControlAck::success(suggest.message_id)
}
