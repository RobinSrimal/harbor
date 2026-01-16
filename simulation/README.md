# Harbor Simulation

Test harness for running multiple Harbor Protocol nodes and simulating network scenarios.

**Key feature:** Node-1 acts as the bootstrap node - no external bootstrap needed!

## Quick Start

```bash
# Build first
cargo build -p harbor-core

# Run simulation
./simulation/simulate.sh
```

## How Bootstrap Works

1. **Node-1 starts first** with `--no-default-bootstrap` (doesn't connect to external bootstrap)
2. **Node-1's info is saved** to `bootstrap.txt` via `/api/bootstrap`
3. **Other nodes start** with `--bootstrap <node-1-info>`
4. **All nodes discover each other** through node-1

This makes the simulation fully self-contained!

## Scripts

### `run.sh` - Manual Node Spawning

```bash
./run.sh              # Start nodes 1-10
./run.sh 3            # Start nodes 1-3
./run.sh 2 5          # Start nodes 2-5
./run.sh --clean      # Clear logs/db first
./run.sh --clean 3    # Clean and start 1-3
```

Each node gets:
- Database: `nodes/node-N/harbor.db`
- Log: `nodes/node-N/output.log`
- API: port `9000 + N` (node-1 = 9001)

### `simulate.sh` - Automated Scenarios

```bash
./simulate.sh                     # Run all scenarios
./simulate.sh --scenario basic    # Basic topic & messaging
./simulate.sh --scenario offline  # Offline node sync test
./simulate.sh --scenario churn    # DHT churn test
./simulate.sh --nodes 5           # Use 5 nodes
```

## CLI Bootstrap Flags

```bash
# Start as bootstrap (no external connection)
harbor-cli --serve --no-default-bootstrap

# Connect to a specific bootstrap node
harbor-cli --serve --bootstrap <endpoint_id>:<relay_url>

# Or just endpoint_id (no relay)
harbor-cli --serve --bootstrap <endpoint_id>
```

## HTTP API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/topic` | POST | Create topic, returns invite |
| `/api/join` | POST | Join topic (invite in body) |
| `/api/send` | POST | Send message (JSON body) |
| `/api/stats` | GET | Node statistics |
| `/api/topics` | GET | List subscribed topics |
| `/api/bootstrap` | GET | **Get this node's bootstrap info** |
| `/api/health` | GET | Health check |

### Bootstrap Info Example

```bash
curl -s localhost:9001/api/bootstrap
```

Returns:
```json
{
  "endpoint_id": "abc123...",
  "relay_url": "https://relay.example.com/",
  "bootstrap_arg": "abc123...:https://relay.example.com/"
}
```

Use `bootstrap_arg` with `--bootstrap`:
```bash
harbor-cli --serve --bootstrap "abc123...:https://relay.example.com/"
```

## Scenarios

### Basic Topic & Messaging
1. Node-1 starts as bootstrap
2. Nodes 2-N start and connect to node-1
3. DHT converges (nodes discover each other)
4. Create topic, all nodes join
5. Each node sends a message
6. Verify delivery

### Offline Node Sync
1. Start 3 nodes, create topic
2. Node-3 goes offline
3. Nodes 1 & 2 send messages
4. Messages replicate to Harbor Nodes
5. Node-3 comes back online
6. Node-3 receives missed messages via Harbor

### DHT Churn
1. Start 5 nodes
2. DHT stabilizes
3. Stop nodes 3 & 4
4. Observe DHT recovery
5. Restart nodes 3 & 4
6. Verify DHT reconverges

## Structure

```
simulation/
├── run.sh              # Manual node spawning
├── run-one.sh          # Run single node (debugging)
├── simulate.sh         # Automated scenarios
├── bootstrap.txt       # Generated: node-1's bootstrap info
├── README.md
└── nodes/
    ├── node-1/         # Bootstrap node
    │   ├── harbor.db
    │   └── output.log
    ├── node-2/
    └── ...
```

## Cleaning Up

```bash
# Clear all logs and databases
rm -f simulation/nodes/*/output.log
rm -f simulation/nodes/*/harbor.db*
rm -f simulation/bootstrap.txt

# Or use --clean flag
./run.sh --clean
```

## Tips

- **Check logs**: `tail -f simulation/nodes/node-1/output.log`
- **DHT takes time**: Allow 15-30s for convergence
- **Harbor replication**: Takes ~10s after message send
- **Debug one node**: Use `./run-one.sh 1` to run in foreground
- **Bootstrap issues**: Check `bootstrap.txt` has valid content
