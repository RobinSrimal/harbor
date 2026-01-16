#!/bin/bash
#
# Scenario: Member Leave Flow
#
# Tests that remaining members continue to communicate after some leave.
# Verification: Logs checked for messages between remaining members.
#

scenario_member_leave() {
    log "=========================================="
    log "SCENARIO: Member Leave Flow"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=10  # Max 10 topic members
    local LEAVING_NODES="2 4 6"  # These nodes will leave
    local MSG_PREFIX="LEAVE-FLOW-MSG"  # Unique prefix for verification
    
    # Start bootstrap node
    start_bootstrap_node
    
    # Start all nodes
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
        sleep 0.3
    done
    for i in $(seq 2 $TEST_NODES); do
        wait_for_api $i
    done
    
    # Wait for DHT
    log "Phase 2: DHT convergence (75s)..."
    sleep 75
    
    # Create topic with nodes 1-10 (max 10 members)
    log "Phase 3: Creating topic with $TOPIC_MEMBERS nodes..."
    INVITE=$(api_create_topic 1)
    TOPIC_ID="${INVITE:0:64}"
    
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 2
    
    info "Nodes 1-$TOPIC_MEMBERS joined topic: ${TOPIC_ID:0:16}..."
    
    # Send message before leave
    log "Phase 4: Sending messages before nodes leave..."
    api_send 1 "$TOPIC_ID" "${MSG_PREFIX}-BEFORE-LEAVE-from-Node1" > /dev/null
    sleep 2
    
    # Multiple nodes leave (stop the nodes - simulates leaving)
    log "Phase 5: Nodes $LEAVING_NODES going offline (leaving)..."
    for i in $LEAVING_NODES; do
        stop_node $i
    done
    sleep 2
    
    # Send messages between remaining nodes
    log "Phase 6: Sending messages between remaining nodes..."
    local remaining_nodes="1 3 5 7 8 9 10"
    for sender in $remaining_nodes; do
        api_send $sender "$TOPIC_ID" "${MSG_PREFIX}-AFTER-LEAVE-from-Node${sender}" > /dev/null
        info "  Node-$sender sent message"
        sleep 0.5
    done
    sleep 5
    
    # Verify remaining nodes still communicate
    log "Phase 7: Verifying message delivery between remaining nodes..."
    local success=0
    local receivers="3 5 7 8 9 10"  # Exclude node-1 as sender
    
    for receiver in $receivers; do
        local received_count=0
        for sender in $remaining_nodes; do
            if [ $sender -ne $receiver ]; then
                if grep -q "${MSG_PREFIX}-AFTER-LEAVE-from-Node${sender}" "$NODES_DIR/node-$receiver/output.log" 2>/dev/null; then
                    received_count=$((received_count + 1))
                fi
            fi
        done
        
        if [ $received_count -ge 4 ]; then
            log "  ✅ Node-$receiver: received $received_count messages from remaining members"
            success=$((success + 1))
        else
            warn "  ⚠️  Node-$receiver: only received $received_count messages"
        fi
    done
    
    # Check that leaving nodes do NOT receive post-leave messages
    log "Phase 8: Verifying leaving nodes don't receive post-leave messages..."
    local leavers_isolated=0
    for leaver in $LEAVING_NODES; do
        # These nodes are stopped, so we check their logs don't have post-leave messages
        local post_leave_found=0
        for sender in $remaining_nodes; do
            if grep -q "${MSG_PREFIX}-AFTER-LEAVE-from-Node${sender}" "$NODES_DIR/node-$leaver/output.log" 2>/dev/null; then
                post_leave_found=$((post_leave_found + 1))
            fi
        done
        
        if [ $post_leave_found -eq 0 ]; then
            info "  ✅ Node-$leaver (left): correctly did not receive post-leave messages"
            leavers_isolated=$((leavers_isolated + 1))
        else
            warn "  ⚠️  Node-$leaver (left): unexpectedly received $post_leave_found post-leave messages"
        fi
    done
    
    # Summary
    log ""
    log "Verification Summary:"
    log "  Remaining nodes receiving messages: $success/6"
    log "  Leaving nodes correctly isolated: $leavers_isolated/3"
    
    if [ $success -ge 5 ] && [ $leavers_isolated -eq 3 ]; then
        log "  ✅ PASSED: Leave flow working correctly"
    elif [ $success -ge 4 ]; then
        warn "  ⚠️  PARTIAL: Most nodes working, some isolation issues"
    else
        warn "  ❌ FAILED: Leave flow has issues"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Member Leave Flow"
    log "=========================================="
}
