# Database Normalization

## Status

Normalization has been implemented. Peer references now use numeric `peer_id` FKs, and relay URL duplication has been removed from child tables.

## Context (Before)

The `peers` table is the canonical source of peer identity and connectivity info. Previously `endpoint_id` (32-byte BLOB) was stored directly in every table that referenced a peer. This meant:

- 32 bytes duplicated per reference (vs 4-8 bytes for an integer FK)
- Relay URL stored redundantly in `connection_list` and `harbor_nodes_cache`
- No `verified` flag — DHT candidates exist only in-memory, preventing unified peer management
- Missing FK constraints on several tables

## Current Schema

### Peers table (updated)

```sql
CREATE TABLE peers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    endpoint_id BLOB UNIQUE NOT NULL CHECK (length(endpoint_id) = 32),
    endpoint_address TEXT,
    address_timestamp INTEGER,
    last_latency_ms INTEGER,
    latency_timestamp INTEGER,
    last_seen INTEGER NOT NULL,
    relay_url TEXT,
    relay_url_last_success INTEGER,
    verified INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
)
```

Changes:
- `id INTEGER PRIMARY KEY AUTOINCREMENT` — new numeric PK
- `endpoint_id` becomes `UNIQUE NOT NULL` (no longer PK)
- `verified INTEGER` — false for candidates/unverified, true for confirmed-alive peers

### Tables migrated (endpoint_id BLOB → peer_id INTEGER)

| Table | Current column | After | FK |
|-------|---------------|-------|-----|
| `dht_routing` | `endpoint_id BLOB PK` | `peer_id INTEGER PK` | `REFERENCES peers(id)` |
| `topic_members` | `endpoint_id BLOB` (composite PK) | `peer_id INTEGER` | `REFERENCES peers(id)` |
| `topics` | `admin_id BLOB` | `admin_peer_id INTEGER` | `REFERENCES peers(id)` |
| `outgoing_recipients` | `endpoint_id BLOB` | `peer_id INTEGER` | `REFERENCES peers(id)` |
| `harbor_recipients` | `recipient_id BLOB` | `peer_id INTEGER` | `REFERENCES peers(id)` |
| `harbor_nodes_cache` | `node_id BLOB` + `relay_url TEXT` | `peer_id INTEGER` (drop relay_url) | `REFERENCES peers(id)` |
| `blob_recipients` | `endpoint_id BLOB` | `peer_id INTEGER` | `REFERENCES peers(id)` |
| `connection_list` | `peer_id BLOB` + `relay_url TEXT` | `peer_id INTEGER` (drop relay_url) | `REFERENCES peers(id)` |
| `pending_topic_invites` | `sender_id BLOB`, `admin_id BLOB` | `sender_peer_id INTEGER`, `admin_peer_id INTEGER` | `REFERENCES peers(id)` |
| `pending_decryption` | `sender_id BLOB` | `sender_peer_id INTEGER` | `REFERENCES peers(id)` |
| `harbor_cache` | `sender_id BLOB` | `sender_peer_id INTEGER` | `REFERENCES peers(id)` |
| `blobs` | `source_id BLOB` | `source_peer_id INTEGER` | `REFERENCES peers(id)` |
| `blob_sections` | `received_from BLOB` | `received_from_peer_id INTEGER` | `REFERENCES peers(id)` |

### What gets deduplicated

- `connection_list.relay_url` → removed (use `peers.relay_url` via JOIN)
- `harbor_nodes_cache.relay_url` → removed (use `peers.relay_url` via JOIN)

## Migration approach (Completed)

Applied incrementally, table by table, with `cargo check` + tests after each change. No backwards compatibility was needed (pre-production).

### Cleanup of unverified peers

Stale cleanup should only delete **unverified** peers. Verified peers (e.g., topic members, connected peers) should remain even if `last_seen` is old to avoid breaking membership state.

- Update `cleanup_stale_peers()` to delete rows where `verified = 0` **and** `last_seen` is older than the retention threshold.
- Verified peers stay regardless of `last_seen`; their volatile fields (address/relay/latency) can still expire independently.

## Relationship to Connector refactor

After normalization, the Connector's `connect()` could take a numeric `peer_id` instead of `&[u8; 32]`. The Connector would fetch both `endpoint_id` and `relay_url` from the peers table in a single query. This eliminates the separate relay lookup entirely.

DhtPool remains unchanged — it wraps ConnectionPool directly with its own DialInfo-based relay logic from the in-memory routing table.
