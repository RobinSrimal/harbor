#!/bin/bash
#
# Scenario: Concurrent Senders
#
# Tests that multiple nodes sending simultaneously don't cause issues.
#

scenario_concurrent() {
    log "=========================================="
    log "SCENARIO: Concurrent Senders"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=10  # Max 10 topic members
    
    start_bootstrap_node
    
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
    done
    
    log "Phase 2: Waiting for DHT convergence (45s)..."
    sleep 45
    
    log "Phase 3: Creating topic and joining nodes 1-$TOPIC_MEMBERS..."
    local invite=$(api_create_topic 1)
    local topic=$(echo "$invite" | head -c 64)
    
    # Join nodes one by one, getting fresh invite each time so all members are known
    local current_invite="$invite"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        sleep 1  # Wait for join to propagate
        current_invite=$(api_get_invite 1 "$topic")  # Get fresh invite with all current members
    done
    sleep 5  # Final propagation time
    
    log "Phase 4: All topic members send simultaneously..."
    # Fire all sends in parallel (don't wait for curl - verify via logs)
    for i in $(seq 1 $TOPIC_MEMBERS); do
        api_send $i "$topic" "Concurrent-from-Node-$i" > /dev/null 2>&1 &
    done
    
    log "Phase 5: Waiting for message delivery (polling logs)..."
    local total_expected=$((TOPIC_MEMBERS * (TOPIC_MEMBERS - 1)))  # Each node should receive from all others
    local timeout=30
    local elapsed=0
    local total_received=0
    
    while [ $elapsed -lt $timeout ]; do
        total_received=0
        for receiver in $(seq 1 $TOPIC_MEMBERS); do
            for sender in $(seq 1 $TOPIC_MEMBERS); do
                if [ $sender -ne $receiver ]; then
                    if grep -q "Concurrent-from-Node-$sender" "$NODES_DIR/node-$receiver/output.log" 2>/dev/null; then
                        total_received=$((total_received + 1))
                    fi
                fi
            done
        done
        
        if [ $total_received -ge $total_expected ]; then
            break
        fi
        
        sleep 1
        elapsed=$((elapsed + 1))
    done
    
    # Kill any lingering curl processes
    pkill -f "curl.*api/send" 2>/dev/null || true
    
    log "Phase 6: Verifying results..."
    for receiver in $(seq 1 $TOPIC_MEMBERS); do
        local received=0
        for sender in $(seq 1 $TOPIC_MEMBERS); do
            if [ $sender -ne $receiver ]; then
                if grep -q "Concurrent-from-Node-$sender" "$NODES_DIR/node-$receiver/output.log" 2>/dev/null; then
                    received=$((received + 1))
                fi
            fi
        done
        local expected=$((TOPIC_MEMBERS - 1))
        if [ $received -lt $expected ]; then
            warn "  Node-$receiver received $received/$expected messages"
        fi
    done
    
    local success_rate=$((total_received * 100 / total_expected))
    log "  Total: $total_received/$total_expected messages ($success_rate%) in ${elapsed}s"
    
    if [ $success_rate -ge 90 ]; then
        log "✅ Concurrent sending successful!"
    else
        warn "⚠️  Some messages lost during concurrent sending"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Concurrent Senders"
    log "=========================================="
}

