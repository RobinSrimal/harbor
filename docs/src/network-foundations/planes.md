# Control & Data Plane

Harbor splits the network into two planes so you can ship quickly without thinking about internals.

## Control Plane

The control plane decides **who can talk to whom**.

It covers:

- Peer connections (DM relationships)
- Topic membership (invites, join/leave, removals)
- Blocking and abuse controls

## Data Plane

The data plane is **what you do once you're connected**.

It covers:

- **Send**: messages and small payloads
- **Stream**: live media or low-latency data
- **Sync**: CRDT updates and state sync
- **Share**: large files

Every data-plane operation works for **topics** and **direct connections**. You choose the target with `Target::Topic(...)` or `Target::Dm(...)`.

## How It All Fits

1. Use the control plane to connect peers or join a topic.
2. Use the data plane to send, stream, sync, or share.
3. Harbor handles offline delivery automatically when it applies.

Next:

- [Topics](../control-plane/topics.md)
- [Direct Connections](../control-plane/direct-connections.md)
- [Send (Messages)](../data-plane/send.md)
