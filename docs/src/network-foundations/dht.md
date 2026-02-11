# DHT for Discovery

Harbor uses a distributed hash table (DHT) as a **discovery layer** to find storage nodes.

## What You Need to Know

- The DHT is **internal plumbing**. Your app doesn't call it directly.
- It helps nodes **find Harbor nodes** to store and retrieve packets.

## Startup (Bootstrap)

When a node starts, it connects to bootstrap nodes and learns about the network. After that, discovery happens automatically.

```bash
# Use default bootstrap nodes
./harbor --serve

# Add a custom bootstrap
./harbor --serve --bootstrap abc123...:https://relay.example.com/
```

In apps, you configure bootstrap nodes through `ProtocolConfig`.

## Security & Resilience

DHT requests use stronger anti-abuse measures because discovery is a critical dependency for Harbor. See [Resilience: Harbor, DHT, Control](../resilience/harbor-control.md).
