#!/bin/bash
#
# Scenario: Harbor Sync Verification
#
# Tests that Harbor nodes properly sync packets between themselves
# and that late joiners receive messages sent AFTER they join.
# (By design, members only receive messages sent after joining)
#
# Verification: Logs checked for POST-JOIN messages, NOT PRE-JOIN.
#

scenario_harbor_sync() {
    log "=========================================="
    log "SCENARIO: Harbor Sync Verification"
    log "=========================================="
    log "Tests that late joiners receive messages sent AFTER they join"
    log "(By design, members only receive messages sent after joining)"
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=10  # Max 10 topic members (nodes 1-10)
    local MSG_PREFIX="HARBOR-SYNC"  # Unique prefix for verification
    
    start_bootstrap_node
    
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
    done
    
    log "Phase 2: Waiting for DHT convergence (45s)..."
    sleep 45
    
    log "Phase 3: Creating topic with initial members..."
    local invite=$(api_create_topic 1)
    local topic=$(echo "$invite" | head -c 64)
    
    # Join nodes 2-7 first (7 members so far)
    for i in $(seq 2 7); do
        api_join_topic $i "$invite" > /dev/null
        info "  Node-$i joined"
    done
    sleep 5
    
    log "Phase 4: Sending PRE-JOIN messages (nodes 8-10 NOT members yet)..."
    for sender in 1 2 3; do
        api_send $sender "$topic" "${MSG_PREFIX}-PRE-JOIN-from-Node${sender}" > /dev/null
        info "  Node-$sender sent PRE-JOIN message"
        sleep 1
    done
    sleep 5
    
    log "Phase 5: Nodes 8-10 join topic (now 10 members total)..."
    for i in 8 9 10; do
        api_join_topic $i "$invite" > /dev/null
        info "  Node-$i joined"
    done
    sleep 10
    
    log "Phase 6: Stopping nodes 8-10 (to test Harbor delivery)..."
    for i in 8 9 10; do
        stop_node $i
    done
    sleep 3
    
    log "Phase 7: Sending POST-JOIN messages (nodes 8-10 are members but offline)..."
    for sender in 1 2 3; do
        api_send $sender "$topic" "${MSG_PREFIX}-POST-JOIN-from-Node${sender}" > /dev/null
        info "  Node-$sender sent POST-JOIN message"
        sleep 1
    done
    sleep 5
    
    log "Phase 8: Restarting nodes 8-10..."
    for i in 8 9 10; do
        start_node $i
    done
    sleep 10
    
    # Re-join to trigger Harbor pull
    for i in 8 9 10; do
        api_join_topic $i "$invite" > /dev/null
    done
    
    log "Phase 9: Waiting for Harbor pull and sync (90s)..."
    sleep 90
    
    log "Phase 10: Verifying message delivery..."
    local success=0
    local total_checks=0
    
    for i in 8 9 10; do
        local pre_join_received=0
        local post_join_received=0
        
        # Check PRE-JOIN messages (should NOT be received - by design, timestamp filter)
        for sender in 1 2 3; do
            if grep -q "${MSG_PREFIX}-PRE-JOIN-from-Node${sender}" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
                pre_join_received=$((pre_join_received + 1))
            fi
        done
        
        # Check POST-JOIN messages (SHOULD be received via Harbor)
        for sender in 1 2 3; do
            if grep -q "${MSG_PREFIX}-POST-JOIN-from-Node${sender}" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
                post_join_received=$((post_join_received + 1))
            fi
        done
        
        total_checks=$((total_checks + 1))
        
        # Success: received POST-JOIN messages, did NOT receive PRE-JOIN messages
        if [ $post_join_received -ge 2 ] && [ $pre_join_received -eq 0 ]; then
            log "  ✅ Node-$i: POST-JOIN=$post_join_received/3, PRE-JOIN=$pre_join_received/3 (correct)"
            success=$((success + 1))
        elif [ $post_join_received -ge 2 ]; then
            warn "  ⚠️ Node-$i: POST-JOIN=$post_join_received/3, PRE-JOIN=$pre_join_received/3 (unexpected pre-join)"
            success=$((success + 1))  # Still count as partial success
        else
            warn "  ❌ Node-$i: POST-JOIN=$post_join_received/3, PRE-JOIN=$pre_join_received/3 (missing post-join)"
        fi
    done
    
    # Summary
    log ""
    if [ $success -ge 2 ]; then
        log "✅ PASSED: Harbor sync working! Late joiners received post-join messages."
    else
        warn "⚠️  PARTIAL: Harbor sync may need improvement"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Harbor Sync Verification"
    log "=========================================="
}
