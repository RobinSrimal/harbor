#!/bin/bash
#
# Scenario: DHT Churn Test
#
# Tests DHT stability when nodes join and leave the network.
# Validates:
#   1. Initial DHT convergence (nodes discover each other)
#   2. DHT remains functional after node departures
#   3. DHT recovers after nodes restart
#
# Uses multiple bootstrap nodes for realistic peer discovery.
#

# Helper to extract dht_nodes from stats JSON
get_dht_nodes() {
    local stats="$1"
    echo "$stats" | grep -o '"dht_nodes":[0-9]*' | cut -d: -f2
}

# Start a bootstrap node (no external bootstrap)
start_bootstrap_only() {
    local n=$1
    local dir="$NODES_DIR/node-$n"
    local port=$((BASE_API_PORT + n - 1))
    
    mkdir -p "$dir"
    > "$dir/output.log"
    
    (cd "$dir" && exec "$HARBOR_BIN" --serve --no-default-bootstrap --db-path "./harbor.db" --api-port "$port" >> output.log 2>&1) &
    NODE_PIDS[$n]=$!
    
    info "Started bootstrap node-$n (PID: ${NODE_PIDS[$n]}, API: $port)"
}

# Get bootstrap arg for a node
get_bootstrap_arg() {
    local n=$1
    local port=$((BASE_API_PORT + n - 1))
    local bootstrap_json=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/bootstrap")
    echo "$bootstrap_json" | grep -o '"bootstrap_arg":"[^"]*"' | cut -d'"' -f4
}

# Start a node with multiple bootstrap nodes
start_node_multi_bootstrap() {
    local n=$1
    shift
    local bootstrap_args=("$@")
    
    local dir="$NODES_DIR/node-$n"
    local port=$((BASE_API_PORT + n - 1))
    
    mkdir -p "$dir"
    if [ -f "$dir/output.log" ] && [ -s "$dir/output.log" ]; then
        echo "" >> "$dir/output.log"
        echo "=== NODE RESTART ===" >> "$dir/output.log"
        echo "" >> "$dir/output.log"
    fi
    
    # Build bootstrap arguments
    local bootstrap_flags=""
    for arg in "${bootstrap_args[@]}"; do
        bootstrap_flags="$bootstrap_flags --bootstrap $arg"
    done
    
    (cd "$dir" && exec "$HARBOR_BIN" --serve $bootstrap_flags --db-path "./harbor.db" --api-port "$port" >> output.log 2>&1) &
    NODE_PIDS[$n]=$!
    
    info "Started node-$n (PID: ${NODE_PIDS[$n]}, API: $port)"
}

scenario_dht_churn() {
    log "=========================================="
    log "SCENARIO: DHT Churn Test"
    log "=========================================="
    
    local TEST_NODES=20
    local BOOTSTRAP_NODES="1 2 3"       # Multiple bootstrap nodes for better discovery
    local CHURN_NODES="5 6 7 8"         # Nodes that will churn (not bootstrap nodes)
    local MIN_DHT_NODES=5               # Minimum expected DHT nodes after convergence
    local CONVERGENCE_TIME=60           # DHT stabilization (~2 verification cycles, staggered batches add ~30s)
    
    # Track test results
    local initial_convergence_ok=true
    local post_churn_ok=true
    local recovery_ok=true
    
    # Phase 1: Start multiple bootstrap nodes
    log "Phase 1a: Starting bootstrap nodes $BOOTSTRAP_NODES..."
    for i in $BOOTSTRAP_NODES; do
        start_bootstrap_only $i
        sleep 0.5
    done
    
    # Wait for all bootstrap nodes to be ready
    for i in $BOOTSTRAP_NODES; do
        wait_for_api $i
    done
    
    # Wait for relay connections
    log "Waiting for bootstrap nodes relay connections (10s)..."
    sleep 10
    
    # Collect bootstrap args from all bootstrap nodes
    local bootstrap_args=()
    for i in $BOOTSTRAP_NODES; do
        local arg=$(get_bootstrap_arg $i)
        if [ -n "$arg" ] && [ "$arg" != ":" ]; then
            bootstrap_args+=("$arg")
            info "  Bootstrap node-$i: ${arg:0:20}..."
        fi
    done
    
    if [ ${#bootstrap_args[@]} -lt 2 ]; then
        warn "Could not get enough bootstrap args, falling back to single bootstrap"
    fi
    
    # Phase 1b: Start remaining nodes in staggered batches (realistic deployment)
    # In real deployments, nodes join gradually, not all at once
    log "Phase 1b: Starting nodes 4-$TEST_NODES in batches (realistic staggered join)..."
    
    # Batch 1: nodes 4-8
    log "  Starting batch 1 (nodes 4-8)..."
    for i in $(seq 4 8); do
        start_node_multi_bootstrap $i "${bootstrap_args[@]}"
        sleep 0.5
    done
    for i in $(seq 4 8); do
        wait_for_api $i
    done
    
    # Wait for batch 1 to connect and be added to bootstrap routing tables
    sleep 10
    
    # Batch 2: nodes 9-14
    log "  Starting batch 2 (nodes 9-14)..."
    for i in $(seq 9 14); do
        start_node_multi_bootstrap $i "${bootstrap_args[@]}"
        sleep 0.5
    done
    for i in $(seq 9 14); do
        wait_for_api $i
    done
    
    # Wait for batch 2 to be added
    sleep 10
    
    # Batch 3: nodes 15-20
    log "  Starting batch 3 (nodes 15-20)..."
    for i in $(seq 15 $TEST_NODES); do
        start_node_multi_bootstrap $i "${bootstrap_args[@]}"
        sleep 0.5
    done
    for i in $(seq 15 $TEST_NODES); do
        wait_for_api $i
    done
    
    # Phase 2: DHT Convergence
    log "Phase 2: DHT convergence (${CONVERGENCE_TIME}s)..."
    sleep $CONVERGENCE_TIME
    
    log "Checking initial DHT state..."
    local nodes_with_low_dht=0
    for i in $(seq 1 10); do
        local stats=$(api_stats $i)
        local dht_count=$(get_dht_nodes "$stats")
        info "  Node-$i: dht_nodes=$dht_count"
        
        if [ -z "$dht_count" ] || [ "$dht_count" -lt "$MIN_DHT_NODES" ]; then
            warn "  Node-$i has low DHT count: $dht_count (expected >= $MIN_DHT_NODES)"
            nodes_with_low_dht=$((nodes_with_low_dht + 1))
        fi
    done
    
    if [ "$nodes_with_low_dht" -gt 3 ]; then
        warn "Too many nodes with low DHT count: $nodes_with_low_dht"
        initial_convergence_ok=false
    fi
    
    # Phase 3: Churn - stop multiple nodes (not bootstrap nodes)
    log "Phase 3: Stopping nodes $CHURN_NODES..."
    for i in $CHURN_NODES; do
        stop_node $i
    done
    
    # Phase 4: Wait for DHT to stabilize after churn
    log "Phase 4: DHT recovery period (45s)..."
    sleep 45
    
    # Check remaining nodes still have DHT connectivity
    log "Checking DHT state after churn..."
    local remaining_nodes="1 2 3 4 9 10"
    local failed_nodes=0
    for i in $remaining_nodes; do
        local stats=$(api_stats $i)
        local dht_count=$(get_dht_nodes "$stats")
        info "  Node-$i: dht_nodes=$dht_count"
        
        if [ -z "$dht_count" ] || [ "$dht_count" -lt 1 ]; then
            warn "  Node-$i lost all DHT connections after churn!"
            failed_nodes=$((failed_nodes + 1))
        fi
    done
    
    if [ "$failed_nodes" -gt 0 ]; then
        warn "$failed_nodes nodes lost DHT connectivity after churn"
        post_churn_ok=false
    fi
    
    # Phase 5: Restart churned nodes with multiple bootstraps
    log "Phase 5: Restarting nodes $CHURN_NODES..."
    for i in $CHURN_NODES; do
        start_node_multi_bootstrap $i "${bootstrap_args[@]}"
        sleep 1
    done
    for i in $CHURN_NODES; do
        wait_for_api $i
    done
    
    # Phase 6: Final recovery check
    log "Phase 6: DHT recovery check (${CONVERGENCE_TIME}s)..."
    sleep $CONVERGENCE_TIME
    
    log "Checking final DHT state..."
    local recovered_ok=0
    local recovered_fail=0
    for i in $(seq 1 10); do
        local stats=$(api_stats $i)
        local dht_count=$(get_dht_nodes "$stats")
        info "  Node-$i: dht_nodes=$dht_count"
        
        if [ -n "$dht_count" ] && [ "$dht_count" -ge "$MIN_DHT_NODES" ]; then
            recovered_ok=$((recovered_ok + 1))
        else
            warn "  Node-$i failed to recover: dht_nodes=$dht_count (expected >= $MIN_DHT_NODES)"
            recovered_fail=$((recovered_fail + 1))
        fi
    done
    
    if [ "$recovered_fail" -gt 3 ]; then
        warn "Too many nodes failed to recover: $recovered_fail"
        recovery_ok=false
    fi
    
    # Summary
    log ""
    log "=== DHT Churn Test Results ==="
    
    if $initial_convergence_ok; then
        log "✅ Initial convergence: PASS (nodes discovered peers)"
    else
        warn "⚠️  Initial convergence: FAIL (nodes didn't discover enough peers)"
    fi
    
    if $post_churn_ok; then
        log "✅ Post-churn stability: PASS (remaining nodes maintained DHT connectivity)"
    else
        warn "⚠️  Post-churn stability: FAIL (some nodes lost DHT connectivity)"
    fi
    
    if $recovery_ok; then
        log "✅ Recovery after restart: PASS ($recovered_ok/10 nodes have good DHT)"
    else
        warn "⚠️  Recovery after restart: FAIL ($recovered_fail/10 nodes have poor DHT)"
    fi
    
    if $initial_convergence_ok && $post_churn_ok && $recovery_ok; then
        log ""
        log "✅ DHT Churn Test: ALL CHECKS PASSED"
    else
        log ""
        warn "⚠️  DHT Churn Test: SOME CHECKS FAILED"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: DHT Churn Test"
    log "=========================================="
}
