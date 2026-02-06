# Running a Node

Detailed guide for running and configuring a Harbor node.

## Command Line Options

```bash
harbor --serve [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--serve`, `-s` | Run in serve mode (required) |
| `--db-path <PATH>` | Database path (default: `harbor.db`) |
| `--blob-path <PATH>` | Blob storage path (default: `.harbor_blobs/`) |
| `--api-port <PORT>` | Enable HTTP API on this port |
| `--max-storage <SIZE>` | Max harbor storage (default: 10GB) |
| `--bootstrap <ID:RELAY>` | Custom bootstrap node |
| `--no-default-bootstrap` | Start without bootstrap (for first node) |
| `--testing` | Use shorter intervals for testing |
| `--help`, `-h` | Show help |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `HARBOR_DB_KEY` | Database encryption key (64 hex chars, **required**) |
| `RUST_LOG` | Log level (e.g., `info`, `debug`) |

## Examples

### Basic Node

```bash
export HARBOR_DB_KEY=$(openssl rand -hex 32)
./harbor --serve --api-port 8080
```

### Custom Paths

```bash
./harbor --serve \
  --db-path /var/lib/harbor/node.db \
  --blob-path /var/lib/harbor/blobs \
  --api-port 8080
```

### High-Capacity Storage Node

```bash
./harbor --serve \
  --max-storage 100GB \
  --api-port 8080
```

### Custom Bootstrap

```bash
./harbor --serve \
  --bootstrap abc123...:https://relay.example.com/ \
  --api-port 8080
```

### First Node (No Bootstrap)

For the very first node in a new network:

```bash
./harbor --serve --no-default-bootstrap --api-port 8080
```

## Storage

### Database

Harbor uses SQLCipher (encrypted SQLite) for persistence. The database stores:

- Node identity (Ed25519 keypair)
- Topic memberships
- DHT routing table
- Harbor packet cache
- Connection state

### Blob Storage

Large files (for the Share protocol) are stored in the blob directory as content-addressed chunks.

## Logging

Control log verbosity:

```bash
# Info level (default)
RUST_LOG=info ./harbor --serve

# Debug specific modules
RUST_LOG=harbor_core::network::harbor=debug ./harbor --serve

# Multiple modules
RUST_LOG=info,harbor_core::network=debug ./harbor --serve
```

## Running as a Service

For production, run Harbor as a systemd service:

```ini
[Unit]
Description=Harbor Node
After=network.target

[Service]
Type=simple
User=harbor
Environment=HARBOR_DB_KEY=your-64-char-hex-key
ExecStart=/usr/local/bin/harbor --serve --api-port 8080
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

See [Bootstrap Nodes](../operating/bootstrap.md) for production deployment scripts.
