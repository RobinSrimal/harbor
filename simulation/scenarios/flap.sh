#!/bin/bash
#
# Scenario: Flapping Node
#
# Tests handling of a node that rapidly joins and leaves.
#

scenario_flap() {
    log "=========================================="
    log "SCENARIO: Flapping Node"
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
    
    log "Phase 3: Creating topic..."
    local invite=$(api_create_topic 1)
    if [ -z "$invite" ]; then
        warn "Failed to create topic"
        log ""
        log "=========================================="
        log "❌ SCENARIO FAILED: Flapping Node"
        log "=========================================="
        log ""
        log "Could not create topic on node-1. Check node-1 logs."
        return 1
    fi
    local topic=$(echo "$invite" | head -c 64)
    info "Created topic: ${topic:0:16}..."
    
    for i in 2 3 4; do
        api_join_topic $i "$invite" > /dev/null 2>&1 || warn "Node-$i failed to join topic"
    done
    sleep 5
    
    log "Phase 4: Node-5 flaps (rapid join/leave cycles)..."
    for cycle in 1 2 3; do
        log "  Flap cycle $cycle/3..."
        # Join may fail if node not ready - that's expected during flap testing
        api_join_topic 5 "$invite" > /dev/null 2>&1 || true
        sleep 2
        stop_node 5
        sleep 2
        start_node 5
        # Wait for API to be ready after restart
        wait_for_api 5 || true
        sleep 1
    done
    
    # Final join - wait for API and retry if needed
    log "  Final join attempt..."
    wait_for_api 5
    sleep 2
    local join_success=false
    for attempt in 1 2 3; do
        if api_join_topic 5 "$invite" > /dev/null 2>&1; then
            join_success=true
            info "  Node-5 rejoined topic on attempt $attempt"
            break
        fi
        sleep 2
    done
    if ! $join_success; then
        warn "  Node-5 failed to rejoin topic after 3 attempts"
    fi
    sleep 5
    
    log "Phase 5: Sending messages after flapping..."
    api_send 1 "$topic" "Post-Flap-Message-1" > /dev/null 2>&1 || warn "Node-1 failed to send message"
    api_send 2 "$topic" "Post-Flap-Message-2" > /dev/null 2>&1 || warn "Node-2 failed to send message"
    sleep 10
    
    log "Phase 6: Verifying network stability..."
    log ""
    local stable=true
    local nodes_receiving=0
    local nodes_checked=0
    
    # Check stable nodes still work
    info "Checking stable nodes (2-4) received messages:"
    for i in 2 3 4; do
        nodes_checked=$((nodes_checked + 1))
        local msg_count=$(grep -c "Post-Flap-Message" "$NODES_DIR/node-$i/output.log" 2>/dev/null || echo "0")
        if [ "$msg_count" -gt 0 ]; then
            info "  ✅ Node-$i: received $msg_count post-flap messages"
            nodes_receiving=$((nodes_receiving + 1))
        else
            warn "  ❌ Node-$i: not receiving messages after flap"
            stable=false
        fi
    done
    
    # Check flapping node works
    log ""
    info "Checking flapping node (5) recovered:"
    local flap_msg_count=$(grep -c "Post-Flap-Message" "$NODES_DIR/node-5/output.log" 2>/dev/null || echo "0")
    if [ "$flap_msg_count" -gt 0 ]; then
        info "  ✅ Node-5 (flapper): received $flap_msg_count post-flap messages"
        nodes_receiving=$((nodes_receiving + 1))
    else
        warn "  ❌ Node-5 (flapper): not receiving messages"
        stable=false
    fi
    nodes_checked=$((nodes_checked + 1))
    
    # Summary
    log ""
    log "Results: $nodes_receiving/$nodes_checked nodes receiving messages"
    
    log ""
    log "=========================================="
    if $stable; then
        log "✅ SCENARIO PASSED: Flapping Node"
        log "=========================================="
        log ""
        log "All nodes remained stable after rapid join/leave cycles."
    else
        log "❌ SCENARIO FAILED: Flapping Node"
        log "=========================================="
        log ""
        log "Network instability detected after flapping."
        log ""
        log "Troubleshooting:"
        log "  1. Check node-5 logs: cat $NODES_DIR/node-5/output.log"
        log "  2. Check for errors: grep -i error $NODES_DIR/node-*/output.log"
        log "  3. Verify topic membership recovered after rejoins"
    fi
}

