//! Unified packet processing
//!
//! Single entry point for processing decrypted packets based on their PacketType.
//! This module provides flat dispatch on PacketType, with context-aware handling.
//!
//! Used by:
//! - HarborService (handles harbor pull)
//! - SendService (handles direct delivery)

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use crate::network::packet::{PacketType, EncryptionKeyType};
use crate::network::membership::verify_membership_proof;
use crate::security::harbor_id_from_topic;
use crate::data::membership::get_topic_by_harbor_id;
use crate::network::stream::StreamService;
use crate::network::send::PacketSource;
use crate::protocol::ProtocolEvent;

/// Context for packet processing
///
/// Contains all the dependencies needed to process any packet type.
pub struct ProcessContext {
    /// Database connection
    pub db: Arc<Mutex<Connection>>,
    /// Event sender for emitting protocol events
    pub event_tx: mpsc::Sender<ProtocolEvent>,
    /// Our endpoint ID
    pub our_id: [u8; 32],
    /// Stream service for handling stream signaling
    pub stream_service: Arc<StreamService>,
    /// Scope context - set based on which harbor_id we pulled from
    pub scope: ProcessScope,
}

/// Scope of packet processing - determines how topic_id is resolved
#[derive(Clone)]
pub enum ProcessScope {
    /// Topic-scoped: we know the topic_id (pulled from topic harbor)
    Topic { topic_id: [u8; 32] },
    /// DM-scoped: point-to-point, no topic context
    Dm,
}

/// Errors that can occur during packet processing
#[derive(Debug)]
pub enum ProcessError {
    /// Failed to decode packet payload
    Decode(String),
    /// Database operation failed
    Database(String),
    /// Sender validation failed
    InvalidSender { expected: String, actual: String },
    /// Topic context required but not available
    MissingTopicContext,
    /// Packet type doesn't match the processing scope
    UnexpectedScope(PacketType, &'static str),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(msg) => write!(f, "decode error: {}", msg),
            Self::Database(msg) => write!(f, "database error: {}", msg),
            Self::InvalidSender { expected, actual } => {
                write!(f, "invalid sender: expected {}, got {}", expected, actual)
            }
            Self::MissingTopicContext => write!(f, "missing topic context for topic-scoped packet"),
            Self::UnexpectedScope(pt, scope) => {
                write!(f, "unexpected packet type {:?} in scope {}", pt, scope)
            }
        }
    }
}

impl std::error::Error for ProcessError {}

/// Process a decrypted packet based on its type
///
/// The `plaintext` includes the type byte at position 0.
/// Returns Ok(()) on successful processing, or an error if processing failed.
///
/// # Arguments
/// * `plaintext` - Full decrypted payload including type byte at [0]
/// * `sender_id` - The verified sender of the packet
/// * `timestamp` - When the packet was created
/// * `ctx` - Processing context with dependencies
pub async fn process_packet(
    plaintext: &[u8],
    sender_id: [u8; 32],
    timestamp: i64,
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    if plaintext.is_empty() {
        return Err(ProcessError::Decode("empty plaintext".into()));
    }

    let type_byte = plaintext[0];
    let payload = &plaintext[1..];

    let packet_type = PacketType::from_byte(type_byte)
        .ok_or_else(|| ProcessError::Decode(format!("unknown packet type byte: 0x{:02x}", type_byte)))?;

    // Validate scope matches packet type's expected encryption key
    match (packet_type.encryption_key_type(), &ctx.scope) {
        (EncryptionKeyType::TopicKey, ProcessScope::Dm) => {
            // TopicJoin/TopicLeave can arrive via topic harbor, so they use TopicKey
            // but other topic-scoped packets shouldn't arrive in DM context
            if !matches!(packet_type, PacketType::TopicJoin | PacketType::TopicLeave) {
                return Err(ProcessError::UnexpectedScope(packet_type, "DM"));
            }
        }
        (EncryptionKeyType::DmKey, ProcessScope::Topic { .. }) => {
            // DM-keyed packets shouldn't arrive via topic harbor
            // (except TopicInvite which goes to recipient's endpoint)
            return Err(ProcessError::UnexpectedScope(packet_type, "Topic"));
        }
        _ => {}
    }

    match packet_type {
        // =========================================================================
        // Topic-scoped messages (0x00-0x05)
        // =========================================================================
        PacketType::TopicContent => {
            process_topic_content(payload, sender_id, timestamp, ctx).await
        }
        PacketType::TopicFileAnnounce => {
            process_topic_file_announce(payload, sender_id, ctx).await
        }
        PacketType::TopicCanSeed => {
            process_topic_can_seed(payload, sender_id, ctx).await
        }
        PacketType::TopicSyncUpdate => {
            process_topic_sync_update(payload, sender_id, ctx).await
        }
        PacketType::TopicSyncRequest => {
            process_topic_sync_request(sender_id, ctx).await
        }
        PacketType::TopicStreamRequest => {
            process_topic_stream_request(plaintext, sender_id, ctx).await
        }

        // =========================================================================
        // DM-scoped messages (0x40-0x44)
        // =========================================================================
        PacketType::DmContent => {
            process_dm_content(payload, sender_id, timestamp, ctx).await
        }
        PacketType::DmFileAnnounce => {
            process_dm_file_announce(payload, sender_id, timestamp, ctx).await
        }
        PacketType::DmSyncUpdate => {
            process_dm_sync_update(payload, sender_id, ctx).await
        }
        PacketType::DmSyncRequest => {
            process_dm_sync_request(sender_id, ctx).await
        }
        PacketType::DmStreamRequest => {
            process_dm_stream_request(plaintext, sender_id, ctx).await
        }

        // =========================================================================
        // Stream signaling (0x50-0x54)
        // =========================================================================
        PacketType::StreamAccept
        | PacketType::StreamReject
        | PacketType::StreamQuery
        | PacketType::StreamActive
        | PacketType::StreamEnded => {
            process_stream_signaling(plaintext, sender_id, ctx).await
        }

        // =========================================================================
        // Control messages (0x80-0x87)
        // =========================================================================
        PacketType::ConnectRequest => {
            process_connect_request(payload, ctx).await
        }
        PacketType::ConnectAccept => {
            process_connect_accept(payload, ctx).await
        }
        PacketType::ConnectDecline => {
            process_connect_decline(payload, ctx).await
        }
        PacketType::TopicInvite => {
            process_topic_invite(payload, sender_id, ctx).await
        }
        PacketType::TopicJoin => {
            process_topic_join(payload, sender_id, ctx).await
        }
        PacketType::TopicLeave => {
            process_topic_leave(payload, sender_id, ctx).await
        }
        PacketType::RemoveMember => {
            process_remove_member(payload, sender_id, ctx).await
        }
        PacketType::Suggest => {
            process_suggest(payload, ctx).await
        }
    }
}

// =============================================================================
// Topic message handlers
// =============================================================================

async fn process_topic_content(
    payload: &[u8],
    sender_id: [u8; 32],
    timestamp: i64,
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    let topic_id = get_topic_id(ctx)?;

    let event = ProtocolEvent::Message(crate::protocol::IncomingMessage {
        topic_id,
        sender_id,
        payload: payload.to_vec(),
        timestamp,
    });

    let _ = ctx.event_tx.send(event).await;
    Ok(())
}

async fn process_topic_file_announce(
    payload: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::packet::FileAnnouncementMessage;
    use crate::data::{get_blob, insert_blob, init_blob_sections, CHUNK_SIZE};

    let topic_id = get_topic_id(ctx)?;

    let ann: FileAnnouncementMessage = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("FileAnnouncement: {}", e)))?;

    // Validate source matches packet sender
    if ann.source_id != sender_id {
        return Err(ProcessError::InvalidSender {
            expected: hex::encode(&ann.source_id[..8]),
            actual: hex::encode(&sender_id[..8]),
        });
    }

    let db_lock = ctx.db.lock().await;

    // Check if we already have this blob
    let existing = get_blob(&db_lock, &ann.hash);
    if existing.is_ok() && existing.as_ref().unwrap().is_none() {
        // Store blob metadata
        if let Err(e) = insert_blob(
            &db_lock,
            &ann.hash,
            &topic_id,
            &ann.source_id,
            &ann.display_name,
            ann.total_size,
            ann.num_sections,
        ) {
            warn!(error = %e, "failed to store blob metadata");
        } else {
            let total_chunks = ann.total_size.div_ceil(CHUNK_SIZE) as u32;
            let _ = init_blob_sections(&db_lock, &ann.hash, ann.num_sections, total_chunks);
        }
    }

    Ok(())
}

async fn process_topic_can_seed(
    payload: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::packet::CanSeedMessage;
    use crate::data::record_peer_can_seed;

    let can_seed: CanSeedMessage = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("CanSeed: {}", e)))?;

    // Validate seeder matches packet sender
    if can_seed.seeder_id != sender_id {
        return Err(ProcessError::InvalidSender {
            expected: hex::encode(&can_seed.seeder_id[..8]),
            actual: hex::encode(&sender_id[..8]),
        });
    }

    let db_lock = ctx.db.lock().await;
    let _ = record_peer_can_seed(&db_lock, &can_seed.hash, &can_seed.seeder_id);

    Ok(())
}

async fn process_topic_sync_update(
    payload: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::packet::SyncUpdateMessage;

    let topic_id = get_topic_id(ctx)?;

    let sync_update: SyncUpdateMessage = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("SyncUpdate: {}", e)))?;

    let event = ProtocolEvent::SyncUpdate(crate::protocol::SyncUpdateEvent {
        topic_id,
        sender_id,
        data: sync_update.data,
    });
    let _ = ctx.event_tx.send(event).await;

    Ok(())
}

async fn process_topic_sync_request(
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    let topic_id = get_topic_id(ctx)?;

    let event = ProtocolEvent::SyncRequest(crate::protocol::SyncRequestEvent {
        topic_id,
        sender_id,
    });
    let _ = ctx.event_tx.send(event).await;

    Ok(())
}

async fn process_topic_stream_request(
    plaintext: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::packet::TopicMessage;

    let topic_id = get_topic_id(ctx)?;

    let msg = TopicMessage::decode(plaintext)
        .map_err(|e| ProcessError::Decode(format!("TopicStreamRequest: {}", e)))?;

    ctx.stream_service
        .handle_signaling(&msg, &topic_id, sender_id, PacketSource::HarborPull)
        .await;

    Ok(())
}

// =============================================================================
// DM message handlers
// =============================================================================

async fn process_dm_content(
    payload: &[u8],
    sender_id: [u8; 32],
    timestamp: i64,
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    let event = ProtocolEvent::DmReceived(crate::protocol::DmReceivedEvent {
        sender_id,
        payload: payload.to_vec(),
        timestamp,
    });
    let _ = ctx.event_tx.send(event).await;

    Ok(())
}

async fn process_dm_file_announce(
    payload: &[u8],
    sender_id: [u8; 32],
    timestamp: i64,
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::packet::FileAnnouncementMessage;

    let msg: FileAnnouncementMessage = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("DmFileAnnounce: {}", e)))?;

    let event = ProtocolEvent::DmFileAnnounced(crate::protocol::DmFileAnnouncedEvent {
        sender_id,
        hash: msg.hash,
        display_name: msg.display_name,
        total_size: msg.total_size,
        total_chunks: msg.total_chunks,
        num_sections: msg.num_sections,
        timestamp,
    });
    let _ = ctx.event_tx.send(event).await;

    Ok(())
}

async fn process_dm_sync_update(
    payload: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    let event = ProtocolEvent::DmSyncUpdate(crate::protocol::DmSyncUpdateEvent {
        sender_id,
        data: payload.to_vec(),
    });
    let _ = ctx.event_tx.send(event).await;

    Ok(())
}

async fn process_dm_sync_request(
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    let event = ProtocolEvent::DmSyncRequest(crate::protocol::DmSyncRequestEvent {
        sender_id,
    });
    let _ = ctx.event_tx.send(event).await;

    Ok(())
}

async fn process_dm_stream_request(
    plaintext: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::packet::DmMessage;

    let dm_msg = DmMessage::decode(plaintext)
        .map_err(|e| ProcessError::Decode(format!("DmStreamRequest: {}", e)))?;

    ctx.stream_service.handle_dm_signaling(&dm_msg, sender_id).await;

    Ok(())
}

// =============================================================================
// Stream signaling handlers
// =============================================================================

async fn process_stream_signaling(
    plaintext: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::packet::StreamSignalingMessage;

    let stream_msg = StreamSignalingMessage::decode(plaintext)
        .map_err(|e| ProcessError::Decode(format!("StreamSignaling: {}", e)))?;

    ctx.stream_service.handle_stream_signaling_msg(&stream_msg, sender_id).await;

    Ok(())
}

// =============================================================================
// Control message handlers
// =============================================================================

async fn process_connect_request(
    payload: &[u8],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::control::protocol::ConnectRequest;

    let req: ConnectRequest = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("ConnectRequest: {}", e)))?;

    // Store connection
    {
        let db_lock = ctx.db.lock().await;
        let _ = crate::data::upsert_connection(
            &db_lock,
            &req.sender_id,
            crate::data::ConnectionState::PendingIncoming,
            req.display_name.as_deref(),
            req.relay_url.as_deref(),
            Some(&req.request_id),
        );
    }

    let event = ProtocolEvent::ConnectionRequest(crate::protocol::ConnectionRequestEvent {
        peer_id: req.sender_id,
        request_id: req.request_id,
        display_name: req.display_name,
        relay_url: req.relay_url,
    });
    let _ = ctx.event_tx.send(event).await;

    Ok(())
}

async fn process_connect_accept(
    payload: &[u8],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::control::protocol::ConnectAccept;

    let accept: ConnectAccept = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("ConnectAccept: {}", e)))?;

    {
        let db_lock = ctx.db.lock().await;
        let _ = crate::data::update_connection_state(
            &db_lock,
            &accept.sender_id,
            crate::data::ConnectionState::Connected,
        );
    }

    let event = ProtocolEvent::ConnectionAccepted(crate::protocol::ConnectionAcceptedEvent {
        peer_id: accept.sender_id,
        request_id: accept.request_id,
    });
    let _ = ctx.event_tx.send(event).await;

    Ok(())
}

async fn process_connect_decline(
    payload: &[u8],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::control::protocol::ConnectDecline;

    let decline: ConnectDecline = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("ConnectDecline: {}", e)))?;

    {
        let db_lock = ctx.db.lock().await;
        let _ = crate::data::update_connection_state(
            &db_lock,
            &decline.sender_id,
            crate::data::ConnectionState::Declined,
        );
    }

    let event = ProtocolEvent::ConnectionDeclined(crate::protocol::ConnectionDeclinedEvent {
        request_id: decline.request_id,
        peer_id: decline.sender_id,
        reason: decline.reason,
    });
    let _ = ctx.event_tx.send(event).await;

    Ok(())
}

async fn process_topic_invite(
    payload: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::control::protocol::TopicInvite;

    let invite: TopicInvite = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("TopicInvite: {}", e)))?;

    // Store as pending invite
    {
        let db_lock = ctx.db.lock().await;
        let _ = crate::data::store_pending_invite(
            &db_lock,
            &invite.message_id,
            &invite.topic_id,
            &invite.sender_id,
            invite.topic_name.as_deref(),
            invite.epoch,
            &invite.epoch_key,
            &invite.admin_id,
            &invite.members,
        );
    }

    // Emit event
    let member_count = invite.members.len();
    let event = ProtocolEvent::TopicInviteReceived(crate::protocol::TopicInviteReceivedEvent {
        message_id: invite.message_id,
        topic_id: invite.topic_id,
        sender_id: invite.sender_id,
        topic_name: invite.topic_name.clone(),
        admin_id: invite.admin_id,
        member_count,
    });
    let _ = ctx.event_tx.send(event).await;

    info!(
        topic = %hex::encode(&invite.topic_id[..8]),
        sender = %hex::encode(&sender_id[..8]),
        admin = %hex::encode(&invite.admin_id[..8]),
        "TOPIC_INVITE_RECEIVED"
    );

    Ok(())
}

async fn process_topic_join(
    payload: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::control::protocol::TopicJoin;
    use crate::data::{add_topic_member, update_peer_relay_url, current_timestamp};

    let topic_id = get_topic_id(ctx)?;

    let join: TopicJoin = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("TopicJoin: {}", e)))?;

    // Validate sender matches packet sender
    if join.sender_id != sender_id {
        return Err(ProcessError::InvalidSender {
            expected: hex::encode(&join.sender_id[..8]),
            actual: hex::encode(&sender_id[..8]),
        });
    }

    let harbor_id = harbor_id_from_topic(&topic_id);
    if join.harbor_id != harbor_id {
        return Err(ProcessError::Decode("TopicJoin harbor_id mismatch".into()));
    }

    if !verify_membership_proof(&topic_id, &join.harbor_id, &sender_id, &join.membership_proof) {
        return Err(ProcessError::Decode("TopicJoin invalid membership proof".into()));
    }

    let db_lock = ctx.db.lock().await;
    let _ = add_topic_member(&db_lock, &topic_id, &join.sender_id);

    // Store relay URL if provided
    if let Some(ref relay_url) = join.relay_url {
        let _ = update_peer_relay_url(&db_lock, &join.sender_id, relay_url, current_timestamp());
    }

    debug!(
        topic = %hex::encode(&topic_id[..8]),
        joiner = %hex::encode(&join.sender_id[..8]),
        "TOPIC_JOIN processed"
    );

    Ok(())
}

async fn process_topic_leave(
    payload: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::control::protocol::TopicLeave;
    use crate::data::remove_topic_member;

    let topic_id = get_topic_id(ctx)?;

    let leave: TopicLeave = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("TopicLeave: {}", e)))?;

    // Validate sender matches packet sender
    if leave.sender_id != sender_id {
        return Err(ProcessError::InvalidSender {
            expected: hex::encode(&leave.sender_id[..8]),
            actual: hex::encode(&sender_id[..8]),
        });
    }

    let harbor_id = harbor_id_from_topic(&topic_id);
    if leave.harbor_id != harbor_id {
        return Err(ProcessError::Decode("TopicLeave harbor_id mismatch".into()));
    }

    if !verify_membership_proof(&topic_id, &leave.harbor_id, &sender_id, &leave.membership_proof) {
        return Err(ProcessError::Decode("TopicLeave invalid membership proof".into()));
    }

    let db_lock = ctx.db.lock().await;
    let _ = remove_topic_member(&db_lock, &topic_id, &leave.sender_id);

    debug!(
        topic = %hex::encode(&topic_id[..8]),
        leaver = %hex::encode(&leave.sender_id[..8]),
        "TOPIC_LEAVE processed"
    );

    Ok(())
}

async fn process_remove_member(
    payload: &[u8],
    sender_id: [u8; 32],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::control::protocol::RemoveMember;

    let remove: RemoveMember = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("RemoveMember: {}", e)))?;

    // Validate sender matches packet sender
    if remove.sender_id != sender_id {
        return Err(ProcessError::InvalidSender {
            expected: hex::encode(&remove.sender_id[..8]),
            actual: hex::encode(&sender_id[..8]),
        });
    }

    let db_lock = ctx.db.lock().await;
    let topic = get_topic_by_harbor_id(&db_lock, &remove.harbor_id)
        .map_err(|e| ProcessError::Database(e.to_string()))?;
    let topic_id = match topic {
        Some(topic) => topic.topic_id,
        None => return Err(ProcessError::Database("unknown topic for RemoveMember".into())),
    };

    if !verify_membership_proof(&topic_id, &remove.harbor_id, &sender_id, &remove.membership_proof) {
        return Err(ProcessError::Decode("RemoveMember invalid membership proof".into()));
    }

    // Store new epoch key
    let _ = crate::data::store_epoch_key(
        &db_lock,
        &topic_id,
        remove.new_epoch,
        &remove.new_epoch_key,
    );
    // Remove the member from our local list
    let _ = crate::data::remove_topic_member(&db_lock, &topic_id, &remove.removed_member);

    let event = ProtocolEvent::TopicEpochRotated(crate::protocol::TopicEpochRotatedEvent {
        topic_id,
        new_epoch: remove.new_epoch,
        removed_member: remove.removed_member,
    });
    let _ = ctx.event_tx.send(event).await;

    Ok(())
}

async fn process_suggest(
    payload: &[u8],
    ctx: &ProcessContext,
) -> Result<(), ProcessError> {
    use crate::network::control::protocol::Suggest;

    let suggest: Suggest = postcard::from_bytes(payload)
        .map_err(|e| ProcessError::Decode(format!("Suggest: {}", e)))?;

    let event = ProtocolEvent::PeerSuggested(crate::protocol::PeerSuggestedEvent {
        introducer_id: suggest.sender_id,
        suggested_peer_id: suggest.suggested_peer,
        relay_url: suggest.relay_url,
        note: suggest.note,
    });
    let _ = ctx.event_tx.send(event).await;

    info!(
        suggested = %hex::encode(&suggest.suggested_peer[..8]),
        "PEER_SUGGESTED"
    );

    Ok(())
}

// =============================================================================
// Helper functions
// =============================================================================

fn get_topic_id(ctx: &ProcessContext) -> Result<[u8; 32], ProcessError> {
    match &ctx.scope {
        ProcessScope::Topic { topic_id } => Ok(*topic_id),
        ProcessScope::Dm => Err(ProcessError::MissingTopicContext),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_scope() {
        let topic_id = [1u8; 32];
        let scope = ProcessScope::Topic { topic_id };

        if let ProcessScope::Topic { topic_id: tid } = scope {
            assert_eq!(tid, [1u8; 32]);
        } else {
            panic!("expected Topic scope");
        }
    }
}
