# Harbor Nodes

Harbor Nodes provide **store-and-forward** functionality, ensuring messages reach recipients even when they're offline.

## How It Works

```
┌────────────┐                          ┌────────────┐
│   Sender   │                          │  Recipient │
│  (online)  │                          │ (offline)  │
└─────┬──────┘                          └────────────┘
      │
      │ 1. Try direct delivery → fails (offline)
      │
      │ 2. Find Harbor Nodes for HarborID
      │
      ▼
┌────────────┐    ┌────────────┐    ┌────────────┐
│  Harbor    │    │  Harbor    │    │  Harbor    │
│  Node A    │    │  Node B    │    │  Node C    │
└─────┬──────┘    └─────┬──────┘    └─────┬──────┘
      │                 │                 │
      │ 3. Store packet on nearby nodes   │
      │                 │                 │
      └─────────────────┼─────────────────┘
                        │
                        │ 4. Recipient comes online
                        │
                        ▼
                  ┌────────────┐
                  │  Recipient │
                  │  (online)  │
                  └─────┬──────┘
                        │
                        │ 5. Pull packets from Harbor Nodes
                        ▼
                    Message delivered!
```

## HarborID

Packets are routed by **HarborID**, a 32-byte identifier:

- **Topics**: `HarborID = BLAKE3(TopicID)`
- **DMs**: `HarborID = BLAKE3(sort(EndpointID_A, EndpointID_B))`

Harbor Nodes don't know the actual TopicID or who the participants are - they only see the HarborID.

## Becoming a Harbor Node

Every Harbor node can act as a Harbor Node. Nodes "volunteer" to store packets for HarborIDs close to their NodeID (Kademlia-style).

Configure storage capacity:

```bash
./harbor --serve --max-storage 100GB
```

## Storage Flow

### Storing a Packet

1. Sender creates encrypted packet
2. Sender looks up Harbor Nodes for the HarborID via DHT
3. Sender sends `Store` request to multiple Harbor Nodes
4. Harbor Nodes validate PoW and store the packet
5. Harbor Nodes replicate to peers for redundancy

### Retrieving Packets

1. Recipient comes online
2. Recipient looks up Harbor Nodes for their topics/DMs
3. Recipient sends `Pull` request with their EndpointID
4. Harbor Nodes return matching packets
5. Packets older than 3 months are automatically expired

## Replication

Harbor Nodes replicate packets to ensure availability:

```
┌────────────┐         ┌────────────┐
│  Harbor    │◄───────►│  Harbor    │
│  Node A    │ sync    │  Node B    │
└────────────┘         └────────────┘
      │                      │
      │         ┌────────────┘
      │         │
      ▼         ▼
┌────────────────────┐
│    Harbor Node C   │
│    (newly joined)  │
└────────────────────┘
```

When a new Harbor Node joins the network, it syncs packets for its responsible HarborIDs from existing nodes.

## Rate Limiting

Harbor Nodes protect themselves from abuse:

- **Per-connection rate limits**: Max requests per time window
- **Per-HarborID limits**: Prevent single topic from using all storage
- **Proof of Work**: Senders must compute PoW (scales with volume)
- **Storage caps**: Automatic eviction when limits reached

## Packet Expiration

Packets have a 3-month TTL (configurable). Expired packets are automatically pruned during maintenance.

## Privacy Guarantees

See [Harbor Node Privacy](../security/harbor-privacy.md) for details on what Harbor Nodes can and cannot see.
