#!/bin/bash
#
# Harbor Simulation - Node Management
#
# This file contains functions for:
# - Starting/stopping nodes
# - Cleanup and process management
# - Bootstrap node handling
#

# Node management
declare -a NODE_PIDS

# Find binary
find_harbor_binary() {
    if [ -f "$SCRIPT_DIR/../target/release/harbor-cli" ]; then
        HARBOR_BIN="$SCRIPT_DIR/../target/release/harbor-cli"
    elif [ -f "$SCRIPT_DIR/../target/debug/harbor-cli" ]; then
        HARBOR_BIN="$SCRIPT_DIR/../target/debug/harbor-cli"
    else
        fail "harbor-cli not found. Run: cargo build -p harbor-core"
    fi
}

cleanup() {
    echo ""
    log "Cleaning up..."
    
    # Kill tracked PIDs
    for pid in "${NODE_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    
    # Also kill any processes on our ports
    for i in $(seq 1 $NODE_COUNT); do
        local port=$((BASE_API_PORT + i - 1))
        local pid=$(lsof -ti :$port 2>/dev/null)
        if [ -n "$pid" ]; then
            kill $pid 2>/dev/null || true
        fi
    done
    
    wait 2>/dev/null
    rm -f "$BOOTSTRAP_FILE"
    log "Done."
}

# Kill any processes using our API ports
kill_stale_processes() {
    # Kill any harbor-cli processes from previous runs
    pkill -f "harbor-cli.*--api-port" 2>/dev/null || true
    
    # Also kill anything on our ports
    for i in $(seq 1 $NODE_COUNT); do
        local port=$((BASE_API_PORT + i - 1))
        # Find and kill processes using this port
        local pid=$(lsof -ti :$port 2>/dev/null)
        if [ -n "$pid" ]; then
            kill $pid 2>/dev/null || true
            info "Killed stale process on port $port (PID: $pid)"
        fi
    done
    
    # Give processes time to die
    sleep 1
}

# Clear old data
clear_data() {
    log "Clearing previous simulation data..."
    
    # First kill any stale processes
    kill_stale_processes
    
    # Clear ALL existing node directories (not just up to NODE_COUNT)
    # This handles cleanup after scale tests that use more nodes
    for dir in "$NODES_DIR"/node-*/; do
        if [ -d "$dir" ]; then
            rm -f "$dir/output.log"
            rm -f "$dir/harbor.db"
            rm -f "$dir/harbor.db-shm"
            rm -f "$dir/harbor.db-wal"
            # Clear blob storage folders
            rm -rf "$dir/blobs"
            rm -rf "$dir/.harbor_blobs"
        fi
    done
    rm -f "$BOOTSTRAP_FILE"
}

# Start the bootstrap node (node-1)
start_bootstrap_node() {
    local dir="$NODES_DIR/node-1"
    local port=$BASE_API_PORT
    
    mkdir -p "$dir"
    > "$dir/output.log"
    
    log "Starting node-1 as BOOTSTRAP (no external bootstrap)..."
    
    # Use RUST_LOG from environment, default to debug if not set
    local log_level="${RUST_LOG:-debug}"
    
    # Start with --no-default-bootstrap so it doesn't try to connect to hardcoded bootstrap
    # Use --testing for shorter intervals during simulation
    # Use --blob-path ./blobs so shared files are visible in node folders
    (cd "$dir" && RUST_LOG="$log_level" exec "$HARBOR_BIN" --serve --testing --no-default-bootstrap --db-path "./harbor.db" --blob-path "./blobs" --api-port "$port" >> output.log 2>&1) &
    NODE_PIDS[1]=$!
    disown ${NODE_PIDS[1]} 2>/dev/null  # Suppress "Terminated" messages when killed
    
    info "Started node-1 (PID: ${NODE_PIDS[1]}, API: $port)"
    
    # Wait for API to be ready
    wait_for_api 1
    
    # Wait for relay connection (important for bootstrap info)
    log "Waiting for node-1 relay connection..."
    sleep 5
    
    # Get bootstrap info and save to file
    local bootstrap_json=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/bootstrap")
    local bootstrap_arg=$(echo "$bootstrap_json" | grep -o '"bootstrap_arg":"[^"]*"' | cut -d'"' -f4)
    
    if [ -z "$bootstrap_arg" ] || [ "$bootstrap_arg" = ":" ]; then
        warn "Could not get complete bootstrap info, using endpoint_id only"
        local endpoint_id=$(echo "$bootstrap_json" | grep -o '"endpoint_id":"[^"]*"' | cut -d'"' -f4)
        echo "$endpoint_id" > "$BOOTSTRAP_FILE"
    else
        echo "$bootstrap_arg" > "$BOOTSTRAP_FILE"
    fi
    
    log "Bootstrap info saved: $(cat $BOOTSTRAP_FILE | head -c 32)..."
}

# Start a regular node (uses bootstrap from node-1)
start_node() {
    local n=$1
    local dir="$NODES_DIR/node-$n"
    local port=$((BASE_API_PORT + n - 1))
    
    mkdir -p "$dir"
    # Append separator when restarting (don't truncate)
    if [ -f "$dir/output.log" ] && [ -s "$dir/output.log" ]; then
        echo "" >> "$dir/output.log"
        echo "=== NODE RESTART ===" >> "$dir/output.log"
        echo "" >> "$dir/output.log"
    fi
    
    # Use RUST_LOG from environment, default to debug if not set
    local log_level="${RUST_LOG:-debug}"
    
    # Read bootstrap info
    local bootstrap_arg=""
    if [ -f "$BOOTSTRAP_FILE" ]; then
        bootstrap_arg=$(cat "$BOOTSTRAP_FILE")
    fi
    
    if [ -n "$bootstrap_arg" ]; then
        # --bootstrap replaces defaults, no need for --no-default-bootstrap
        # Use --testing for shorter intervals during simulation
        # Use --blob-path ./blobs so shared files are visible in node folders
        (cd "$dir" && RUST_LOG="$log_level" exec "$HARBOR_BIN" --serve --testing --bootstrap "$bootstrap_arg" --db-path "./harbor.db" --blob-path "./blobs" --api-port "$port" >> output.log 2>&1) &
    else
        # No custom bootstrap, use defaults
        (cd "$dir" && RUST_LOG="$log_level" exec "$HARBOR_BIN" --serve --testing --db-path "./harbor.db" --blob-path "./blobs" --api-port "$port" >> output.log 2>&1) &
    fi
    NODE_PIDS[$n]=$!
    disown ${NODE_PIDS[$n]} 2>/dev/null  # Suppress "Terminated" messages when killed
    
    info "Started node-$n (PID: ${NODE_PIDS[$n]}, API: $port)"
}

# Stop a node
stop_node() {
    local n=$1
    local port=$((BASE_API_PORT + n - 1))
    
    # Kill by tracked PID
    if [ -n "${NODE_PIDS[$n]}" ]; then
        kill "${NODE_PIDS[$n]}" 2>/dev/null || true
        NODE_PIDS[$n]=""
    fi
    
    # Also kill by port in case PID tracking failed
    local pid=$(lsof -ti :$port 2>/dev/null)
    if [ -n "$pid" ]; then
        kill $pid 2>/dev/null || true
    fi
    
    # Wait for port to be free
    local attempts=0
    while lsof -ti :$port >/dev/null 2>&1; do
        sleep 0.2
        attempts=$((attempts + 1))
        if [ $attempts -gt 10 ]; then
            warn "Port $port still in use after 2s"
            break
        fi
    done
    
    info "Stopped node-$n"
}

# Helper functions for discovery scenario
wait_for_relay() {
    local n=$1
    sleep 5  # Simple wait for relay connection
}

save_bootstrap() {
    local n=$1
    local port=$(api_port $n)
    local bootstrap_json=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/bootstrap")
    local bootstrap_arg=$(echo "$bootstrap_json" | grep -o '"bootstrap_arg":"[^"]*"' | cut -d'"' -f4)
    
    if [ -n "$bootstrap_arg" ] && [ "$bootstrap_arg" != ":" ]; then
        echo "$bootstrap_arg" > "$BOOTSTRAP_FILE"
    fi
}

