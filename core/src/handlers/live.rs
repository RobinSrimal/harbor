//! Live protocol incoming handler
//!
//! Handles incoming Live protocol connections (MOQ media transport).
//! Peers connect via LIVE_ALPN after a stream request has been accepted.
//!
//! The incoming side is the **destination** — the peer who accepted the stream.
//! The source opens a QUIC connection with LIVE_ALPN, and this handler performs
//! the MOQ handshake via `moq_lite::Server`, creating a `LiveSession` that the
//! destination app uses to consume the broadcast.

use iroh::protocol::{AcceptError, ProtocolHandler};
use tracing::{debug, info, warn};
use web_transport_iroh::QuicRequest;

use crate::network::live::LiveService;
use crate::network::live::session::LiveSession;

impl ProtocolHandler for LiveService {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let peer_id = *conn.remote_id().as_bytes();
        info!(
            peer = %hex::encode(&peer_id[..8]),
            "incoming Live (MOQ) connection"
        );

        if let Err(e) = self.handle_incoming_moq(conn, peer_id).await {
            debug!(error = %e, peer = %hex::encode(&peer_id[..8]), "Live connection handler error");
        }
        Ok(())
    }
}

impl LiveService {
    /// Handle an incoming MOQ connection from the stream source.
    ///
    /// The destination (us) accepted the stream via signaling. Now the source
    /// opens a QUIC connection with LIVE_ALPN. We perform the MOQ handshake
    /// and create a LiveSession for consuming the broadcast.
    pub(crate) async fn handle_incoming_moq(
        &self,
        conn: iroh::endpoint::Connection,
        peer_id: [u8; 32],
    ) -> Result<(), crate::network::live::LiveError> {
        use crate::network::live::LiveError;

        // 1. Wrap iroh Connection as a raw WebTransport session (no HTTP/3 — direct ALPN)
        let wt_session = QuicRequest::accept(conn).ok();

        // 2. Find which accepted stream this connection corresponds to.
        //    Look through pending_incoming for a request from this peer.
        let pending = self.find_pending_incoming_for_peer(&peer_id).await;

        let pending = match pending {
            Some(p) => p,
            None => {
                warn!(
                    peer = %hex::encode(&peer_id[..8]),
                    "incoming MOQ connection from peer with no accepted stream request"
                );
                return Err(LiveError::RequestNotFound);
            }
        };

        // 3. Create Origin for content routing
        let origin = moq_lite::Origin::produce();

        // 4. Perform MOQ handshake as server
        //    - with_publish: we provide OriginConsumer so the remote (source) can publish to us
        //    - with_consume: we provide OriginProducer so the remote can subscribe from us
        let moq_server = moq_lite::Server::new()
            .with_publish(origin.consumer.clone())
            .with_consume(origin.producer.clone());

        let moq_session = moq_server.accept(wt_session).await
            .map_err(|e| LiveError::Moq(e.to_string()))?;

        info!(
            peer = %hex::encode(&peer_id[..8]),
            request_id = %hex::encode(&pending.request_id[..8]),
            "MOQ session established (destination side)"
        );

        // 5. Create LiveSession and store it
        let live_session = LiveSession::from_parts(
            moq_session,
            origin.producer,
            origin.consumer,
            pending.topic_id,
            pending.request_id,
        );

        self.store_session(pending.request_id, live_session).await;

        // 6. Wait for session to close (keeps the handler alive while media flows)
        if let Some(session) = self.get_session(&pending.request_id).await {
            let _ = session.session().closed().await;
            info!(
                request_id = %hex::encode(&pending.request_id[..8]),
                "MOQ session closed"
            );
        }
        self.remove_session(&pending.request_id).await;

        Ok(())
    }
}
