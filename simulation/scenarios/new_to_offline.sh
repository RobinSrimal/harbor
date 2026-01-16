#!/bin/bash
#
# Scenario: New Member to Offline Member Delivery
#
# Tests that messages from new members reach offline original members via Harbor.
# New members have complete member lists from invites, so they can send
# directly to all members (including offline ones via Harbor replication).
#
# Verification: Logs checked for message delivery to previously-offline nodes.
#

scenario_new_to_offline() {
    log "=========================================="
    log "SCENARIO: New Member to Offline Member Delivery"
    log "=========================================="
    log "Tests Harbor delivery from new members to offline original members"
    
    local TEST_NODES=20
    local INITIAL_NODES=5       # Nodes 1-5 start initially and join topic
    local OFFLINE_NODES="3 4 5" # These go offline before new nodes join
    local NEW_NODES="6 7 8 9 10" # New nodes join while others offline (max 10 total members)
    local MSG_PREFIX="NEWMEMBER-MSG"  # Unique prefix for verification
    
    # Start bootstrap node and initial nodes
    start_bootstrap_node
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
        sleep 0.3
    done
    for i in $(seq 2 $TEST_NODES); do
        wait_for_api $i
    done
    
    # Wait for DHT
    log "Phase 1: DHT convergence (75s)..."
    sleep 75
    
    # Initial nodes join topic
    log "Phase 2: Initial nodes (1-$INITIAL_NODES) join topic..."
    INVITE=$(api_create_topic 1)
    TOPIC_ID="${INVITE:0:64}"
    local current_invite="$INVITE"
    for i in $(seq 2 $INITIAL_NODES); do
        api_join_topic $i "$current_invite" > /dev/null
        info "  Node-$i joined"
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 2
    
    # Some initial nodes go offline
    log "Phase 3: Nodes $OFFLINE_NODES going OFFLINE..."
    for i in $OFFLINE_NODES; do
        stop_node $i
    done
    sleep 2
    
    # New nodes join with fresh invite (contains all members including offline ones)
    log "Phase 4: New nodes $NEW_NODES joining with fresh invite..."
    log "         (Invite contains all members, including offline nodes $OFFLINE_NODES)"
    
    # Get fresh invite that includes all current members
    FRESH_INVITE=$(api_get_invite 1 "$TOPIC_ID")
    for i in $NEW_NODES; do
        api_join_topic $i "$FRESH_INVITE" > /dev/null
        info "  Node-$i joined (knows about offline nodes from invite)"
        sleep 0.5
        FRESH_INVITE=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 2
    
    # New nodes send messages (to all members including offline ones)
    log "Phase 5: New nodes sending messages..."
    log "         These should be replicated to Harbor for offline nodes"
    for i in $NEW_NODES; do
        api_send $i "$TOPIC_ID" "${MSG_PREFIX}-from-NewNode${i}" > /dev/null
        info "  Node-$i sent message"
        sleep 1
    done
    
    # Wait for Harbor replication
    log "Phase 6: Waiting for Harbor replication (30s)..."
    sleep 30
    
    # Offline nodes come back online
    log "Phase 7: Nodes $OFFLINE_NODES coming back ONLINE..."
    for i in $OFFLINE_NODES; do
        start_node $i
        sleep 1
    done
    for i in $OFFLINE_NODES; do
        wait_for_api $i
    done
    
    # Rejoin topic to trigger Harbor pull
    log "Phase 8: Offline nodes rejoining to trigger Harbor pull..."
    for i in $OFFLINE_NODES; do
        api_join_topic $i "$INVITE" > /dev/null
    done
    
    # Wait for Harbor sync
    log "Phase 9: Waiting for Harbor sync (60s)..."
    sleep 60
    
    # Check if offline nodes received new nodes' messages
    log "Phase 10: Verifying message delivery to previously-offline nodes..."
    local success=0
    local total_offline=0
    
    for receiver in $OFFLINE_NODES; do
        total_offline=$((total_offline + 1))
        local found=0
        for sender in $NEW_NODES; do
            if grep -q "${MSG_PREFIX}-from-NewNode${sender}" "$NODES_DIR/node-$receiver/output.log" 2>/dev/null; then
                found=$((found + 1))
            fi
        done
        
        if [ $found -ge 3 ]; then
            log "  ✅ Node-$receiver: received $found/5 messages from new nodes via Harbor"
            success=$((success + 1))
        elif [ $found -gt 0 ]; then
            warn "  ⚠️  Node-$receiver: received only $found/5 messages from new nodes"
        else
            warn "  ❌ Node-$receiver: didn't receive any messages from new nodes"
        fi
    done
    
    # Check if online initial nodes also received messages
    log "Phase 11: Verifying delivery to online original members..."
    for receiver in 1 2; do
        local found=0
        for sender in $NEW_NODES; do
            if grep -q "${MSG_PREFIX}-from-NewNode${sender}" "$NODES_DIR/node-$receiver/output.log" 2>/dev/null; then
                found=$((found + 1))
            fi
        done
        info "  Node-$receiver: received $found/5 messages from new nodes"
    done
    
    # Summary
    log ""
    log "Verification Summary:"
    log "  Previously-offline nodes receiving messages: $success/$total_offline"
    
    if [ $success -ge 2 ]; then
        log "  ✅ PASSED: New members can reach offline original members via Harbor"
    else
        warn "  ⚠️  PARTIAL: Harbor delivery may need improvement"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: New Member to Offline Member Delivery"
    log "=========================================="
}
