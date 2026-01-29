# Harbor

A topic-based message protocol for consumer apps built on [Iroh](https://iroh.computer/).

> **Early Stage Software**
>
> This project is in active development and **not ready for production use**.
>
> - **Security Warning:** The database encryption key is currently hardcoded. Do not use for sensitive data.
> - **Stability:** APIs may change. Expect bugs. More testing is needed.
> - **Try it out:** Run the [simulations](simulation/) or use the invite feature in the [test app](test-app/) to create group chats.
> - **Bootstrap Nodes:** If you'd like to volunteer to run a bootstrap node, please open an issue or reach out!

## Features

- **Topic-based messaging** - Send encrypted messages to all members of a topic
- **CRDT sync primitives** - SyncUpdate, SyncRequest, SyncResponse for collaborative apps
- **File sharing** - P2P distribution for large files with BLAKE3 chunking
- **Streaming** - Real-time audio/video streaming between peers
- **Offline delivery** - Harbor Nodes store messages for offline members
- **DHT routing** - Kademlia-style distributed hash table for peer discovery
- **End-to-end encryption** - All messages encrypted with topic-derived keys
- **Direct messages** - Peer-to-peer messaging, sync, sharing, and streaming without topics

## Quick Start

```bash
git clone git@github.com:RobinSrimal/harbor.git
cd harbor
cargo build
cargo run -p harbor-core
```

Harbor uses **bundled SQLCipher and OpenSSL** — no system libraries needed, just basic build tools and [Rust](https://rustup.rs/).

## Project Structure

```
harbor/
├── core/               # Protocol implementation
│   └── src/
│       ├── data/       # SQLCipher-encrypted storage + blob storage
│       ├── handlers/   # Incoming/outgoing message handlers
│       ├── network/    # Services (DHT, Send, Harbor, Share, Sync, Stream)
│       ├── protocol/   # Protocol struct and public API
│       ├── security/   # Cryptographic operations
│       ├── tasks/      # Background tasks (harbor pull, maintenance)
│       └── resilience/ # Rate limiting, PoW, storage limits
├── simulation/         # Integration test scenarios (bash scripts)
└── test-app/           # Tauri desktop app with Loro CRDT collaboration
```

## Test App

A Tauri desktop app for testing Harbor with real-time collaborative text editing via [Loro CRDT](https://loro.dev).

```bash
cd test-app
npm install
npm run tauri dev
```

Requires [Node.js](https://nodejs.org/) v18+.

## Testing

```bash
# Unit tests
cargo test -p harbor-core

# Integration tests (simulations)
cargo build -p harbor-core
cd simulation && ./simulate.sh
```

See [SIMULATIONS.md](SIMULATIONS.md) for details on available scenarios.

## Logging

Control verbosity with `RUST_LOG`:

```bash
RUST_LOG=info cargo run -p harbor-core
RUST_LOG=harbor_core=debug cargo run -p harbor-core
RUST_LOG=harbor_core::network::harbor=debug cargo run -p harbor-core
```

| Module Path | What It Logs |
|-------------|--------------|
| `harbor_core::network::harbor` | Harbor Node operations (store, pull, sync) |
| `harbor_core::network::dht` | DHT routing, lookups, candidate verification |
| `harbor_core::network::send` | Direct message sending |
| `harbor_core::network::stream` | Streaming sessions |
| `harbor_core::protocol` | Protocol API operations |

## Troubleshooting

**"file is not a database"** — SQLCipher version/settings changed. Delete the database and rebuild:
```bash
rm test-app/src-tauri/harbor_protocol.db
cargo build
```

**Slow first build** — Normal. SQLCipher and OpenSSL compile from source on the first build. Subsequent builds are fast.

**C compiler errors** — Install build tools (`xcode-select --install` on macOS).

## License

MIT OR Apache-2.0. See [LICENSE](LICENSE) for details.
