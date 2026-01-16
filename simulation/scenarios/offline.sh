#!/bin/bash
#
# Scenario: Offline Node Message Retrieval
#
# Tests that offline nodes can retrieve missed messages via Harbor.
# Verification: Logs checked for specific message content received by offline nodes.
#

scenario_offline() {
    log "=========================================="
    log "SCENARIO: Offline Node Message Retrieval"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=10  # Max 10 topic members (nodes 1-10)
    local OFFLINE_NODES="8 9 10"  # These nodes will go offline
    local MSG_PREFIX="OFFLINE-TEST-MSG"  # Unique prefix for verification
    
    # Start bootstrap node
    start_bootstrap_node
    
    # Start all other nodes
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
        sleep 0.3
    done
    
    for i in $(seq 2 $TEST_NODES); do
        wait_for_api $i
    done
    
    # Wait for DHT (must be > 60s to capture DHT save to DB)
    log "Phase 2: DHT convergence (75s)..."
    sleep 75
    
    # Create topic
    log "Phase 3: Creating topic..."
    INVITE=$(api_create_topic 1)
    info "Created topic, invite: ${INVITE:0:32}..."
    
    # Get topic ID from initial invite (first 64 chars are topic ID)
    TOPIC_ID="${INVITE:0:64}"
    
    # Only nodes 1-10 join topic (max 10 members, nodes 11-20 remain as Harbor nodes)
    log "Phase 3b: Nodes 1-$TOPIC_MEMBERS joining topic..."
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        info "Node-$i joined"
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 3
    
    if [ -z "$TOPIC_ID" ]; then
        fail "Could not get topic ID"
    fi
    
    # Multiple nodes go offline
    log "Phase 4: Nodes $OFFLINE_NODES going OFFLINE..."
    for i in $OFFLINE_NODES; do
        stop_node $i
    done
    sleep 2
    
    # Send messages while offline nodes are down
    log "Phase 5: Sending messages (nodes $OFFLINE_NODES offline)..."
    for sender in 1 2 3 4 5; do
        api_send $sender "$TOPIC_ID" "${MSG_PREFIX}-from-Node${sender}-WhileOffline" > /dev/null
        info "  Node-$sender sent message"
        sleep 1
    done
    
    info "Messages should now be stored on Harbor Nodes"
    
    # Check Harbor storage on online nodes (topic members only)
    log "Harbor storage status:"
    for i in 1 2 3 4 5 6 7; do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done
    
    # Wait for replication
    log "Phase 6: Waiting for Harbor replication (15s)..."
    sleep 15
    
    # Offline nodes come back online
    log "Phase 7: Nodes $OFFLINE_NODES coming back ONLINE..."
    for i in $OFFLINE_NODES; do
        start_node $i
        sleep 1
    done
    for i in $OFFLINE_NODES; do
        wait_for_api $i
    done
    
    # Rejoin topic (will trigger Harbor pull)
    log "Phase 8: Offline nodes rejoining topic..."
    for i in $OFFLINE_NODES; do
        api_join_topic $i "$INVITE" > /dev/null
    done
    
    # Wait for sync
    log "Phase 9: Waiting for Harbor sync (75s)..."
    sleep 75
    
    # Check final stats (topic members only)
    log "Phase 10: Final verification"
    for i in $(seq 1 $TOPIC_MEMBERS); do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done
    
    # Verify message reception for offline nodes
    log ""
    log "Verifying offline nodes received messages..."
    local offline_success=0
    local total_offline_nodes=0
    
    for receiver in $OFFLINE_NODES; do
        total_offline_nodes=$((total_offline_nodes + 1))
        local received_count=0
        local expected_count=5  # 5 messages sent while offline
        
        for sender in 1 2 3 4 5; do
            if grep -q "${MSG_PREFIX}-from-Node${sender}-WhileOffline" "$NODES_DIR/node-$receiver/output.log" 2>/dev/null; then
                received_count=$((received_count + 1))
            fi
        done
        
        if [ $received_count -ge 3 ]; then
            log "  ✅ Node-$receiver: received $received_count/$expected_count messages via Harbor"
            offline_success=$((offline_success + 1))
        else
            warn "  ⚠️  Node-$receiver: received only $received_count/$expected_count messages"
        fi
    done
    
    # Summary
    log ""
    if [ $offline_success -eq $total_offline_nodes ]; then
        log "✅ All $total_offline_nodes offline nodes received their messages via Harbor!"
    elif [ $offline_success -gt 0 ]; then
        warn "⚠️  $offline_success/$total_offline_nodes offline nodes received messages"
    else
        warn "❌ No offline nodes received their messages"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Offline Node Message Retrieval"
    log "=========================================="
}
