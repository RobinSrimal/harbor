//! Connection handlers
//!
//! Each service implements `iroh::protocol::ProtocolHandler` for ALPN-based routing.
//! The iroh Router dispatches incoming connections to the correct handler.

mod dht;
mod harbor;
mod live;
mod send;
mod share;
mod sync;

use std::sync::Arc;

use iroh::Endpoint;
use iroh::protocol::Router;

use crate::network::dht::{DhtService, DHT_ALPN};
use crate::network::harbor::HarborService;
use crate::network::harbor::protocol::HARBOR_ALPN;
use crate::network::live::{LiveService, LIVE_ALPN};
use crate::network::send::{SendService, protocol::SEND_ALPN};
use crate::network::share::{ShareService, SHARE_ALPN};
use crate::network::sync::{SyncService, SYNC_ALPN};

/// Build the iroh Router that dispatches incoming connections by ALPN.
pub(crate) fn build_router(
    endpoint: Endpoint,
    send_service: Arc<SendService>,
    dht_service: Option<Arc<DhtService>>,
    harbor_service: Arc<HarborService>,
    share_service: Arc<ShareService>,
    sync_service: Arc<SyncService>,
    live_service: Arc<LiveService>,
) -> Router {
    let mut builder = iroh::protocol::Router::builder(endpoint)
        .accept(SEND_ALPN, send_service)
        .accept(HARBOR_ALPN, harbor_service)
        .accept(SHARE_ALPN, share_service)
        .accept(SYNC_ALPN, sync_service)
        .accept(LIVE_ALPN, live_service);

    if let Some(dht) = dht_service {
        builder = builder.accept(DHT_ALPN, dht);
    }

    builder.spawn()
}
