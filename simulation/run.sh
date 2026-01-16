#!/bin/bash
#
# Spawn Harbor nodes in parallel with HTTP API
# Node-1 acts as bootstrap for all other nodes
#
# Usage:
#   ./run.sh           # Run all nodes (1-10)
#   ./run.sh 3         # Run only nodes 1-3
#   ./run.sh 2 5       # Run nodes 2-5
#   ./run.sh --clean   # Clean logs/db before running

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
NODES_DIR="$SCRIPT_DIR/nodes"
BASE_API_PORT=9001
BOOTSTRAP_FILE="$SCRIPT_DIR/bootstrap.txt"

# Check for --clean flag
CLEAN_FIRST=false
for arg in "$@"; do
    if [ "$arg" == "--clean" ]; then
        CLEAN_FIRST=true
    fi
done

# Parse arguments (ignore --clean)
ARGS=()
for arg in "$@"; do
    if [ "$arg" != "--clean" ]; then
        ARGS+=("$arg")
    fi
done

START=${ARGS[0]:-1}
END=${ARGS[1]:-10}

# If only one argument, treat as "run 1 to N"
if [ ${#ARGS[@]} -eq 1 ]; then
    END=${ARGS[0]}
    START=1
fi

echo "Harbor Factory"
echo "=============="

# Clean if requested
if [ "$CLEAN_FIRST" = true ]; then
    echo "Cleaning previous data..."
    for i in $(seq $START $END); do
        rm -f "$NODES_DIR/node-$i/output.log"
        rm -f "$NODES_DIR/node-$i/harbor.db"
        rm -f "$NODES_DIR/node-$i/harbor.db-shm"
        rm -f "$NODES_DIR/node-$i/harbor.db-wal"
    done
    rm -f "$BOOTSTRAP_FILE"
    echo "Done."
    echo ""
fi

echo "Starting nodes $START to $END"
echo "Node-1 will be the bootstrap node"
echo ""

# Arrays to store PIDs
declare -a NODE_PIDS
declare -a TAIL_PIDS

# Cleanup function
cleanup() {
    echo ""
    echo "Stopping all nodes..."
    
    for pid in "${TAIL_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    
    for pid in "${NODE_PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null
        fi
    done
    
    wait 2>/dev/null
    rm -f "$BOOTSTRAP_FILE"
    echo "Done."
    exit 0
}

trap cleanup SIGINT SIGTERM

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

echo "Using: $HARBOR_BIN"
echo ""

# Start node-1 as bootstrap (if in range)
if [ $START -eq 1 ]; then
    NODE_DIR="$NODES_DIR/node-1"
    LOG_FILE="$NODE_DIR/output.log"
    API_PORT=$BASE_API_PORT
    
    mkdir -p "$NODE_DIR"
    > "$LOG_FILE"
    
    echo "Starting node-1 as BOOTSTRAP..."
    
    # Start without external bootstrap
    (
        cd "$NODE_DIR"
        exec "$HARBOR_BIN" --serve --no-default-bootstrap --db-path "./harbor.db" --api-port "$API_PORT" >> "$LOG_FILE" 2>&1
    ) &
    NODE_PIDS+=($!)
    
    # Tail with prefix
    (
        tail -f "$LOG_FILE" 2>/dev/null | while IFS= read -r line; do
            echo "[node-1 BOOTSTRAP] $line"
        done
    ) &
    TAIL_PIDS+=($!)
    
    # Wait for API and get bootstrap info
    echo "Waiting for node-1 API..."
    attempts=0
    while ! curl -s "http://127.0.0.1:$API_PORT/api/health" > /dev/null 2>&1; do
        sleep 0.5
        attempts=$((attempts + 1))
        if [ $attempts -gt 20 ]; then
            echo "Error: Node-1 API not responding"
            exit 1
        fi
    done
    
    # Wait for relay connection
    echo "Waiting for node-1 relay connection..."
    sleep 5
    
    # Get bootstrap info
    bootstrap_json=$(curl -s "http://127.0.0.1:$API_PORT/api/bootstrap")
    bootstrap_arg=$(echo "$bootstrap_json" | grep -o '"bootstrap_arg":"[^"]*"' | cut -d'"' -f4)
    
    if [ -n "$bootstrap_arg" ] && [ "$bootstrap_arg" != ":" ]; then
        echo "$bootstrap_arg" > "$BOOTSTRAP_FILE"
        echo "Bootstrap: ${bootstrap_arg:0:32}..."
    else
        endpoint_id=$(echo "$bootstrap_json" | grep -o '"endpoint_id":"[^"]*"' | cut -d'"' -f4)
        if [ -n "$endpoint_id" ]; then
            echo "$endpoint_id" > "$BOOTSTRAP_FILE"
            echo "Bootstrap (no relay): ${endpoint_id:0:32}..."
        else
            echo "Warning: Could not get bootstrap info"
        fi
    fi
    
    echo ""
    START=2  # Skip node-1 in the loop below
fi

# Spawn remaining nodes
for i in $(seq $START $END); do
    NODE_DIR="$NODES_DIR/node-$i"
    LOG_FILE="$NODE_DIR/output.log"
    API_PORT=$((BASE_API_PORT + i - 1))
    
    mkdir -p "$NODE_DIR"
    > "$LOG_FILE"
    
    echo "Starting node-$i (API: $API_PORT)..."
    
    # Read bootstrap info
    BOOTSTRAP_ARG=""
    if [ -f "$BOOTSTRAP_FILE" ]; then
        BOOTSTRAP_ARG=$(cat "$BOOTSTRAP_FILE")
    fi
    
    # Run node with bootstrap (--bootstrap replaces defaults)
    if [ -n "$BOOTSTRAP_ARG" ]; then
        (
            cd "$NODE_DIR"
            exec "$HARBOR_BIN" --serve --bootstrap "$BOOTSTRAP_ARG" --db-path "./harbor.db" --api-port "$API_PORT" >> "$LOG_FILE" 2>&1
        ) &
    else
        (
            cd "$NODE_DIR"
            exec "$HARBOR_BIN" --serve --db-path "./harbor.db" --api-port "$API_PORT" >> "$LOG_FILE" 2>&1
        ) &
    fi
    NODE_PIDS+=($!)
    
    # Tail with prefix
    (
        tail -f "$LOG_FILE" 2>/dev/null | while IFS= read -r line; do
            echo "[node-$i] $line"
        done
    ) &
    TAIL_PIDS+=($!)
    
    sleep 0.3
done

echo ""
echo "Started ${#NODE_PIDS[@]} nodes"
echo ""
echo "API Ports: $BASE_API_PORT - $((BASE_API_PORT + END - 1))"
echo "Bootstrap: $(cat $BOOTSTRAP_FILE 2>/dev/null | head -c 32)..."
echo ""
echo "=== Node Output ==="
echo ""

wait "${NODE_PIDS[@]}" 2>/dev/null
