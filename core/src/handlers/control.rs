//! Control protocol incoming handler
//!
//! Handles incoming Control protocol connections via irpc.
//! Dispatches to service methods for actual processing.

use iroh::protocol::{AcceptError, ProtocolHandler};
use tracing::{debug, trace};

use crate::network::control::protocol::{ControlRpcMessage, ControlRpcProtocol};
use crate::network::control::{incoming, ControlService};

impl ProtocolHandler for ControlService {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let sender_id = *conn.remote_id().as_bytes();
        if let Err(e) = handle_control_connection(self, conn, sender_id).await {
            debug!(error = %e, sender = %hex::encode(sender_id), "Control connection handler error");
        }
        Ok(())
    }
}

/// Handle a single incoming Control protocol connection
///
/// Uses irpc for wire framing (varint length-prefix + postcard).
/// Dispatches control messages to service methods.
async fn handle_control_connection(
    service: &ControlService,
    conn: iroh::endpoint::Connection,
    sender_id: [u8; 32],
) -> Result<(), crate::protocol::ProtocolError> {
    trace!(sender = %hex::encode(&sender_id[..8]), "handling Control connection");

    loop {
        // Read next request using irpc framing
        let msg = match irpc_iroh::read_request::<ControlRpcProtocol>(&conn).await {
            Ok(Some(msg)) => msg,
            Ok(None) => {
                trace!("Control connection closed normally");
                break;
            }
            Err(e) => {
                debug!(error = %e, "Control read_request error");
                break;
            }
        };

        // Dispatch to service methods
        match msg {
            ControlRpcMessage::ConnectRequest(req) => {
                let response = incoming::handle_connect_request(service, &req, sender_id).await;
                req.tx.send(response).await.ok();
            }
            ControlRpcMessage::ConnectAccept(accept) => {
                let response = incoming::handle_connect_accept(service, &accept, sender_id).await;
                accept.tx.send(response).await.ok();
            }
            ControlRpcMessage::ConnectDecline(decline) => {
                let response = incoming::handle_connect_decline(service, &decline, sender_id).await;
                decline.tx.send(response).await.ok();
            }
            ControlRpcMessage::TopicInvite(invite) => {
                let response = incoming::handle_topic_invite(service, &invite, sender_id).await;
                invite.tx.send(response).await.ok();
            }
            ControlRpcMessage::TopicJoin(join) => {
                let response = incoming::handle_topic_join(service, &join, sender_id).await;
                join.tx.send(response).await.ok();
            }
            ControlRpcMessage::TopicLeave(leave) => {
                let response = incoming::handle_topic_leave(service, &leave, sender_id).await;
                leave.tx.send(response).await.ok();
            }
            ControlRpcMessage::RemoveMember(remove) => {
                let response = incoming::handle_remove_member(service, &remove, sender_id).await;
                remove.tx.send(response).await.ok();
            }
            ControlRpcMessage::Suggest(suggest) => {
                let response = incoming::handle_suggest(service, &suggest, sender_id).await;
                suggest.tx.send(response).await.ok();
            }
        }
    }

    Ok(())
}
