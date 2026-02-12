//! Connection handlers
//!
//! Each service implements `iroh::protocol::ProtocolHandler` for ALPN-based routing.
//! The iroh Router dispatches incoming connections to the correct handler.

mod control;
mod dht;
mod harbor;
mod send;
mod share;
mod stream;
mod sync;

use std::sync::Arc;

use iroh::Endpoint;
use iroh::protocol::Router;

use crate::network::control::{CONTROL_ALPN, ControlService};
use crate::network::dht::{DHT_ALPN, DhtService};
use crate::network::harbor::HarborService;
use crate::network::harbor::protocol::HARBOR_ALPN;
use crate::network::send::{SendService, protocol::SEND_ALPN};
use crate::network::share::{SHARE_ALPN, ShareService};
use crate::network::stream::{STREAM_ALPN, StreamService};
use crate::network::sync::{SYNC_ALPN, SyncService};

/// Build the iroh Router that dispatches incoming connections by ALPN.
pub(crate) fn build_router(
    endpoint: Endpoint,
    send_service: Arc<SendService>,
    dht_service: Option<Arc<DhtService>>,
    harbor_service: Arc<HarborService>,
    share_service: Arc<ShareService>,
    sync_service: Arc<SyncService>,
    stream_service: Arc<StreamService>,
    control_service: Arc<ControlService>,
) -> Router {
    let mut builder = iroh::protocol::Router::builder(endpoint)
        .accept(SEND_ALPN, send_service)
        .accept(HARBOR_ALPN, harbor_service)
        .accept(SHARE_ALPN, share_service)
        .accept(SYNC_ALPN, sync_service)
        .accept(STREAM_ALPN, stream_service)
        .accept(CONTROL_ALPN, control_service);

    if let Some(dht) = dht_service {
        builder = builder.accept(DHT_ALPN, dht);
    }

    builder.spawn()
}
