# Bootstrap Nodes

Bootstrap nodes help new peers join the Harbor network by providing initial DHT contacts.

## What are Bootstrap Nodes?

When a new Harbor node starts, it needs to discover other peers. Bootstrap nodes are well-known, stable nodes that:

1. Are always online
2. Have populated DHT routing tables
3. Are hardcoded or configured as initial contacts

## Running Bootstrap Nodes

### Single Node

```bash
export HARBOR_DB_KEY=$(openssl rand -hex 32)
./harbor --serve \
  --no-default-bootstrap \
  --max-storage 100GB \
  --api-port 8080
```

Get its bootstrap info:
```bash
curl http://localhost:8080/api/bootstrap
```

### Production Setup (Multiple Nodes)

For production, run multiple bootstrap nodes for redundancy. Use the setup script:

```bash
# Clone the repo on your VPS
git clone https://github.com/RobinSrimal/harbor
cd harbor/core && cargo build --release
sudo cp target/release/harbor /usr/local/bin/

# Run setup (creates systemd services)
cd .. && sudo ./bootstrap/setup.sh 20 3001 50GB
```

This creates:
- 20 systemd services (`harbor-bootstrap-1` through `harbor-bootstrap-20`)
- Data directories at `/var/lib/harbor/bootstrap-{1..20}/`
- Generated Rust code to add to the codebase

## Adding Bootstrap Nodes to the Codebase

After running the setup script, copy the generated entries from `bootstrap_output/bootstrap_entries.rs` into `core/src/network/dht/internal/bootstrap.rs`:

```rust
pub fn default_bootstrap_nodes() -> Vec<BootstrapNode> {
    vec![
        // Harbor foundation nodes
        BootstrapNode::from_hex_with_relay(
            "1f5c436e4511ab2db15db837b827f237931bc722ede6f223c7f5d41b412c0cad",
            Some("harbor-bootstrap-1"),
            "https://euc1-1.relay.n0.iroh-canary.iroh.link./",
        ),
        // ... more nodes ...
    ]
    .into_iter()
    .flatten()
    .collect()
}
```

## Contributing Bootstrap Nodes

Community members can contribute bootstrap nodes:

1. Run the setup script on your VPS
2. Copy the generated entries
3. Submit a PR adding them to `bootstrap.rs`

Nodes are labeled by operator:
```rust
BootstrapNode::from_hex_with_relay(
    "abc123...",
    Some("alice-bootstrap-1"),  // Your name
    "https://...",
),
```

## Managing Bootstrap Nodes

### Check Status

```bash
systemctl status harbor-bootstrap-1
```

### View Logs

```bash
journalctl -u harbor-bootstrap-1 -f
```

### Restart

```bash
sudo systemctl restart harbor-bootstrap-1
```

### Stop All

```bash
for i in {1..20}; do sudo systemctl stop harbor-bootstrap-$i; done
```

## Storage Considerations

Bootstrap nodes also act as Harbor Nodes. Size `--max-storage` based on your disk:

| Nodes | Storage per node | Total disk needed |
|-------|-----------------|-------------------|
| 10 | 10GB | ~100GB |
| 20 | 50GB | ~1TB |
| 20 | 100GB | ~2TB |

## Firewall

Open the required ports:

```bash
# For ports 3001-3020
sudo ufw allow 3001:3020/udp  # QUIC
sudo ufw allow 3001:3020/tcp  # HTTP API (optional, for monitoring)
```

## Data Persistence

**Critical**: Bootstrap nodes must persist their identity.

Each node's identity is stored in its data directory (`/var/lib/harbor/bootstrap-N/`). If this is deleted, the node gets a new identity and the hardcoded bootstrap entries become stale.

**Never delete the data directories** for production bootstrap nodes.

## High Availability

For maximum uptime:

1. Run nodes across multiple VPS providers
2. Use different geographic regions
3. Monitor node health
4. Have a process to update bootstrap entries if nodes change
