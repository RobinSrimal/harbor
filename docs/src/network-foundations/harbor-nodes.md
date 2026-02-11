# Harbor & Offline Delivery

Harbor is the **store-and-forward** layer that keeps messages flowing when peers are offline.

## What It Does

- Tries **direct delivery** first
- Falls back to **Harbor storage** when recipients are offline
- Lets recipients **pull missed packets** when they reconnect

## Any Node Can Be a Harbor Node

Every node in the network can **act as a Harbor node** if it opts in to store packets. You can choose how much storage you want to contribute.

```bash
./harbor --serve --max-storage 100GB
```

In apps, this is controlled by `ProtocolConfig::with_max_storage(...)`.

## The Simple Flow

```
Sender online -> tries direct delivery
Recipient offline -> store on Harbor nodes
Recipient online -> pull missed packets
```

Harbor nodes store encrypted packets and cannot read the contents. See [Harbor Security & Privacy](../security/harbor-privacy.md).

## How Nodes Are Chosen

Harbor uses the DHT to find a small set of nearby nodes to store packets for a given topic or DM. The DHT exists primarily to support Harbor discovery.

See [DHT for Discovery](./dht.md).

## Resilience Basics

Harbor relies on replication, rate limits, and Proof of Work to stay reliable under load. The exact knobs are configurable, but you usually don't need to touch them.

See [Resilience: Harbor, DHT, Control](../resilience/harbor-control.md) for the high-level model.
