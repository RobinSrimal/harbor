# Harbor Simulations

The `simulation/` directory contains bash-based integration tests for Harbor protocol features.

## Overview

- Tests use the CLI's HTTP API (`/api/sync/update`, `/api/sync/request`, `/api/sync/respond`)
- Mock CRDT bytes are generated with simple hex encoding
- Verification is done by checking log files for event delivery
- Four sync scenarios test: basic updates, request/response, concurrent updates, offline recovery

## Quick Start

```bash
# Build first
cargo build -p harbor-core

# Run all scenarios
cd simulation && ./simulate.sh

# Run specific scenario
./simulate.sh --scenario sync-basic
```

## Running Simulations

```bash
cd simulation

# Run individual scenarios with --scenario flag
./simulate.sh --scenario sync-basic       # Test sync update transport
./simulate.sh --scenario sync-initial     # Test sync request/response
./simulate.sh --scenario sync-concurrent  # Test concurrent updates
./simulate.sh --scenario sync-offline     # Test offline recovery

# Run all sync scenarios
./simulate.sh --scenario sync

# Other scenario suites
./simulate.sh --scenario membership  # All membership tests
./simulate.sh --scenario share       # All file sharing tests
./simulate.sh --scenario control     # All control protocol tests
./simulate.sh --scenario full        # Run all scenarios

# Control protocol scenarios (peer connections, topic invites)
./simulate.sh --scenario control-connect       # Connection via invite token
./simulate.sh --scenario control-topic-invite  # Topic invite/accept flow
./simulate.sh --scenario control-suggest       # Peer introduction
./simulate.sh --scenario control-offline       # Offline delivery via Harbor
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

## Scenario Descriptions

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

### Control Connection Flow
1. Node-1 and Node-2 start
2. Node-1 generates a connect invite (token)
3. Node-2 connects using the invite
4. Node-1 auto-accepts (valid token)
5. Both nodes show `connected` state
6. Verify log events: "connection auto-accepted via token", "CONNECTION_ACCEPTED"

### Control Topic Invite Flow
1. Node-1 and Node-2 connect via Control ALPN
2. Node-1 creates a topic
3. Node-1 invites Node-2 to the topic
4. Node-2 receives TopicInviteReceived event
5. Node-2 accepts the invite
6. Both nodes have the topic membership
7. Verify log events: "TOPIC_INVITE_RECEIVED", "TOPIC_MEMBER_JOINED"

### Control Peer Suggestion
1. Node-1 connects to Node-2 and Node-3
2. Node-2 is NOT connected to Node-3
3. Node-1 suggests Node-3 to Node-2
4. Node-2 receives PeerSuggested event
5. Node-2 can initiate connection to Node-3
6. Verify log event: "PEER_SUGGESTED"

### Control Offline Delivery
1. Node-1, Node-2, Node-3 start and connect
2. Node-3 goes offline
3. Node-1 invites Node-3 to a topic (stored to Harbor)
4. Node-3 comes back online
5. Node-3 pulls TopicInvite from Harbor
6. Node-3 accepts the invite
7. All nodes have consistent topic membership

## Testing

```bash
# Test core library only (fast)
cargo test -p harbor-core

# Test with all features
cargo test

# Build CLI
cargo build -p harbor-core --bin harbor-cli

# Clean build (when dependencies change)
cargo clean && cargo build
```

## Viewing Logs

```bash
# View node logs during simulation
tail -f simulation/nodes/node-1/output.log

# Check all node logs
ls -la simulation/nodes/*/output.log
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

## HTTP API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/topic` | POST | Create topic, returns invite |
| `/api/join` | POST | Join topic (invite in body) |
| `/api/send` | POST | Send message (JSON body) |
| `/api/sync/update` | POST | Send sync update (CRDT delta) |
| `/api/sync/request` | POST | Request sync from all members |
| `/api/sync/respond` | POST | Respond with full state snapshot |
| `/api/stats` | GET | Node statistics |
| `/api/topics` | GET | List subscribed topics |
| `/api/bootstrap` | GET | Get this node's bootstrap info |
| `/api/health` | GET | Health check |

## Structure

```
simulation/
├── run.sh              # Manual node spawning
├── run-one.sh          # Run single node (debugging)
├── simulate.sh         # Automated scenarios
├── bootstrap.txt       # Generated: node-1's bootstrap info
├── lib/                # Helper functions
│   └── common.sh
├── scenarios/          # Test scenarios
│   ├── sync_basic.sh
│   ├── sync_initial.sh
│   ├── sync_concurrent.sh
│   ├── sync_offline.sh
│   ├── control_connect.sh
│   ├── control_topic_invite.sh
│   ├── control_suggest.sh
│   └── control_offline.sh
└── nodes/
    ├── node-1/         # Bootstrap node
    │   ├── harbor.db
    │   └── output.log
    ├── node-2/
    └── ...
```
