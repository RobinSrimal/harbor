# Harbor Protocol

Harbor is a decentralized peer-to-peer communication protocol designed for **offline-first messaging** with strong privacy guarantees.

## What is Harbor?

Harbor enables secure communication between peers without relying on centralized servers. Messages are delivered directly when peers are online, and stored on distributed **Harbor Nodes** when recipients are offline.

**Key features:**

- **Offline-first**: Messages reach recipients even when they're not online
- **End-to-end encrypted**: Only participants can read message content
- **Decentralized**: No central servers, no single point of failure
- **Topic-based**: Organize communication into invite-only groups
- **CRDT-ready**: Built-in primitives for collaborative applications

## How It Works

```
┌─────────────┐         ┌─────────────┐
│   Alice     │◄───────►│    Bob      │
│   (online)  │  direct │  (online)   │
└─────────────┘         └─────────────┘
       │
       │ Bob goes offline...
       ▼
┌─────────────┐         ┌─────────────┐
│   Alice     │────────►│ Harbor Node │
│   (online)  │  store  │  (storage)  │
└─────────────┘         └─────────────┘
                               │
       Bob comes back online...│
                               ▼
┌─────────────┐         ┌─────────────┐
│    Bob      │◄────────│ Harbor Node │
│   (online)  │  pull   │  (storage)  │
└─────────────┘         └─────────────┘
```

1. **Direct delivery**: When both peers are online, messages are sent directly via QUIC
2. **Harbor storage**: If a recipient is offline, messages are stored on Harbor Nodes
3. **Pull on reconnect**: When the recipient comes online, they pull missed messages

## Privacy by Design

Harbor Nodes store messages but **cannot read them**:

- Messages are encrypted with topic keys that Harbor Nodes don't have
- Harbor Nodes only know the `HarborID` (a hash), not the actual topic or participants
- Messages are signed, so tampering is detectable

See [Harbor Node Privacy](./security/harbor-privacy.md) for details.

## Use Cases

- **Private messaging**: 1:1 and group chat with offline delivery
- **Collaborative apps**: Real-time collaboration with CRDT support
- **File sharing**: P2P file distribution (BitTorrent-style)
- **IoT**: Device-to-device communication

## Getting Started

Ready to dive in? Start with the [Quick Start](./getting-started/quick-start.md) guide.
