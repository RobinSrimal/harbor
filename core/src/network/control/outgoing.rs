//! Outgoing control operations
//!
//! Implements all send_* methods for ControlService.
//! Handles direct delivery and stores packets for Harbor replication.

use tracing::{debug, info, warn};

use crate::data::{
    create_connect_token, delete_pending_invite, get_connection, get_current_epoch,
    get_current_epoch_key, get_pending_invite, get_topic, get_topic_admin, get_topic_members,
    is_topic_admin, store_epoch_key, store_outgoing_packet, subscribe_topic_with_admin,
    update_connection_state, upsert_connection, ConnectionState,
};
use crate::resilience::{ProofOfWork, PoWConfig, build_context};
use crate::security::harbor_id_from_topic;
use crate::network::membership::create_membership_proof;

use super::protocol::{
    ConnectAccept, ConnectDecline, ConnectRequest, ControlPacketType, RemoveMember, Suggest,
    TopicInvite, TopicJoin, TopicLeave,
};
use super::service::{generate_id, ConnectInvite, ControlError, ControlResult, ControlService};

/// Compute PoW for a control message
/// Context: sender_id || message_type_byte
fn compute_control_pow(sender_id: &[u8; 32], message_type: ControlPacketType) -> ControlResult<ProofOfWork> {
    let context = build_context(&[sender_id, &[message_type as u8]]);
    let difficulty = PoWConfig::control().base_difficulty;
    ProofOfWork::compute(&context, difficulty)
        .ok_or_else(|| ControlError::Rpc("failed to compute PoW".to_string()))
}

// =============================================================================
// Connection operations
// =============================================================================

/// Request a connection to a peer
pub async fn request_connection(
    service: &ControlService,
    peer_id: &[u8; 32],
    relay_url: Option<&str>,
    display_name: Option<&str>,
    token: Option<[u8; 32]>,
) -> ControlResult<[u8; 32]> {
    let request_id = generate_id();
    let our_relay = service.endpoint().addr().relay_urls().next().map(|u| u.to_string());
    let pow = compute_control_pow(&service.local_id(), ControlPacketType::ConnectRequest)?;

    let req = ConnectRequest {
        request_id,
        sender_id: service.local_id(),
        display_name: display_name.map(|s| s.to_string()),
        relay_url: our_relay.clone(),
        token,
        pow,
    };

    // Store pending outgoing connection
    {
        let db = service.db().lock().await;
        upsert_connection(
            &db,
            peer_id,
            ConnectionState::PendingOutgoing,
            None,
            relay_url,
            Some(&request_id),
        ).map_err(|e| ControlError::Database(e.to_string()))?;
    }

    // Try direct delivery
    match service.dial_peer(peer_id, relay_url).await {
        Ok(client) => {
            match client.rpc(req.clone()).await {
                Ok(ack) => {
                    if ack.success {
                        info!(
                            peer = %hex::encode(&peer_id[..8]),
                            request_id = %hex::encode(&request_id[..8]),
                            "connect request sent directly"
                        );
                    } else {
                        warn!(
                            peer = %hex::encode(&peer_id[..8]),
                            reason = ?ack.reason,
                            "connect request rejected"
                        );
                    }
                }
                Err(e) => {
                    debug!(
                        peer = %hex::encode(&peer_id[..8]),
                        error = %e,
                        "failed to send connect request directly, will replicate via harbor"
                    );
                }
            }
        }
        Err(e) => {
            debug!(
                peer = %hex::encode(&peer_id[..8]),
                error = %e,
                "peer offline, will replicate via harbor"
            );
        }
    }

    // Store for harbor replication (point-to-point: harbor_id = recipient_id)
    store_control_packet(
        service,
        &request_id,
        peer_id,           // harbor_id = recipient for point-to-point
        &[*peer_id],       // single recipient
        &postcard::to_allocvec(&req).unwrap(),
        ControlPacketType::ConnectRequest,
    )
    .await?;

    Ok(request_id)
}

/// Accept a connection request
pub async fn accept_connection(
    service: &ControlService,
    request_id: &[u8; 32],
) -> ControlResult<()> {
    // Find the pending incoming connection by request_id
    let (peer_id, relay_url) = {
        let db = service.db().lock().await;
        // We need to find which peer sent this request
        // For now, iterate all pending incoming connections
        let connections =
            crate::data::list_connections_by_state(&db, ConnectionState::PendingIncoming)
                .map_err(|e| ControlError::Database(e.to_string()))?;
        let conn = connections
            .into_iter()
            .find(|c| c.request_id == Some(*request_id))
            .ok_or(ControlError::RequestNotFound)?;
        (conn.peer_id, conn.relay_url)
    };

    // Update connection state
    {
        let db = service.db().lock().await;
        update_connection_state(&db, &peer_id, ConnectionState::Connected)
            .map_err(|e| ControlError::Database(e.to_string()))?;
    }

    // Update connection gate
    if let Some(gate) = service.connection_gate() {
        gate.mark_dm_connected(&peer_id).await;
    }

    // Send the accept message
    send_connect_accept(service, &peer_id, relay_url.as_deref(), request_id).await
}

/// Send a ConnectAccept message to a peer
///
/// Used by both `accept_connection` (manual accept) and the auto-accept flow
/// when a valid token is provided in the connect request.
pub async fn send_connect_accept(
    service: &ControlService,
    peer_id: &[u8; 32],
    peer_relay_url: Option<&str>,
    request_id: &[u8; 32],
) -> ControlResult<()> {
    let our_relay = service.endpoint().addr().relay_urls().next().map(|u| u.to_string());
    let pow = compute_control_pow(&service.local_id(), ControlPacketType::ConnectAccept)?;

    let accept = ConnectAccept {
        request_id: *request_id,
        sender_id: service.local_id(),
        display_name: None, // Could be set from local config
        relay_url: our_relay.clone(),
        pow,
    };

    // Try direct delivery
    if let Ok(client) = service.dial_peer(peer_id, peer_relay_url).await {
        if let Ok(ack) = client.rpc(accept.clone()).await {
            if ack.success {
                info!(
                    peer = %hex::encode(&peer_id[..8]),
                    request_id = %hex::encode(&request_id[..8]),
                    "connect accept sent directly"
                );
            }
        }
    }

    // Store for harbor replication
    store_control_packet(
        service,
        request_id,
        peer_id, // harbor_id = recipient
        &[*peer_id],
        &postcard::to_allocvec(&accept).unwrap(),
        ControlPacketType::ConnectAccept,
    )
    .await?;

    Ok(())
}

/// Decline a connection request
pub async fn decline_connection(
    service: &ControlService,
    request_id: &[u8; 32],
    reason: Option<&str>,
) -> ControlResult<()> {
    // Find the pending incoming connection
    let (peer_id, relay_url) = {
        let db = service.db().lock().await;
        let connections =
            crate::data::list_connections_by_state(&db, ConnectionState::PendingIncoming)
                .map_err(|e| ControlError::Database(e.to_string()))?;
        let conn = connections
            .into_iter()
            .find(|c| c.request_id == Some(*request_id))
            .ok_or(ControlError::RequestNotFound)?;
        (conn.peer_id, conn.relay_url)
    };

    let pow = compute_control_pow(&service.local_id(), ControlPacketType::ConnectDecline)?;
    let decline = ConnectDecline {
        request_id: *request_id,
        sender_id: service.local_id(),
        reason: reason.map(|s| s.to_string()),
        pow,
    };

    // Update connection state
    {
        let db = service.db().lock().await;
        update_connection_state(&db, &peer_id, ConnectionState::Declined)
            .map_err(|e| ControlError::Database(e.to_string()))?;
    }

    // Try direct delivery
    if let Ok(client) = service.dial_peer(&peer_id, relay_url.as_deref()).await {
        let _ = client.rpc(decline.clone()).await;
    }

    // Store for harbor replication
    store_control_packet(
        service,
        request_id,
        &peer_id,
        &[peer_id],
        &postcard::to_allocvec(&decline).unwrap(),
        ControlPacketType::ConnectDecline,
    )
    .await?;

    Ok(())
}

/// Generate a connect token (QR code / invite string)
pub async fn generate_connect_token(service: &ControlService) -> ControlResult<ConnectInvite> {
    let token = generate_id();

    // Store token
    {
        let db = service.db().lock().await;
        create_connect_token(&db, &token).map_err(|e| ControlError::Database(e.to_string()))?;
    }

    let invite = ConnectInvite {
        endpoint_id: service.local_id(),
        relay_url: service.endpoint().addr().relay_urls().next().map(|u| u.to_string()),
        token,
    };

    info!(
        token = %hex::encode(&token[..8]),
        "connect token generated"
    );

    Ok(invite)
}

/// Block a peer
pub async fn block_peer(service: &ControlService, peer_id: &[u8; 32]) -> ControlResult<()> {
    {
        let db = service.db().lock().await;
        update_connection_state(&db, peer_id, ConnectionState::Blocked)
            .map_err(|e| ControlError::Database(e.to_string()))?;
    }

    // Update connection gate
    if let Some(gate) = service.connection_gate() {
        gate.mark_dm_blocked(peer_id).await;
    }

    info!(peer = %hex::encode(&peer_id[..8]), "peer blocked");
    Ok(())
}

/// Unblock a peer
pub async fn unblock_peer(service: &ControlService, peer_id: &[u8; 32]) -> ControlResult<()> {
    let was_blocked = {
        let db = service.db().lock().await;
        // Unblocking moves back to connected (or pending if was pending)
        if let Ok(Some(conn)) = get_connection(&db, peer_id) {
            if conn.state == ConnectionState::Blocked {
                update_connection_state(&db, peer_id, ConnectionState::Connected)
                    .map_err(|e| ControlError::Database(e.to_string()))?;
                true
            } else {
                false
            }
        } else {
            false
        }
    };

    if was_blocked {
        // Update connection gate
        if let Some(gate) = service.connection_gate() {
            gate.mark_dm_connected(peer_id).await;
        }
        info!(peer = %hex::encode(&peer_id[..8]), "peer unblocked");
    }
    Ok(())
}

// =============================================================================
// Topic operations
// =============================================================================

/// Invite a peer to a topic
pub async fn invite_to_topic(
    service: &ControlService,
    peer_id: &[u8; 32],
    topic_id: &[u8; 32],
) -> ControlResult<[u8; 32]> {
    let message_id = generate_id();

    // Get topic info, admin, and current epoch key
    // Topics always have at least epoch 0 key (stored at creation time)
    let (epoch, epoch_key, admin_id, members) = {
        let db = service.db().lock().await;
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

    let pow = compute_control_pow(&service.local_id(), ControlPacketType::TopicInvite)?;
    let invite = TopicInvite {
        message_id,
        sender_id: service.local_id(),
        topic_id: *topic_id,
        topic_name: None, // TODO: Store topic name in database
        epoch,
        epoch_key,
        admin_id,
        members,
        pow,
    };

    // Try direct delivery
    if let Ok(client) = service.dial_peer(peer_id, None).await {
        if let Ok(ack) = client.rpc(invite.clone()).await {
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
    store_control_packet(
        service,
        &message_id,
        peer_id,
        &[*peer_id],
        &postcard::to_allocvec(&invite).unwrap(),
        ControlPacketType::TopicInvite,
    )
    .await?;

    Ok(message_id)
}

/// Accept a topic invite
pub async fn accept_topic_invite(
    service: &ControlService,
    message_id: &[u8; 32],
) -> ControlResult<()> {
    // Get the pending invite
    let invite = {
        let db = service.db().lock().await;
        get_pending_invite(&db, message_id)
            .map_err(|e| ControlError::Database(e.to_string()))?
            .ok_or(ControlError::RequestNotFound)?
    };

    // Subscribe to topic and add members
    {
        let db = service.db().lock().await;
        // Subscribe with the admin from the invite
        subscribe_topic_with_admin(&db, &invite.topic_id, &invite.admin_id)
            .map_err(|e| ControlError::Database(e.to_string()))?;
        // Add ourselves as a member (required for get_topics_for_member to find this topic)
        let _ = crate::data::add_topic_member(&db, &invite.topic_id, &service.local_id());
        // Add other members from the invite
        for member in &invite.members {
            let _ = crate::data::add_topic_member(&db, &invite.topic_id, member);
        }
        // Store the epoch key from the invite
        store_epoch_key(&db, &invite.topic_id, invite.epoch, &invite.epoch_key)
            .map_err(|e| ControlError::Database(e.to_string()))?;
    }

    // Update connection gate with all topic peers
    if let Some(gate) = service.connection_gate() {
        gate.add_topic_peers(&invite.topic_id, &invite.members).await;
    }

    // Send TopicJoin to all members
    let join_message_id = generate_id();
    let our_relay = service.endpoint().addr().relay_urls().next().map(|u| u.to_string());

    let harbor_id = harbor_id_from_topic(&invite.topic_id);
    let membership_proof = create_membership_proof(
        &invite.topic_id,
        &harbor_id,
        &service.local_id(),
    );
    let pow = compute_control_pow(&service.local_id(), ControlPacketType::TopicJoin)?;

    let join = TopicJoin {
        message_id: join_message_id,
        harbor_id,
        sender_id: service.local_id(),
        epoch: invite.epoch,
        relay_url: our_relay,
        membership_proof,
        pow,
    };

    // Try direct delivery to all members
    for member in &invite.members {
        if *member == service.local_id() {
            continue;
        }
        if let Ok(client) = service.dial_peer(member, None).await {
            let _ = client.rpc(join.clone()).await;
        }
    }

    // Store for harbor replication (topic-scoped: harbor_id = hash(topic_id))
    store_control_packet(
        service,
        &join_message_id,
        &harbor_id,
        &invite.members,
        &postcard::to_allocvec(&join).unwrap(),
        ControlPacketType::TopicJoin,
    )
    .await?;

    // Delete the pending invite
    {
        let db = service.db().lock().await;
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
    service: &ControlService,
    message_id: &[u8; 32],
) -> ControlResult<()> {
    // Just delete the pending invite
    let db = service.db().lock().await;
    delete_pending_invite(&db, message_id).map_err(|e| ControlError::Database(e.to_string()))?;

    info!(message_id = %hex::encode(&message_id[..8]), "declined topic invite");
    Ok(())
}

/// Leave a topic
pub async fn leave_topic(service: &ControlService, topic_id: &[u8; 32]) -> ControlResult<()> {
    let message_id = generate_id();

    // Get current members and epoch before leaving
    let (members, epoch) = {
        let db = service.db().lock().await;
        let members = get_topic_members(&db, topic_id).map_err(|e| ControlError::Database(e.to_string()))?;
        let epoch = get_current_epoch(&db, topic_id)
            .map_err(|e| ControlError::Database(e.to_string()))?
            .unwrap_or(1);
        (members, epoch)
    };

    let harbor_id = harbor_id_from_topic(topic_id);
    let membership_proof = create_membership_proof(topic_id, &harbor_id, &service.local_id());
    let pow = compute_control_pow(&service.local_id(), ControlPacketType::TopicLeave)?;

    let leave = TopicLeave {
        message_id,
        harbor_id,
        sender_id: service.local_id(),
        epoch,
        membership_proof,
        pow,
    };

    // Try direct delivery to all members
    for member in &members {
        if *member == service.local_id() {
            continue;
        }
        if let Ok(client) = service.dial_peer(member, None).await {
            let _ = client.rpc(leave.clone()).await;
        }
    }

    // Store for harbor replication (topic-scoped)
    store_control_packet(
        service,
        &message_id,
        &harbor_id,
        &members,
        &postcard::to_allocvec(&leave).unwrap(),
        ControlPacketType::TopicLeave,
    )
    .await?;

    // Unsubscribe from topic
    {
        let db = service.db().lock().await;
        crate::data::unsubscribe_topic(&db, topic_id)
            .map_err(|e| ControlError::Database(e.to_string()))?;
    }

    // Update connection gate - remove this topic
    if let Some(gate) = service.connection_gate() {
        gate.remove_topic(topic_id).await;
    }

    info!(topic = %hex::encode(&topic_id[..8]), "left topic");
    Ok(())
}

/// Remove a member from a topic (admin only)
pub async fn remove_topic_member(
    service: &ControlService,
    topic_id: &[u8; 32],
    member_id: &[u8; 32],
) -> ControlResult<()> {
    // Verify we are admin and get topic members
    let (members, current_epoch) = {
        let db = service.db().lock().await;
        let _topic =
            get_topic(&db, topic_id).map_err(|e| ControlError::Database(e.to_string()))?;
        let _topic = _topic.ok_or(ControlError::TopicNotFound)?;

        // Verify we are the admin
        let our_id = service.local_id();
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
        if *member == service.local_id() {
            continue;
        }

        let message_id = generate_id();
        let harbor_id = harbor_id_from_topic(topic_id);
        let membership_proof =
            create_membership_proof(topic_id, &harbor_id, &service.local_id());
        let pow = compute_control_pow(&service.local_id(), ControlPacketType::RemoveMember)?;

        let remove = RemoveMember {
            message_id,
            harbor_id,
            sender_id: service.local_id(),
            removed_member: *member_id,
            new_epoch,
            new_epoch_key,
            membership_proof,
            pow,
        };

        // Try direct delivery
        if let Ok(client) = service.dial_peer(member, None).await {
            let _ = client.rpc(remove.clone()).await;
        }

        // Store for harbor replication (point-to-point: harbor_id = recipient)
        // Each RemoveMember goes to a specific member
        store_control_packet(
            service,
            &message_id,
            member, // harbor_id = this specific recipient
            &[*member],
            &postcard::to_allocvec(&remove).unwrap(),
            ControlPacketType::RemoveMember,
        )
        .await?;
    }

    // Remove member from our local membership and store new epoch key
    {
        let db = service.db().lock().await;
        crate::data::remove_topic_member(&db, topic_id, member_id)
            .map_err(|e| ControlError::Database(e.to_string()))?;
        // Store the new epoch key
        store_epoch_key(&db, topic_id, new_epoch, &new_epoch_key)
            .map_err(|e| ControlError::Database(e.to_string()))?;
    }

    // Update connection gate
    if let Some(gate) = service.connection_gate() {
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

// =============================================================================
// Introduction operations
// =============================================================================

/// Suggest a peer to another peer
pub async fn suggest_peer(
    service: &ControlService,
    to_peer: &[u8; 32],
    suggested_peer: &[u8; 32],
    note: Option<&str>,
) -> ControlResult<[u8; 32]> {
    let message_id = generate_id();

    // Get suggested peer's relay URL from our database
    let relay_url = {
        let db = service.db().lock().await;
        crate::data::get_peer_relay_info(&db, suggested_peer)
            .ok()
            .flatten()
            .map(|(url, _)| url)
    };

    let pow = compute_control_pow(&service.local_id(), ControlPacketType::Suggest)?;
    let suggest = Suggest {
        message_id,
        sender_id: service.local_id(),
        suggested_peer: *suggested_peer,
        relay_url,
        note: note.map(|s| s.to_string()),
        pow,
    };

    // Try direct delivery
    if let Ok(client) = service.dial_peer(to_peer, None).await {
        if let Ok(ack) = client.rpc(suggest.clone()).await {
            if ack.success {
                info!(
                    to = %hex::encode(&to_peer[..8]),
                    suggested = %hex::encode(&suggested_peer[..8]),
                    "peer suggestion sent directly"
                );
            }
        }
    }

    // Store for harbor replication (point-to-point)
    store_control_packet(
        service,
        &message_id,
        to_peer,
        &[*to_peer],
        &postcard::to_allocvec(&suggest).unwrap(),
        ControlPacketType::Suggest,
    )
    .await?;

    Ok(message_id)
}

// =============================================================================
// Helper functions
// =============================================================================

/// Store a control packet for harbor replication
async fn store_control_packet(
    service: &ControlService,
    packet_id: &[u8; 32],
    harbor_id: &[u8; 32],
    recipients: &[[u8; 32]],
    packet_data: &[u8],
    packet_type: ControlPacketType,
) -> ControlResult<()> {
    let mut db = service.db().lock().await;

    // For point-to-point control packets, use harbor_id as topic_id
    // so the replication task seals them as DM packets (topic_id == harbor_id check).
    // For topic-scoped packets (TopicJoin/TopicLeave), harbor_id is hash(topic_id),
    // so they won't match and will be sealed as topic packets.
    let topic_id = *harbor_id;

    // Prepend type byte to packet data so receiver can identify Control packets.
    // DM packets use 0x80+ range, Control packets use their ControlPacketType value.
    let mut prefixed_data = Vec::with_capacity(1 + packet_data.len());
    prefixed_data.push(packet_type as u8);
    prefixed_data.extend_from_slice(packet_data);

    store_outgoing_packet(
        &mut db,
        packet_id,
        &topic_id,
        harbor_id,
        &prefixed_data,
        recipients,
        packet_type as u8,
    )
    .map_err(|e| ControlError::Database(e.to_string()))?;

    debug!(
        packet_id = %hex::encode(&packet_id[..8]),
        harbor_id = %hex::encode(&harbor_id[..8]),
        packet_type = ?packet_type,
        recipients = recipients.len(),
        "stored control packet for harbor replication"
    );

    Ok(())
}
