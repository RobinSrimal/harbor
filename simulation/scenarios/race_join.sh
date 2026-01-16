#!/bin/bash
#
# Scenario: Race - Message During Join
#
# Tests handling of messages received during join process.
#

scenario_race_join() {
    log "=========================================="
    log "SCENARIO: Race - Message During Join"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=5  # Small topic for this test
    
    start_bootstrap_node
    
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
    done
    
    log "Phase 2: Waiting for DHT convergence (30s)..."
    sleep 30
    
    log "Phase 3: Creating topic with nodes 1-3..."
    local invite=$(api_create_topic 1)
    local topic=$(echo "$invite" | head -c 64)
    
    for i in 2 3; do
        api_join_topic $i "$invite" > /dev/null
    done
    sleep 5
    
    log "Phase 4: Node-4 joins while Node-1 sends rapidly..."
    # Send messages in background while joining
    (
        for i in $(seq 1 5); do
            api_send 1 "$topic" "Race-Message-$i" > /dev/null
            sleep 0.2
        done
    ) &
    local send_pid=$!
    
    # Join simultaneously
    api_join_topic 4 "$invite" > /dev/null
    
    wait $send_pid
    
    log "Phase 5: Waiting for message delivery (20s)..."
    sleep 20
    
    log "Phase 6: Verifying node-4 handled race condition..."
    local received=0
    for i in $(seq 1 5); do
        if grep -q "Race-Message-$i" "$NODES_DIR/node-4/output.log" 2>/dev/null; then
            received=$((received + 1))
        fi
    done
    
    log "  Node-4 received $received/5 race messages"
    
    # Check node-4 is functioning (can receive new messages)
    api_send 2 "$topic" "Post-Race-Message" > /dev/null
    sleep 5
    
    if grep -q "Post-Race-Message" "$NODES_DIR/node-4/output.log" 2>/dev/null; then
        log "  ✅ Node-4 functioning after race"
        log "✅ Race condition handled gracefully!"
    else
        warn "⚠️  Node-4 may be in bad state after race"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Race - Message During Join"
    log "=========================================="
}

