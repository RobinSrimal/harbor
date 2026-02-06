# DHT & Discovery

Harbor uses a Kademlia-style Distributed Hash Table (DHT) for peer and Harbor Node discovery.

## Purpose

The DHT serves two main functions:

1. **Peer Discovery**: Find how to reach a peer by their EndpointID
2. **Harbor Node Discovery**: Find which nodes store packets for a HarborID

## How It Works

### Node IDs

Every Harbor node has a 256-bit NodeID (derived from their Ed25519 public key). Nodes are organized by XOR distance:

```
distance(A, B) = A XOR B
```

Nodes closer in XOR distance are "neighbors" in the DHT.

### Routing Table

Each node maintains a routing table of known peers, organized into **k-buckets**:

- Bucket 0: Nodes with distance 2⁰ to 2¹
- Bucket 1: Nodes with distance 2¹ to 2²
- ...
- Bucket 255: Nodes with distance 2²⁵⁵ to 2²⁵⁶

Each bucket holds up to `k` nodes (default: 20).

### Lookups

To find a node or value:

1. Calculate the target ID's XOR distance from known nodes
2. Query the `α` closest nodes (default: 3)
3. They return their closest known nodes
4. Repeat until no closer nodes are found

```
Query: Find nodes near ID "abc123..."

Step 1: Ask my closest known nodes
        → They return nodes closer to target

Step 2: Ask those nodes
        → They return even closer nodes

Step 3: Repeat until converged
        → Found the closest nodes!
```

## Bootstrap

New nodes join the network by:

1. Connecting to bootstrap nodes (hardcoded or configured)
2. Looking up their own NodeID
3. Populating their routing table from responses

```bash
# Using default bootstrap nodes
./harbor --serve

# Using custom bootstrap
./harbor --serve --bootstrap abc123...:https://relay.example.com/
```

## Harbor Node Discovery

To find Harbor Nodes for a topic:

1. Compute `HarborID = BLAKE3(TopicID)`
2. Look up nodes closest to HarborID
3. Those nodes are responsible for storing packets

Nodes "close" to a HarborID volunteer to store packets for it.

## Persistence

The routing table is periodically saved to the database and restored on restart. This speeds up rejoining the network.

## Refresh

Routing tables are periodically refreshed:

- **Initial refresh**: Every 10 seconds (finding peers quickly)
- **Stable refresh**: Every 120 seconds (maintenance)

## Configuration

| Setting | Default | Description |
|---------|---------|-------------|
| `k` (bucket size) | 20 | Max nodes per bucket |
| `α` (concurrency) | 3 | Parallel queries |
| `dht_bootstrap_delay_secs` | 5 | Delay before bootstrap |
| `dht_initial_refresh_interval_secs` | 10 | Fast refresh on startup |
| `dht_stable_refresh_interval_secs` | 120 | Normal refresh |
| `dht_save_interval_secs` | 60 | Persistence interval |

## Security

DHT operations require Proof of Work to prevent:

- Sybil attacks (creating many fake nodes)
- Eclipse attacks (surrounding a target)
- DHT pollution (storing garbage data)

See [Proof of Work](../security/proof-of-work.md) for details.
