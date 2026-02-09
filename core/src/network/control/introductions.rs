//! Peer introductions
//!
//! Handles suggesting peers to each other:
//! - Outgoing: suggest a peer to another peer
//! - Incoming: handle suggestion from a remote peer

use tracing::{info, warn};

use crate::protocol::{PeerSuggestedEvent, ProtocolEvent};

use super::protocol::{ControlAck, ControlPacketType, Suggest};
use super::service::{
    compute_control_pow, generate_id, verify_sender, ControlResult, ControlService,
};

impl ControlService {
    /// Suggest a peer to another peer
    pub async fn suggest_peer(
        &self,
        to_peer: &[u8; 32],
        suggested_peer: &[u8; 32],
        note: Option<&str>,
    ) -> ControlResult<[u8; 32]> {
        let message_id = generate_id();

        // Get suggested peer's relay URL from our database
        let relay_url = {
            let db = self.db().lock().await;
            crate::data::get_peer_relay_info(&db, suggested_peer)
                .ok()
                .flatten()
                .map(|(url, _)| url)
        };

        let pow = compute_control_pow(&self.local_id(), ControlPacketType::Suggest)?;
        let suggest = Suggest {
            message_id,
            sender_id: self.local_id(),
            suggested_peer: *suggested_peer,
            relay_url,
            note: note.map(|s| s.to_string()),
            pow,
        };

        // Try direct delivery
        if let Ok(client) = self.dial_peer(to_peer).await {
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
        self.store_control_packet(
            &message_id,
            to_peer,
            &[*to_peer],
            &postcard::to_allocvec(&suggest).unwrap(),
            ControlPacketType::Suggest,
        )
        .await?;

        Ok(message_id)
    }

    /// Handle an incoming Suggest (peer introduction)
    pub async fn handle_suggest(
        &self,
        suggest: &Suggest,
        sender_id: [u8; 32],
    ) -> ControlAck {
        // Verify PoW first
        if let Err(e) = self.verify_pow(&suggest.pow, &sender_id, ControlPacketType::Suggest) {
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
        let _ = self.event_tx().send(event).await;

        ControlAck::success(suggest.message_id)
    }
}
