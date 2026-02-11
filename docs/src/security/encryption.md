# Data Plane Security

The data plane is **peer-to-peer**. Once a connection or topic membership exists, Harbor uses encrypted transport and authenticated identities by default.

## Direct Connections

When two peers are connected, all data-plane traffic runs over encrypted transport (QUIC/TLS). Peers authenticate each other using their node identities.

## Topics

Topic members share encryption keys automatically through the control plane. You don't manage keys manually; you just send data to a topic target.

## What You Need to Do

- Keep your local database key safe (see [Local Data & Keys](./database.md)).
- Use the control plane to gate access to data-plane operations.
- Assume the network is untrusted and build your app UX accordingly.

For offline delivery security, see [Harbor Security & Privacy](./harbor-privacy.md).
