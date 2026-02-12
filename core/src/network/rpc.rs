//! Shared RPC utilities for irpc over iroh connections
//!
//! Provides the `ExistingConnection` adapter that allows using an already-established
//! `iroh::endpoint::Connection` with irpc's typed RPC client. This enables any protocol
//! to send typed request/response RPCs over QUIC streams with automatic
//! varint length-prefix + postcard serialization.
//!
//! # Usage
//!
//! ```ignore
//! use crate::network::rpc::ExistingConnection;
//!
//! // Outgoing: send a typed RPC over an existing connection
//! let client = irpc::Client::<MyProtocol>::boxed(ExistingConnection::new(conn));
//! let response = client.rpc(MyRequest { ... }).await?;
//!
//! // Incoming: read typed requests from a connection
//! let msg = irpc_iroh::read_request::<MyProtocol>(&conn).await?;
//! ```

use std::sync::Arc;

use iroh::endpoint::Connection;

/// Adapter to use an existing iroh Connection with irpc's typed RPC client
///
/// Wraps an already-established `iroh::endpoint::Connection` to satisfy
/// irpc's `RemoteConnection` trait. This lets you create an `irpc::Client`
/// for any protocol without establishing a new connection.
#[derive(Debug, Clone)]
pub struct ExistingConnection(Arc<Connection>);

impl ExistingConnection {
    /// Wrap an existing connection
    pub fn new(conn: &Connection) -> Self {
        Self(Arc::new(conn.clone()))
    }
}

impl irpc::rpc::RemoteConnection for ExistingConnection {
    fn clone_boxed(&self) -> Box<dyn irpc::rpc::RemoteConnection> {
        Box::new(self.clone())
    }

    fn open_bi(
        &self,
    ) -> n0_future::future::Boxed<
        Result<(iroh::endpoint::SendStream, iroh::endpoint::RecvStream), irpc::RequestError>,
    > {
        let conn = self.0.clone();
        Box::pin(async move {
            let (send, recv) = conn.open_bi().await?;
            Ok((send, recv))
        })
    }

    fn zero_rtt_accepted(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'static>> {
        // Already-established connections have completed the handshake
        Box::pin(async { true })
    }
}
