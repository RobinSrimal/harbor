# Harbor Security & Privacy

Harbor nodes provide offline delivery, but they are **untrusted storage**. The design assumes that any Harbor node may be malicious.

## What Harbor Nodes Can and Can't See

Harbor nodes **cannot read message contents**. They only see what they need to route and deliver packets:

Can see:

- Harbor routing IDs
- Sender and recipient endpoint IDs
- Packet size and timing

Cannot see:

- Message contents
- Topic membership lists
- Topic identifiers

## How This Works

You don't need to handle any extra encryption yourself. Harbor automatically:

- Encrypts payloads before storage
- Verifies sender identity
- Ensures only intended recipients can pull packets

## What This Means for Your App

- Harbor storage is safe for sensitive content.
- Metadata (who talked to whom, roughly when, and how much) is still visible to Harbor nodes.
- If you need stronger metadata privacy, add your own application-layer padding or batching.

See [Data Plane Security](./encryption.md) for direct-connection security.
