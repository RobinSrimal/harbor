# Harbor

A topic-based message protocol for consumer apps built on [Iroh](https://iroh.computer/).

> ⚠️ **Early Stage Software**
> 
> This project is in active development and **not ready for production use**.
> 
> - **Security Warning:** The database encryption key is currently hardcoded. Do not use for sensitive data.
> - **Stability:** APIs may change. Expect bugs. More testing is needed.
> - **Try it out:** Run the [simulations](simulation/) or use the invite feature in the [test app](test-app/) to create group chats.
> - **Bootstrap Nodes:** If you'd like to volunteer to run a bootstrap node, please open an issue or reach out!

## Crates

| Crate | Description | Status |
|-------|-------------|--------|
| [`harbor-core`](core/) | Core messaging protocol - peer connections, topics, Send mode, DHT, Harbor Nodes | ✅ Implemented |
| [`harbor-sync`](sync/) | CRDT synchronization for collaborative data structures | 🚧 Planned |
| [`harbor-stream`](stream/) | Real-time audio/video streaming | 🚧 Planned |

```
                       ┌──────────────┐
                       │  harbor-core │ 
                       └──────────────┘
                              ↑
              ┌───────────────┴───────────────┐
              │                               │
      ┌───────────────┐               ┌────────────────┐
      │  harbor-sync  │               │ harbor-stream  │
      └───────────────┘               └────────────────┘
```

## Quick Start

### 1. Clone the Repository

```bash
git clone git@github.com:RobinSrimal/harbor.git
cd harbor
```

### 2. Install Build Tools

Harbor uses **bundled SQLCipher and OpenSSL** - no system libraries needed! You just need basic build tools.

### 3. Install Rust

If you don't have Rust installed:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 4. Build the Project

```bash
cargo build
```

### 5. Run the CLI

```bash
cargo run -p harbor-core
```

---

## Test App (Tauri Desktop App)

### Prerequisites

- [Node.js](https://nodejs.org/) (v18+)
- [pnpm](https://pnpm.io/) or npm

### Running the Test App

```bash
cd test-app
npm install
npm run tauri dev
```

---

## Project Structure

```
harbor/
├── core/               # harbor-core: Foundation protocol
│   └── src/
│       ├── data/       # SQLCipher-encrypted storage
│       ├── handlers/   # Incoming/outgoing message handlers
│       ├── network/    # Network protocols (DHT, Send, Harbor)
│       ├── protocol/   # Core Protocol struct and API
│       ├── security/   # Cryptographic operations
│       ├── tasks/      # Background tasks (sync, maintenance)
│       └── resilience/ # Rate limiting, PoW, storage limits
├── sync/               # harbor-sync: CRDT synchronization (planned)
├── stream/             # harbor-stream: Audio/video streaming (planned)
└── test-app/           # Tauri desktop app for testing
    ├── src/            # React frontend
    └── src-tauri/      # Tauri/Rust backend
```

---

## Testing

### Running Tests

```bash
# Run all core protocol tests
cargo test -p harbor-core

# Run with output
cargo test -p harbor-core -- --nocapture

# Run a specific test
cargo test -p harbor-core test_name
```

---

## Logging

Harbor uses `tracing` for structured logging. Control verbosity with the `RUST_LOG` environment variable.

### Log Levels

| Level | Description |
|-------|-------------|
| `error` | Critical errors only |
| `warn` | Warnings and errors |
| `info` | General operational info (default) |
| `debug` | Detailed debugging info |
| `trace` | Very verbose (performance impact) |

### Examples
 
```bash
# Default - info level
RUST_LOG=info cargo run -p harbor-core

# Debug everything in harbor-core
RUST_LOG=harbor_core=debug cargo run -p harbor-core

# Debug specific modules
RUST_LOG=harbor_core::network::harbor=debug cargo run -p harbor-core
RUST_LOG=harbor_core::network::dht=debug cargo run -p harbor-core

# Combine multiple modules
RUST_LOG=info,harbor_core::network::harbor=debug,harbor_core::network::dht=trace cargo run

# Quiet mode (warnings only)
RUST_LOG=warn cargo run -p harbor-core
```

### Key Modules

| Module Path | What It Logs |
|-------------|--------------|
| `harbor_core::network::harbor` | Harbor Node operations (store, pull, sync) |
| `harbor_core::network::dht` | DHT routing, lookups, candidate verification |
| `harbor_core::network::send` | Direct message sending |
| `harbor_core::protocol` | Protocol API operations |

### In Test App

```bash
cd test-app
RUST_LOG=info,harbor_core=debug npm run tauri dev
```

---

## SQLCipher & OpenSSL Configuration

Harbor uses **bundled SQLCipher with vendored OpenSSL** which compiles everything from source. This means:

- ✅ **Zero system dependencies** - no need to install SQLCipher or OpenSSL
- ✅ **Works consistently** across macOS, Linux, and Windows
- ✅ **No environment variables** needed
- ⚠️ First build is slower (compiles SQLCipher + OpenSSL from source)

The configuration in `core/Cargo.toml`:

```toml
# Bundled with vendored OpenSSL (default, recommended)
rusqlite = { version = "0.38.0", features = ["bundled-sqlcipher-vendored-openssl"] }

# Alternative: Bundled SQLCipher only (requires system OpenSSL)
# rusqlite = { version = "0.38.0", features = ["bundled-sqlcipher"] }

# Alternative: System SQLCipher (requires both installed)
# rusqlite = { version = "0.38.0", features = ["sqlcipher"] }
```

---

## Troubleshooting

### "file is not a database" error

This happens when the SQLCipher version/settings change. Delete the database and rebuild:

```bash
rm test-app/src-tauri/harbor_protocol.db
cargo build
```

### Slow first build

This is normal! SQLCipher and OpenSSL are compiled from source on the first build (can take several minutes). Subsequent builds use cached artifacts and are much faster.

### Build fails with C compiler errors

Make sure you have build tools installed. On macOS:

```bash
xcode-select --install
```

---

## License

MIT OR Apache-2.0. See [LICENSE](LICENSE) for details.
