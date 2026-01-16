#!/bin/bash
#
# Scenario: Offline Sync with Membership Changes
#
# Tests that offline nodes can sync properly even when
# membership changes occur while they are offline.
#

scenario_offline_sync() {
    log "=========================================="
    log "SCENARIO: Offline Sync with Membership Changes"
    log "=========================================="
    
    local TEST_NODES=20
    local INITIAL_NODES=5        # 1-5 start initially and join topic
    local OFFLINE_NODES="4 5"    # Go offline before new members join
    local NEW_NODES="6 7 8 9 10" # Join while others offline (max 10 total members)
    local LEAVING_NODES="2 3"    # Leave after new members join
    
    # Start bootstrap and all nodes
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
    
    # Create topic with initial nodes
    log "Phase 2: Creating topic with initial nodes 1-$INITIAL_NODES..."
    INVITE=$(api_create_topic 1)
    TOPIC_ID="${INVITE:0:64}"
    local current_invite="$INVITE"
    for i in $(seq 2 $INITIAL_NODES); do
        api_join_topic $i "$current_invite" > /dev/null
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 2
    
    # Some nodes go offline
    log "Phase 3: Nodes $OFFLINE_NODES going OFFLINE..."
    for i in $OFFLINE_NODES; do
        stop_node $i
    done
    sleep 2
    
    # New nodes join while others offline (nodes 6-10 join topic, max 10 members total)
    log "Phase 4: New nodes $NEW_NODES joining while nodes $OFFLINE_NODES offline..."
    FRESH_INVITE=$(api_get_invite 1 "$TOPIC_ID")
    for i in $NEW_NODES; do
        api_join_topic $i "$FRESH_INVITE" > /dev/null
        sleep 0.5
        FRESH_INVITE=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 2
    
    # Messages exchanged between online nodes
    log "Phase 5: Sending messages (nodes $OFFLINE_NODES offline)..."
    for sender in 1 6 7 8 9 10; do
        api_send $sender "$TOPIC_ID" "Message from Node-$sender (some nodes offline)" > /dev/null
        sleep 0.5
    done
    sleep 2
    
    # Some nodes leave
    log "Phase 6: Nodes $LEAVING_NODES leaving..."
    for i in $LEAVING_NODES; do
        stop_node $i
    done
    sleep 2
    
    # Offline nodes come back online
    log "Phase 7: Nodes $OFFLINE_NODES coming back ONLINE..."
    for i in $OFFLINE_NODES; do
        start_node $i
        sleep 1
    done
    for i in $OFFLINE_NODES; do
        wait_for_api $i
    done
    
    # Rejoin to trigger sync
    for i in $OFFLINE_NODES; do
        api_join_topic $i "$INVITE" > /dev/null
    done
    
    # Wait for sync
    log "Phase 8: Waiting for Harbor sync (75s)..."
    sleep 75
    
    # Verify offline nodes received messages from new nodes
    log "Phase 9: Verifying sync..."
    local success=0
    for receiver in $OFFLINE_NODES; do
        local found=0
        for sender in $NEW_NODES; do
            if grep -q "Message from Node-$sender" "$NODES_DIR/node-$receiver/output.log"; then
                found=$((found + 1))
            fi
        done
        if [ $found -gt 0 ]; then
            log "✅ Node-$receiver received $found messages from new nodes!"
            success=$((success + 1))
        else
            warn "⚠️  Node-$receiver didn't receive messages from new nodes"
        fi
    done
    
    if [ $success -ge 1 ]; then
        log "✅ Offline nodes synced with membership changes!"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Offline Sync with Membership Changes"
    log "=========================================="
}

