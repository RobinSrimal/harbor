# Harbor Protocol

Harbor is a decentralized peer-to-peer communication protocol for **offline-first apps**. It lets your users communicate directly when they're online and keeps delivery working when they're not.

## What Harbor Gives You

- **Offline delivery** without centralized servers
- **Direct peer-to-peer** when possible
- **Topics and 1:1 connections** as the core relationship model
- **Data-plane primitives** for messaging, streaming, sync, and file sharing

## Mental Model

Think of Harbor as three layers:

1. **Harbor (offline delivery)**
   Any node can **act as a Harbor node** if it opts in to store packets.

2. **DHT (discovery)**
   The DHT exists mainly to **find Harbor nodes** (and peers). It is a support system, not your app API.

3. **Planes**
   - **Control plane**: who can talk to whom (connections, topics)
   - **Data plane**: what you do once connected (send, stream, sync, share)

## Quick Flow

```
Sender online -> direct delivery
Recipient offline -> Harbor storage
Recipient online -> pull from Harbor nodes
```

Harbor handles the routing and storage automatically. Your app just uses the protocol APIs.

## Where to Start

- [Quick Start](./getting-started/quick-start.md)
- [Harbor & Offline Delivery](./network-foundations/harbor-nodes.md)
- [Control & Data Planes](./network-foundations/planes.md)
