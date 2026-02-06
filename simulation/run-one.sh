#!/bin/bash
#
# Run a single node in foreground (for debugging)
#
# Usage:
#   ./run-one.sh 1    # Run node-1 in foreground

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
NODES_DIR="$SCRIPT_DIR/nodes"
NODE_NUM=${1:-1}
NODE_DIR="$NODES_DIR/node-$NODE_NUM"

# Find the harbor-cli binary
HARBOR_BIN=""
if [ -f "$SCRIPT_DIR/../target/release/harbor-cli" ]; then
    HARBOR_BIN="$SCRIPT_DIR/../target/release/harbor-cli"
elif [ -f "$SCRIPT_DIR/../target/debug/harbor-cli" ]; then
    HARBOR_BIN="$SCRIPT_DIR/../target/debug/harbor-cli"
elif command -v harbor-cli &> /dev/null; then
    HARBOR_BIN="harbor-cli"
else
    echo "Error: harbor-cli not found"
    echo "Build with: cargo build -p harbor-core"
    exit 1
fi

echo "Running node-$NODE_NUM in foreground"
echo "Binary: $HARBOR_BIN"
echo "Directory: $NODE_DIR"
echo ""

mkdir -p "$NODE_DIR"
cd "$NODE_DIR"

# Database encryption key (same default as simulation nodes)
export HARBOR_DB_KEY="${HARBOR_DB_KEY:-486172626f724e6f64654b6579323032355f76315f7365727665725f6b657921}"

# Run in foreground with full output
RUST_LOG=info exec "$HARBOR_BIN" --serve --db-path "./harbor.db"

