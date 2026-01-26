# Connection Primitive Design

This document outlines enhancements to the existing connection infrastructure in Harbor.

## Current State

Harbor already has a `ConnectionPool` at `core/src/network/pool.rs` with:
- Connection caching with idle timeout
- LRU eviction when at capacity
- Connection liveness checking
- Configurable timeouts and limits

## What's Missing: DNS Fallback

The current `ConnectionPool::connect()` method calls `endpoint.connect(node_addr, self.alpn)` directly. If the provided relay URL is stale, the connection fails without trying DNS discovery.

**Iroh behavior**: DNS discovery only triggers when **no relay URL is provided**. If we provide a stale relay URL, iroh uses it directly and fails.

## Proposed Enhancement

Add DNS fallback logic to `ConnectionPool::connect()`:

```rust
async fn connect(&self, node_addr: NodeAddr) -> Result<ConnectionRef, PoolError> {
    let node_id = node_addr.node_id;

    // ... existing connection limit check ...

    // Try with provided NodeAddr (may include relay URL)
    let connect_fut = self.endpoint.connect(node_addr.clone(), self.alpn);
    let result = tokio::time::timeout(self.config.connect_timeout, connect_fut).await;

    let connection = match result {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) | Err(_) => {
            // Connection failed - try DNS fallback if we had a relay URL
            if node_addr.relay_url.is_some() {
                debug!(
                    alpn = ?std::str::from_utf8(self.alpn),
                    target = %node_id,
                    "relay connection failed, trying DNS discovery"
                );

                // Retry with just NodeId (triggers DNS discovery)
                let fallback_addr = NodeAddr::from(node_id);
                let fallback_fut = self.endpoint.connect(fallback_addr, self.alpn);
                tokio::time::timeout(self.config.connect_timeout, fallback_fut)
                    .await
                    .map_err(|_| PoolError::Timeout)?
                    .map_err(|e| PoolError::ConnectionFailed(e.to_string()))?
            } else {
                // No relay URL was provided, so DNS was already tried
                return Err(match result {
                    Ok(Err(e)) => PoolError::ConnectionFailed(e.to_string()),
                    Err(_) => PoolError::Timeout,
                    _ => unreachable!(),
                });
            }
        }
    };

    // ... existing caching logic ...
}
```

## Prerequisites

**Consider upgrading iroh first.** In iroh 0.94+:
- `NodeId` → `EndpointId`
- `NodeAddr` → `EndpointAddr`

Harbor's `endpoint_id` naming already aligns with the new terminology.

## Connection Flow

```
Service.get_connection(node_addr)
         │
         ▼
┌────────────────────┐
│ Cached & alive?    │──── Yes ──→ Return cached
└────────────────────┘
         │ No
         ▼
┌────────────────────────────────────────┐
│ 1. Try with provided NodeAddr          │
│    (includes relay URL if available)   │
│              │                         │
│              ▼                         │
│    ┌──────────────┐                    │
│    │ Success?     │─── Yes ──→ Cache & Return
│    └──────────────┘                    │
│              │ No                      │
│              ▼                         │
│ 2. Had relay URL?                      │
│    │                                   │
│    ├─ No ──→ Return error (DNS tried)  │
│    │                                   │
│    └─ Yes ──→ Fallback: DNS discovery  │
│              NodeAddr::from(node_id)   │
│              │                         │
│              ▼                         │
│         Success or Error               │
└────────────────────────────────────────┘
```

## Why This Works

1. **Fast path**: Stored relay URL works → immediate connection
2. **Fallback**: Relay fails → try DNS discovery via `dns.iroh.link`
3. **No redundant DNS**: If no relay URL was provided, DNS was already the first attempt

## Files to Modify

1. `core/src/network/pool.rs` - Add DNS fallback to `connect()`
2. Optionally: Extract a `Connection` struct if we want to encapsulate more behavior

## Integration with Services

Services already use `ConnectionPool`. The DNS fallback is transparent:

```rust
// SendService - no changes needed
let conn = self.pool.get_connection(node_addr).await?;
```

The pool handles the fallback internally.

## Optional: Connection Struct

If we want to add more behavior (stream management, metrics, etc.), we could wrap the iroh `Connection`:

```rust
pub struct Connection {
    inner: iroh::endpoint::Connection,
    node_id: NodeId,
    relay_url: Option<RelayUrl>,  // Could be updated after DNS discovery
}
```

But for now, the existing `ConnectionRef` wrapper may be sufficient.
