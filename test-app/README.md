# Harbor Test App

A desktop application for testing and demonstrating Harbor Protocol, built with [Tauri](https://tauri.app/) + React + TypeScript.

## Features

- **Chat Interface**: Create topics, share invites, and send encrypted messages
- **Dashboard**: Monitor protocol stats, DHT routing table, and Harbor Node activity
- **Persistent Identity**: Same node identity across app restarts

## Prerequisites

- [Node.js](https://nodejs.org/) v18+
- [Rust](https://rustup.rs/) (for Tauri backend)
- Build tools (see main [README](../README.md#2-install-build-tools))

## Running

```bash
cd test-app
npm install
npm run tauri dev
```

## Usage

### Starting the Protocol

1. Launch the app
2. Click **Start Protocol**
3. Wait for relay connection (status indicator turns green)

### Creating a Topic

1. Click **Create Topic** in the sidebar
2. Click **Copy Invite** to share with others
3. Start sending messages

### Joining a Topic

1. Paste an invite code in the input field
2. Click **Join**
3. You'll see existing members and can start chatting

### Dashboard

Switch to the **Dashboard** tab to view:

- **Identity**: Your EndpointID and relay connection status
- **Network**: Online/offline status
- **DHT**: Routing table nodes and buckets
- **Topics**: Subscribed topics and members
- **Outgoing**: Pending messages and receipt status
- **Harbor**: Packets stored as a Harbor Node

## Project Structure

```
test-app/
├── src/                    # React frontend
│   ├── App.tsx             # Main chat interface
│   ├── App.css
│   ├── Dashboard.tsx       # Stats and monitoring
│   └── Dashboard.css
├── src-tauri/              # Tauri/Rust backend
│   ├── src/
│   │   ├── lib.rs          # Tauri commands (harbor-core integration)
│   │   └── main.rs         # Entry point
│   ├── Cargo.toml
│   └── tauri.conf.json     # Tauri configuration
├── package.json
└── vite.config.ts
```

## Tauri Commands

The backend exposes these commands to the frontend:

| Command | Description |
|---------|-------------|
| `start_protocol` | Start Harbor Protocol |
| `stop_protocol` | Stop the protocol |
| `create_topic` | Create a new topic |
| `join_topic` | Join using an invite |
| `leave_topic` | Leave a topic |
| `send_message` | Send message to topic |
| `poll_messages` | Get incoming messages |
| `get_stats` | Get protocol statistics |
| `get_dht_buckets` | Get DHT routing table |
| `get_topic_details` | Get topic member info |

## Development

### Frontend only (hot reload)

```bash
npm run dev
```

### Full app with backend

```bash
npm run tauri dev
```

### Build for production

```bash
npm run tauri build
```

## Logging

Set `RUST_LOG` to control backend logging:

```bash
RUST_LOG=info npm run tauri dev
RUST_LOG=harbor_core=debug npm run tauri dev
```

## Database

The app stores its database at `src-tauri/harbor_protocol.db` (SQLCipher encrypted).

To reset the node identity:

```bash
rm src-tauri/harbor_protocol.db*
```
