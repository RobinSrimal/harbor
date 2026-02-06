# Bootstrap Node Setup

Sets up N bootstrap nodes as **persistent systemd services**. The nodes created by this script ARE your production bootstrap nodes - they keep their identity across restarts.

### What it does

1. Creates a `harbor` system user
2. Creates data directories at `/var/lib/harbor/bootstrap-{1..N}`
3. Creates systemd service files for each node
4. Starts and enables all services (auto-start on boot)
5. Collects endpoint IDs and relay URLs
6. Generates Rust code entries to add to the codebase

### Prerequisites

On your VPS (Debian/Ubuntu):
```bash
# Install dependencies
apt update && apt install -y jq curl

# Install Rust and build
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Clone and build
git clone <your-repo-url>
cd harbor/core
cargo build --release

# Copy binary to system path
sudo cp target/release/harbor /usr/local/bin/
```

### Usage

```bash
cd harbor

# Run setup (requires root for systemd)
sudo ./bootstrap/setup.sh

# Customize: [num_nodes] [start_port] [max_storage]
sudo ./bootstrap/setup.sh 20 3001        # 20 nodes, default storage (10GB)
sudo ./bootstrap/setup.sh 20 3001 50GB   # 20 nodes, 50GB storage each
sudo ./bootstrap/setup.sh 10 4001 100GB  # 10 nodes, 100GB storage each
```

The script will prompt for your operator name (e.g., `harbor`, `alice`).

**Storage sizing:** Bootstrap nodes also act as harbor storage nodes. Set `max_storage` based on your VPS disk space. For 20 nodes with 50GB each, you need ~1TB disk.

### Output

After running, you'll find in `./bootstrap_output/`:
- `bootstrap_list.txt` - Raw list of `endpoint_id:relay_url`
- `bootstrap_entries.rs` - Rust code to add to `bootstrap.rs`

### Managing nodes

```bash
# Check status
systemctl status harbor-bootstrap-1

# View logs
journalctl -u harbor-bootstrap-1 -f

# Restart a node
systemctl restart harbor-bootstrap-1

# Stop all nodes
for i in {1..20}; do sudo systemctl stop harbor-bootstrap-$i; done

# Start all nodes
for i in {1..20}; do sudo systemctl start harbor-bootstrap-$i; done
```

### Data persistence

Each node stores its identity and data in `/var/lib/harbor/bootstrap-N/`. As long as this directory exists, the node keeps its identity (endpoint ID) across restarts.

**Never delete these directories** - if you do, the node gets a new identity and the hardcoded bootstrap entries become stale.

### Contributing bootstrap nodes

Multiple operators can contribute nodes:

1. Run the script on your VPS
2. Copy entries from `bootstrap_output/bootstrap_entries.rs`
3. Add to `core/src/network/dht/internal/bootstrap.rs` inside the `vec![]`
4. Submit a PR

Example structure in `bootstrap.rs`:
```rust
pub fn default_bootstrap_nodes() -> Vec<BootstrapNode> {
    vec![
        // Harbor foundation nodes
        BootstrapNode::from_hex_with_relay(
            "1f5c436e...",
            Some("harbor-bootstrap-1"),
            "https://...",
        ),
        // ... more harbor nodes ...

        // Community: alice
        BootstrapNode::from_hex_with_relay(
            "abc123...",
            Some("alice-bootstrap-1"),
            "https://...",
        ),
        // ... more alice nodes ...
    ]
    .into_iter()
    .flatten()
    .collect()
}
```

### Firewall

Make sure ports are open for your bootstrap nodes:
```bash
# If using ufw
sudo ufw allow 3001:3020/udp  # QUIC
sudo ufw allow 3001:3020/tcp  # HTTP API
```
