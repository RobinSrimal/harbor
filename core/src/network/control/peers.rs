//! Peer connection lifecycle
//!
//! Handles peer-level connection management:
//! - Outgoing: request, accept, decline, block, unblock, token generation
//! - Incoming: handle connect request/accept/decline from remote peers

use rusqlite::Connection as DbConnection;
use serde::Serialize;
use tracing::{debug, info, warn};

use crate::data::{
    ConnectionState, consume_token, create_connect_token, get_connection, is_peer_blocked,
    list_connections_by_state, update_connection_state, upsert_connection,
};
use crate::protocol::{
    ConnectionAcceptedEvent, ConnectionDeclinedEvent, ConnectionRequestEvent, ProtocolEvent,
};

use super::protocol::{
    ConnectAccept, ConnectDecline, ConnectRequest, ControlAck, ControlPacketType,
};
use super::service::{
    ConnectInvite, ControlError, ControlResult, ControlService, compute_control_pow, generate_id,
    verify_sender,
};

fn encode_control_message<T: Serialize>(
    message: &T,
    label: &'static str,
) -> ControlResult<Vec<u8>> {
    postcard::to_allocvec(message)
        .map_err(|e| ControlError::Rpc(format!("failed to encode {}: {}", label, e)))
}

fn consume_connect_token_for_auto_accept(
    conn: &DbConnection,
    token: &[u8; 32],
) -> ControlResult<bool> {
    consume_token(conn, token).map_err(|e| ControlError::Database(e.to_string()))
}

fn persist_blocked_state(conn: &DbConnection, peer_id: &[u8; 32]) -> ControlResult<()> {
    upsert_connection(conn, peer_id, ConnectionState::Blocked, None, None, None)
        .map_err(|e| ControlError::Database(e.to_string()))
}

fn has_matching_pending_outgoing_request(
    conn: &DbConnection,
    peer_id: &[u8; 32],
    request_id: &[u8; 32],
) -> ControlResult<bool> {
    match get_connection(conn, peer_id).map_err(|e| ControlError::Database(e.to_string()))? {
        Some(existing) => Ok(existing.state == ConnectionState::PendingOutgoing
            && existing.request_id == Some(*request_id)),
        None => Ok(false),
    }
}

fn mark_pending_outgoing_declined(
    conn: &DbConnection,
    peer_id: &[u8; 32],
    request_id: &[u8; 32],
) -> ControlResult<bool> {
    if !has_matching_pending_outgoing_request(conn, peer_id, request_id)? {
        return Ok(false);
    }
    update_connection_state(conn, peer_id, ConnectionState::Declined)
        .map_err(|e| ControlError::Database(e.to_string()))
}

impl ControlService {
    // =========================================================================
    // Outgoing operations
    // =========================================================================

    /// Request a connection to a peer
    ///
    /// Sends a ConnectRequest message. If a token is provided, it's for the
    /// QR code / invite string flow where the recipient auto-accepts valid tokens.
    pub async fn request_connection(
        &self,
        peer_id: &[u8; 32],
        relay_url: Option<&str>,
        display_name: Option<&str>,
        token: Option<[u8; 32]>,
    ) -> ControlResult<[u8; 32]> {
        let request_id = generate_id();
        let our_relay = self
            .endpoint()
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string());
        let pow = compute_control_pow(&self.local_id(), ControlPacketType::ConnectRequest)?;

        let req = ConnectRequest {
            request_id,
            sender_id: self.local_id(),
            display_name: display_name.map(|s| s.to_string()),
            relay_url: our_relay.clone(),
            token,
            pow,
        };

        // Store pending outgoing connection
        {
            let db = self.db().lock().await;
            upsert_connection(
                &db,
                peer_id,
                ConnectionState::PendingOutgoing,
                None,
                relay_url,
                Some(&request_id),
            )
            .map_err(|e| ControlError::Database(e.to_string()))?;
        }

        // Try direct delivery
        match self.dial_peer(peer_id).await {
            Ok(client) => match client.rpc(req.clone()).await {
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
            },
            Err(e) => {
                debug!(
                    peer = %hex::encode(&peer_id[..8]),
                    error = %e,
                    "peer offline, will replicate via harbor"
                );
            }
        }

        // Store for harbor replication (point-to-point: harbor_id = recipient_id)
        self.store_control_packet(
            &request_id,
            peer_id,     // harbor_id = recipient for point-to-point
            &[*peer_id], // single recipient
            &encode_control_message(&req, "ConnectRequest")?,
            ControlPacketType::ConnectRequest,
        )
        .await?;

        Ok(request_id)
    }

    /// Accept a connection request
    pub async fn accept_connection(&self, request_id: &[u8; 32]) -> ControlResult<()> {
        // Find the pending incoming connection by request_id
        let peer_id = {
            let db = self.db().lock().await;
            let connections = list_connections_by_state(&db, ConnectionState::PendingIncoming)
                .map_err(|e| ControlError::Database(e.to_string()))?;
            let conn = connections
                .into_iter()
                .find(|c| c.request_id == Some(*request_id))
                .ok_or(ControlError::RequestNotFound)?;
            conn.peer_id
        };

        // Update connection state
        {
            let db = self.db().lock().await;
            update_connection_state(&db, &peer_id, ConnectionState::Connected)
                .map_err(|e| ControlError::Database(e.to_string()))?;
        }

        // Update connection gate
        if let Some(gate) = self.connection_gate() {
            gate.mark_dm_connected(&peer_id).await;
        }

        // Send the accept message
        self.send_connect_accept(&peer_id, request_id).await
    }

    /// Send a ConnectAccept message to a peer
    ///
    /// Used by both `accept_connection` (manual accept) and the auto-accept flow
    /// when a valid token is provided in the connect request.
    pub(crate) async fn send_connect_accept(
        &self,
        peer_id: &[u8; 32],
        request_id: &[u8; 32],
    ) -> ControlResult<()> {
        let our_relay = self
            .endpoint()
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string());
        let pow = compute_control_pow(&self.local_id(), ControlPacketType::ConnectAccept)?;

        let accept = ConnectAccept {
            request_id: *request_id,
            sender_id: self.local_id(),
            display_name: None, // Could be set from local config
            relay_url: our_relay.clone(),
            pow,
        };

        // Try direct delivery
        if let Ok(client) = self.dial_peer(peer_id).await {
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
        self.store_control_packet(
            request_id,
            peer_id, // harbor_id = recipient
            &[*peer_id],
            &encode_control_message(&accept, "ConnectAccept")?,
            ControlPacketType::ConnectAccept,
        )
        .await?;

        Ok(())
    }

    /// Decline a connection request
    pub async fn decline_connection(
        &self,
        request_id: &[u8; 32],
        reason: Option<&str>,
    ) -> ControlResult<()> {
        // Find the pending incoming connection
        let peer_id = {
            let db = self.db().lock().await;
            let connections = list_connections_by_state(&db, ConnectionState::PendingIncoming)
                .map_err(|e| ControlError::Database(e.to_string()))?;
            let conn = connections
                .into_iter()
                .find(|c| c.request_id == Some(*request_id))
                .ok_or(ControlError::RequestNotFound)?;
            conn.peer_id
        };

        let pow = compute_control_pow(&self.local_id(), ControlPacketType::ConnectDecline)?;
        let decline = ConnectDecline {
            request_id: *request_id,
            sender_id: self.local_id(),
            reason: reason.map(|s| s.to_string()),
            pow,
        };

        // Update connection state
        {
            let db = self.db().lock().await;
            update_connection_state(&db, &peer_id, ConnectionState::Declined)
                .map_err(|e| ControlError::Database(e.to_string()))?;
        }

        // Try direct delivery
        if let Ok(client) = self.dial_peer(&peer_id).await {
            let _ = client.rpc(decline.clone()).await;
        }

        // Store for harbor replication
        self.store_control_packet(
            request_id,
            &peer_id,
            &[peer_id],
            &encode_control_message(&decline, "ConnectDecline")?,
            ControlPacketType::ConnectDecline,
        )
        .await?;

        Ok(())
    }

    /// Generate a connect token (QR code / invite string)
    pub async fn generate_connect_token(&self) -> ControlResult<ConnectInvite> {
        let token = generate_id();

        // Store token
        {
            let db = self.db().lock().await;
            create_connect_token(&db, &token).map_err(|e| ControlError::Database(e.to_string()))?;
        }

        let invite = ConnectInvite {
            endpoint_id: self.local_id(),
            relay_url: self
                .endpoint()
                .addr()
                .relay_urls()
                .next()
                .map(|u| u.to_string()),
            token,
        };

        info!(
            token = %hex::encode(&token[..8]),
            "connect token generated"
        );

        Ok(invite)
    }

    /// Block a peer
    pub async fn block_peer(&self, peer_id: &[u8; 32]) -> ControlResult<()> {
        {
            let db = self.db().lock().await;
            persist_blocked_state(&db, peer_id)?;
        }

        // Update connection gate
        if let Some(gate) = self.connection_gate() {
            gate.mark_dm_blocked(peer_id).await;
        }

        info!(peer = %hex::encode(&peer_id[..8]), "peer blocked");
        Ok(())
    }

    /// Unblock a peer
    pub async fn unblock_peer(&self, peer_id: &[u8; 32]) -> ControlResult<()> {
        let was_blocked = {
            let db = self.db().lock().await;
            if let Some(conn) =
                get_connection(&db, peer_id).map_err(|e| ControlError::Database(e.to_string()))?
            {
                if conn.state == ConnectionState::Blocked {
                    update_connection_state(&db, peer_id, ConnectionState::Connected)
                        .map_err(|e| ControlError::Database(e.to_string()))
                } else {
                    Ok(false)
                }
            } else {
                Ok(false)
            }
        }?;

        if was_blocked {
            // Update connection gate
            if let Some(gate) = self.connection_gate() {
                gate.mark_dm_connected(peer_id).await;
            }
            info!(peer = %hex::encode(&peer_id[..8]), "peer unblocked");
        }
        Ok(())
    }

    // =========================================================================
    // Incoming handlers
    // =========================================================================

    /// Handle an incoming ConnectRequest
    pub async fn handle_connect_request(
        &self,
        req: &ConnectRequest,
        sender_id: [u8; 32],
    ) -> ControlAck {
        // Verify PoW first
        if let Err(e) = self.verify_pow(&req.pow, &sender_id, ControlPacketType::ConnectRequest) {
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
            let db = self.db().lock().await;
            match is_peer_blocked(&db, &sender_id) {
                Ok(blocked) => blocked,
                Err(e) => {
                    warn!(
                        sender = %hex::encode(&sender_id[..8]),
                        error = %e,
                        "connect request rejected: failed to check blocked state"
                    );
                    return ControlAck::failure(req.request_id, "internal error");
                }
            }
        };
        if is_blocked {
            info!(peer = %hex::encode(&sender_id[..8]), "rejected connect request from blocked peer");
            return ControlAck::failure(req.request_id, "blocked");
        }

        // Check for valid token (auto-accept flow)
        let auto_accept = if let Some(token) = req.token {
            let db = self.db().lock().await;
            match consume_connect_token_for_auto_accept(&db, &token) {
                Ok(true) => true,
                Ok(false) => {
                    info!(
                        peer = %hex::encode(&sender_id[..8]),
                        "rejected connect request with invalid token"
                    );
                    return ControlAck::failure(req.request_id, "invalid token");
                }
                Err(e) => {
                    warn!(
                        sender = %hex::encode(&sender_id[..8]),
                        error = %e,
                        "connect request rejected: failed to consume token"
                    );
                    return ControlAck::failure(req.request_id, "internal error");
                }
            }
        } else {
            false
        };

        // Store connection info
        {
            let db = self.db().lock().await;
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
            if let Some(gate) = self.connection_gate() {
                gate.mark_dm_connected(&sender_id).await;
            }

            // Send ConnectAccept back to the requester
            if let Err(e) = self.send_connect_accept(&sender_id, &req.request_id).await {
                warn!(error = %e, "failed to send connect accept");
            }

            // Emit accepted event
            let event = ProtocolEvent::ConnectionAccepted(ConnectionAcceptedEvent {
                peer_id: sender_id,
                request_id: req.request_id,
            });
            let _ = self.event_tx().send(event).await;
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
            let _ = self.event_tx().send(event).await;
        }

        ControlAck::success(req.request_id)
    }

    /// Handle an incoming ConnectAccept
    pub async fn handle_connect_accept(
        &self,
        accept: &ConnectAccept,
        sender_id: [u8; 32],
    ) -> ControlAck {
        // Verify PoW first
        if let Err(e) = self.verify_pow(&accept.pow, &sender_id, ControlPacketType::ConnectAccept) {
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
            let db = self.db().lock().await;
            match has_matching_pending_outgoing_request(&db, &sender_id, &accept.request_id) {
                Ok(true) => {
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
                }
                Ok(false) => return ControlAck::failure(accept.request_id, "no pending request"),
                Err(e) => {
                    warn!(error = %e, "failed to load pending outgoing connection");
                    return ControlAck::failure(accept.request_id, "internal error");
                }
            }
        }

        // Update connection gate
        if let Some(gate) = self.connection_gate() {
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
        let _ = self.event_tx().send(event).await;

        ControlAck::success(accept.request_id)
    }

    /// Handle an incoming ConnectDecline
    pub async fn handle_connect_decline(
        &self,
        decline: &ConnectDecline,
        sender_id: [u8; 32],
    ) -> ControlAck {
        // Verify PoW first
        if let Err(e) = self.verify_pow(&decline.pow, &sender_id, ControlPacketType::ConnectDecline)
        {
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
            let db = self.db().lock().await;
            match mark_pending_outgoing_declined(&db, &sender_id, &decline.request_id) {
                Ok(true) => {}
                Ok(false) => return ControlAck::failure(decline.request_id, "no pending request"),
                Err(e) => {
                    warn!(error = %e, "failed to update declined connection state");
                    return ControlAck::failure(decline.request_id, "internal error");
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
        let _ = self.event_tx().send(event).await;

        ControlAck::success(decline.request_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::{create_control_tables, create_peer_table};
    use crate::data::{ConnectionState, create_connect_token, get_connection, upsert_connection};

    fn setup_db() -> DbConnection {
        let conn = DbConnection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_peer_table(&conn).unwrap();
        create_control_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_consume_connect_token_for_auto_accept() {
        let conn = setup_db();
        let token = [1u8; 32];
        create_connect_token(&conn, &token).unwrap();

        assert!(consume_connect_token_for_auto_accept(&conn, &token).unwrap());
        assert!(!consume_connect_token_for_auto_accept(&conn, &token).unwrap());
    }

    #[test]
    fn test_persist_blocked_state_creates_connection_row() {
        let conn = setup_db();
        let peer_id = [9u8; 32];

        persist_blocked_state(&conn, &peer_id).unwrap();

        let stored = get_connection(&conn, &peer_id).unwrap().unwrap();
        assert_eq!(stored.state, ConnectionState::Blocked);
    }

    #[test]
    fn test_has_matching_pending_outgoing_request() {
        let conn = setup_db();
        let peer_id = [2u8; 32];
        let request_id = [3u8; 32];

        upsert_connection(
            &conn,
            &peer_id,
            ConnectionState::PendingOutgoing,
            None,
            None,
            Some(&request_id),
        )
        .unwrap();

        assert!(has_matching_pending_outgoing_request(&conn, &peer_id, &request_id).unwrap());
        assert!(!has_matching_pending_outgoing_request(&conn, &peer_id, &[4u8; 32]).unwrap());
    }

    #[test]
    fn test_mark_pending_outgoing_declined_requires_matching_request() {
        let conn = setup_db();
        let peer_id = [5u8; 32];
        let request_id = [6u8; 32];

        upsert_connection(
            &conn,
            &peer_id,
            ConnectionState::PendingOutgoing,
            None,
            None,
            Some(&request_id),
        )
        .unwrap();

        assert!(!mark_pending_outgoing_declined(&conn, &peer_id, &[7u8; 32]).unwrap());
        assert!(mark_pending_outgoing_declined(&conn, &peer_id, &request_id).unwrap());

        let stored = get_connection(&conn, &peer_id).unwrap().unwrap();
        assert_eq!(stored.state, ConnectionState::Declined);
    }
}
