# Harbor

A peer-to-peer messaging protocol with offline delivery, built on [Iroh](https://iroh.computer/).

> **Early Stage Software**
>
> This project is in active development and **not ready for production use**.
>
> - **Try it out:** Run the [simulations](simulation/) or use the invite feature in the [test app](test-app/) to create group chats.
> - **Bootstrap Nodes:** If you'd like to volunteer to run a bootstrap node, please open an issue or reach out!

## Features

- **Messaging** - Topic-based group messaging and direct peer-to-peer messages
- **Connection management** - Peer relationships with connect requests, blocking, and suggestions
- **CRDT sync primitives** - Built-in support for collaborative applications
- **File sharing** - P2P distribution for large files with BLAKE3 chunking
- **Streaming** - Real-time streaming transport layer between peers
- **Offline delivery** - Harbor Nodes store messages for offline members
- **DHT routing** - Kademlia-style distributed hash table for peer discovery
- **End-to-end encryption** - All messages signed and encrypted
- **Proof of Work** - Adaptive PoW with per-peer scaling for Harbor, Control, DHT, and Send protocols
- **Connection gating** - Fast authorization cache for incoming connections

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
│       ├── network/    # Services (DHT, Send, Harbor, Share, Sync, Stream, Control)
│       ├── protocol/   # Protocol struct and public API
│       ├── security/   # Cryptographic operations
│       ├── tasks/      # Background tasks (harbor pull, maintenance)
│       └── resilience/ # Rate limiting, PoW, storage limits
├── simulation/         # Integration test scenarios (bash scripts)
└── test-app/           # Tauri desktop app with Loro CRDT collaboration
```

## Test App

A desktop application for testing and demonstrating Harbor Protocol, built with [Tauri](https://tauri.app/) + React + TypeScript.

**Features:**
- **Chat Interface** — Create topics, share invite codes, and send end-to-end encrypted messages
- **Collaborative Editing** — Real-time text collaboration powered by [Loro CRDT](https://loro.dev)
- **Dashboard** — Monitor protocol stats, DHT routing table, topic membership, and Harbor Node activity
- **Persistent Identity** — Same node identity across app restarts (SQLCipher encrypted database)

```bash
cd test-app
npm install
npm run tauri dev
```

Requires [Node.js](https://nodejs.org/) v18+ and [Rust](https://rustup.rs/). See [test-app/README.md](test-app/README.md) for detailed usage instructions.

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
| `harbor_core::network::control` | Connection requests, topic invites, membership |
| `harbor_core::network::share` | File sharing operations |
| `harbor_core::network::sync` | Large sync responses (point-to-point) |
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
