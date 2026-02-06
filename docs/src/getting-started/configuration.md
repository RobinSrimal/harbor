# Configuration

All configuration is done via `ProtocolConfig` when starting the protocol.

## Basic Example

```rust
use harbor_core::{Protocol, ProtocolConfig};

let config = ProtocolConfig::default()
    .with_db_key(my_32_byte_key)
    .with_db_path("my-app.db".into())
    .with_max_storage(50 * 1024 * 1024 * 1024);  // 50 GB

let protocol = Protocol::start(config).await?;
```

## All Options

### Storage

| Method | Default | Description |
|--------|---------|-------------|
| `.with_db_key(key)` | None (required) | 32-byte database encryption key |
| `.with_db_path(path)` | `harbor_protocol.db` | SQLCipher database file |
| `.with_blob_path(path)` | `.harbor_blobs/` | Directory for file chunks |
| `.with_max_storage(bytes)` | 10 GB | Maximum Harbor cache size |

### Network

| Method | Default | Description |
|--------|---------|-------------|
| `.with_bootstrap_node(id)` | - | Add a bootstrap node |
| `.with_bootstrap_nodes(vec)` | Default nodes | Replace bootstrap nodes |

### Harbor Node Timing

| Method | Default | Description |
|--------|---------|-------------|
| `.with_harbor_pull_interval(secs)` | 60 | Pull from Harbor Nodes |
| `.with_harbor_sync_interval(secs)` | 300 | Harbor-to-Harbor sync |
| `.with_replication_factor(n)` | 3 | Replicate to N nodes |
| `.with_max_replication_attempts(n)` | 6 | Max nodes to try |
| `.with_harbor_pull_max_nodes(n)` | 5 | Max nodes to pull from |
| `.with_harbor_pull_early_stop(n)` | 2 | Stop after N empty responses |

### DHT Timing

| Method | Default | Description |
|--------|---------|-------------|
| `.with_dht_bootstrap_delay(secs)` | 5 | Delay before bootstrap |
| `.with_dht_initial_refresh_interval(secs)` | 10 | Fast refresh on startup |
| `.with_dht_stable_refresh_interval(secs)` | 120 | Normal refresh interval |
| `.with_dht_save_interval(secs)` | 60 | Persist routing table |

### Proof of Work

| Method | Description |
|--------|-------------|
| `.with_harbor_pow(config)` | Harbor ALPN PoW settings |
| `.with_control_pow(config)` | Control ALPN PoW settings |
| `.with_dht_pow(config)` | DHT ALPN PoW settings |
| `.with_send_pow(config)` | Send ALPN PoW settings |
| `.with_pow_disabled()` | Disable all PoW (testing only) |

## Testing Configuration

For development and testing:

```rust
let config = ProtocolConfig::for_testing()
    .with_db_key(test_key);
```

This sets:
- Shorter intervals (faster feedback)
- Higher replication (more reliable in small networks)
- Disabled Proof of Work (faster operations)
- No default bootstrap nodes (isolated testing)
- Lower storage limits (10 MB)

## Key Management

See [Database & Keyring](../security/database.md) for:
- Generating secure keys
- Using OS keyring integration
- Deriving keys from passphrases
