# Configuration

Harbor can be configured via command-line flags or programmatically via `ProtocolConfig`.

## Programmatic Configuration

```rust
use harbor_core::{Protocol, ProtocolConfig};

let config = ProtocolConfig::default()
    .with_db_path("my-node.db".into())
    .with_db_key(my_32_byte_key)
    .with_blob_path(".my-blobs".into())
    .with_max_storage(50 * 1024 * 1024 * 1024)  // 50 GB
    .with_bootstrap_nodes(vec![
        "abc123...:https://relay.example.com/".into()
    ]);

let protocol = Protocol::start(config).await?;
```

## Configuration Options

### Storage

| Option | Default | Description |
|--------|---------|-------------|
| `db_path` | `harbor.db` | SQLCipher database file |
| `db_key` | Generated | 32-byte encryption key |
| `blob_path` | `.harbor_blobs/` | Directory for file chunks |
| `max_storage_bytes` | 10 GB | Maximum harbor cache size |

### Network

| Option | Default | Description |
|--------|---------|-------------|
| `bootstrap_nodes` | Default nodes | DHT bootstrap nodes |

### Intervals

| Option | Default | Description |
|--------|---------|-------------|
| `dht_bootstrap_delay_secs` | 5 | Delay before DHT bootstrap |
| `dht_initial_refresh_interval_secs` | 10 | Initial DHT refresh rate |
| `dht_stable_refresh_interval_secs` | 120 | Stable DHT refresh rate |
| `dht_save_interval_secs` | 60 | DHT persistence interval |
| `harbor_pull_interval_secs` | 30 | Harbor pull frequency |
| `harbor_replication_interval_secs` | 60 | Harbor replication frequency |

## Testing Configuration

For development and testing, use shorter intervals:

```rust
let config = ProtocolConfig::for_testing()
    .with_db_key(test_key);
```

This sets:
- Faster DHT refresh (5-10 seconds)
- Faster harbor operations (5 seconds)
- Lower storage limits (10 MB)
- No default bootstrap nodes

## Database Key Management

The database key encrypts all local data. **If lost, data cannot be recovered.**

### Generating a Key

```bash
# Generate a random 32-byte key
openssl rand -hex 32
```

### Storing Securely

- Use environment variables (not command-line args which appear in `ps`)
- Use a secrets manager in production
- Consider hardware security modules for high-security deployments

```bash
# Good: environment variable
export HARBOR_DB_KEY=abc123...
./harbor --serve

# Bad: command-line arg (visible in process list)
./harbor --serve --db-key abc123...  # DON'T DO THIS
```
