//! Incoming connection handlers
//!
//! Handles connections from other endpoints. Routes based on ALPN:
//! - SEND_ALPN: Message delivery (packets, receipts)
//! - DHT_ALPN: DHT protocol (FindNode requests)
//! - HARBOR_ALPN: Harbor protocol (store, pull, sync)

mod dht;
mod harbor;
mod send;

use std::sync::Arc;
use std::time::Duration;

use iroh::Endpoint;
use rusqlite::Connection;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, trace};

use crate::network::dht::{ApiClient as DhtApiClient, DHT_ALPN};
use crate::network::harbor::protocol::HARBOR_ALPN;
use crate::network::send::protocol::SEND_ALPN;

use crate::protocol::{IncomingMessage, Protocol};

impl Protocol {
    /// Run the incoming connection handler
    ///
    /// Accepts connections and routes them to protocol-specific handlers.
    pub(crate) async fn run_incoming_handler(
        endpoint: Endpoint,
        db: Arc<Mutex<Connection>>,
        event_tx: mpsc::Sender<IncomingMessage>,
        our_id: [u8; 32],
        running: Arc<RwLock<bool>>,
        dht_client: Option<DhtApiClient>,
    ) {
        loop {
            // Check if we should stop
            if !*running.read().await {
                break;
            }

            // Accept incoming connection
            let incoming = tokio::select! {
                conn = endpoint.accept() => conn,
                _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
            };

            let Some(incoming) = incoming else {
                // Endpoint is shutting down
                break;
            };

            // Accept the connection
            let conn = match incoming.await {
                Ok(conn) => conn,
                Err(e) => {
                    debug!(error = %e, "failed to accept connection");
                    continue;
                }
            };

            // Check ALPN and route to appropriate handler
            let alpn = conn.alpn();
            trace!(alpn = ?alpn, "connection ALPN check");

            // Get sender's NodeId
            let sender_id: [u8; 32] = match conn.remote_node_id() {
                Ok(id) => *id.as_bytes(),
                Err(e) => {
                    debug!(error = %e, "failed to get remote node id");
                    continue;
                }
            };

            trace!(sender = %hex::encode(sender_id), "accepted connection");

            if alpn.as_deref() == Some(SEND_ALPN) {
                // Handle Send protocol
                let db = db.clone();
                let event_tx = event_tx.clone();

                tokio::spawn(async move {
                    trace!(sender = %hex::encode(sender_id), "starting Send connection handler");
                    if let Err(e) =
                        Self::handle_send_connection(conn, db, event_tx, sender_id, our_id).await
                    {
                        debug!(error = %e, sender = %hex::encode(sender_id), "Send connection handler error");
                    }
                });
            } else if alpn.as_deref() == Some(DHT_ALPN) {
                // Handle DHT protocol
                let db = db.clone();
                let dht_client = dht_client.clone();

                tokio::spawn(async move {
                    trace!(sender = %hex::encode(sender_id), "starting DHT connection handler");
                    if let Err(e) =
                        Self::handle_dht_connection(conn, db, sender_id, our_id, dht_client).await
                    {
                        debug!(error = %e, sender = %hex::encode(sender_id), "DHT connection handler error");
                    }
                });
            } else if alpn.as_deref() == Some(HARBOR_ALPN) {
                // Handle Harbor protocol (store, pull, sync)
                let db = db.clone();

                tokio::spawn(async move {
                    trace!(sender = %hex::encode(sender_id), "starting Harbor connection handler");
                    if let Err(e) =
                        Self::handle_harbor_connection(conn, db, sender_id, our_id).await
                    {
                        debug!(error = %e, sender = %hex::encode(sender_id), "Harbor connection handler error");
                    }
                });
            } else {
                debug!(alpn = ?alpn, "ignoring unknown ALPN");
                continue;
            }
        }

        info!("Incoming handler stopped");
    }
}
