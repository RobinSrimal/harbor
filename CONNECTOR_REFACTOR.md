# Connector Refactor

## Context

Every service that makes outgoing QUIC connections duplicates the same pattern:
1. Lock DB, fetch relay URL from `peers` table via `get_peer_relay_info()`
2. Parse relay URL, call `connect::connect()` with relay freshness logic
3. On success, write back `update_peer_relay_url()` if relay was confirmed
4. Cache the connection (some services) or use one-off (others)

This is repeated in SendService, HarborService, ShareService, SyncService, StreamService. ControlService already uses a `Connector` abstraction that centralizes steps 1-3, but lacks pooling.

## Current State

- `Connector` (connect.rs): relay lookup + connect + relay-confirm. No pooling. Used by ControlService only.
- `ConnectionPool` (pool.rs): caching + LRU eviction + liveness checks. No relay logic. Used by DhtPool only.
- `SendService`: reimplements both inline (HashMap cache + relay boilerplate). `send/pool.rs` is dead code.
- Other services (Harbor, Share, Sync, Stream): duplicate the relay boilerplate, no pooling.

## Target Design

**One ALPN per Connector, passed at construction. Single method: `connector.connect(peer_id)` where `peer_id` is the numeric FK from `peers`.**

Each service gets its own `Arc<Connector>` configured for its ALPN, timeout, and pool settings. They share `db` and `endpoint` via Arc.

```rust
pub struct Connector {
    endpoint: Endpoint,
    db: Arc<Mutex<DbConnection>>,
    alpn: &'static [u8],
    connect_timeout: Duration,
    pool: Option<ConnectionPool>,  // None = one-off, Some = pooled
}

impl Connector {
    pub fn new(
        endpoint: Endpoint,
        db: Arc<Mutex<DbConnection>>,
        alpn: &'static [u8],
        connect_timeout: Duration,
        pool_config: Option<PoolConfig>,
    ) -> Self {
        let pool = pool_config.map(|config|
            ConnectionPool::new(endpoint.clone(), alpn, config)
        );
        Self { endpoint, db, alpn, connect_timeout, pool }
    }

    pub fn endpoint(&self) -> &Endpoint { &self.endpoint }

    /// Single connection method — pooled or one-off based on config
    pub async fn connect(&self, peer_id: i64) -> Result<Connection, ConnectError> {
        let (endpoint_id, relay_url) = self.load_peer_connection_info(peer_id).await?;
        let node_id = EndpointId::from_bytes(&endpoint_id)?;

        // If pooled, check cache first
        if let Some(pool) = &self.pool {
            if let Some(cached) = pool.try_get_cached(node_id).await {
                return Ok(cached.connection().clone());
            }
        }

        // Relay lookup → smart connect → relay-confirm writeback
        let conn = self.connect_inner(&endpoint_id, relay_url.as_deref(), node_id).await?;

        // Cache if pooled
        if let Some(pool) = &self.pool {
            let _ = pool.cache_connection(node_id, conn.clone()).await;
        }

        Ok(conn)
    }

    pub async fn close(&self, peer_id: i64) { ... }

    /// Internal: relay lookup + connect + relay-confirm
    async fn connect_inner(&self, endpoint_id: &[u8; 32], relay_url: Option<&str>, node_id: EndpointId)
        -> Result<Connection, ConnectError> { /* existing connect_to_peer logic */ }
}
```

### Usage

```
SendService    → Arc<Connector> { alpn: SEND_ALPN,    pool: Some(PoolConfig::default()) }
ControlService → Arc<Connector> { alpn: CONTROL_ALPN, pool: None }

connector.connect(peer_id) → Result<Connection>
  ├── Has pool? → check cache → hit? return cached
  │                           → miss? peer lookup → connect → confirm → cache
  └── No pool?  → peer lookup → connect → confirm → return

DhtPool → wraps ConnectionPool directly (unchanged, no Connector involvement)
```

### Startup wiring (core.rs)

```rust
let send_connector = Arc::new(Connector::new(
    endpoint.clone(), db.clone(), SEND_ALPN,
    Duration::from_secs(5), Some(PoolConfig::default()),
));
let control_connector = Arc::new(Connector::new(
    endpoint.clone(), db.clone(), CONTROL_ALPN,
    Duration::from_secs(10), None,
));
```

### Prerequisites

- `ConnectionPool` (pool.rs) needs two new public methods:
  - `try_get_cached(node_id) -> Option<ConnectionRef>`
  - `cache_connection(node_id, conn) -> Result<ConnectionRef, PoolError>`
- Existing `get_connection()` on ConnectionPool stays unchanged (DhtPool uses it)

### Changes per service

| Service | Current | After |
|---------|---------|-------|
| **SendService** | `endpoint` + `HashMap<EndpointId, Connection>` + inline relay boilerplate | `Arc<Connector>` with pooled config |
| **ControlService** | `Arc<Connector>` calling `connect_to_peer(peer_id, alpn, timeout)` | `Arc<Connector>` calling `connect(peer_id)` |
| **HarborService** | `endpoint` + inline relay boilerplate | `Arc<Connector>` (no pool, one-off connections) |
| **ShareService** | `endpoint` + inline relay boilerplate | `Arc<Connector>` (no pool, one-off connections) |
| **SyncService** | `endpoint` + inline relay boilerplate | `Arc<Connector>` (no pool, one-off connections) |
| **StreamService** | `endpoint` + inline relay boilerplate | `Arc<Connector>` (no pool, one-off connections) |
| **DhtService** | `DhtPool` wrapping `ConnectionPool` directly | Unchanged |

Only Send and DHT use connection pools. All other services use one-off connections via `Connector` without pooling.

### What gets deleted

- `send/pool.rs` (dead code)
- SendPool re-exports from `send/mod.rs`
- ~15 lines of relay boilerplate per service (×6 services)
- `endpoint` field from services that don't need it directly
