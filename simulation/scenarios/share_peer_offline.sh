#!/bin/bash
#
# Scenario: File Sharing with Peer Offline
#
# Tests that a peer can receive a file after coming back online.
# 1. Create topic with 5 members
# 2. Node-5 goes offline
# 3. Node-1 shares a file
# 4. Node-5 comes back online
# 5. Node-5 should receive the file
#
# Verification: Logs checked for "SHARE: Received file announcement" on node-5
#

scenario_share_peer_offline() {
    log "=========================================="
    log "SCENARIO: File Sharing with Peer Offline"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=5
    local OFFLINE_NODE=5
    local TEST_FILE_SIZE_MB=1
    local TEST_FILE="$NODES_DIR/test_share_offline.bin"
    
    # Create test file
    log "Phase 0: Creating test file..."
    create_test_file "$TEST_FILE" $TEST_FILE_SIZE_MB
    
    # Start all nodes
    start_bootstrap_node
    
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
        sleep 0.3
    done
    
    for i in $(seq 2 $TEST_NODES); do
        wait_for_api $i
    done
    
    # DHT convergence
    log "Phase 2: DHT convergence (75s)..."
    sleep 75
    
    # Create topic
    log "Phase 3: Creating topic and joining..."
    INVITE=$(api_create_topic 1)
    TOPIC_ID="${INVITE:0:64}"
    
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 3
    
    # Node goes offline BEFORE file is shared
    log "Phase 4: Node-$OFFLINE_NODE going OFFLINE..."
    stop_node $OFFLINE_NODE
    sleep 2
    
    # Node 1 shares file (node 5 is offline)
    log "Phase 5: Node-1 sharing file (node-$OFFLINE_NODE offline)..."
    local share_result=$(api_share_file 1 "$TOPIC_ID" "$TEST_FILE")
    info "Share result: $share_result"
    
    local BLOB_HASH=$(echo "$share_result" | grep -o '"hash":"[a-f0-9]*"' | cut -d'"' -f4)
    info "Blob hash: ${BLOB_HASH:0:16}..."
    
    # Wait for online nodes to receive
    log "Phase 6: Waiting for online nodes to receive (30s)..."
    sleep 30
    
    # Verify online nodes received announcement
    log "Phase 7: Verifying online nodes received announcement..."
    local online_received=0
    for i in 2 3 4; do
        if grep -q "SHARE: Received file announcement" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            log "  ✅ Node-$i: received announcement (while node-$OFFLINE_NODE offline)"
            online_received=$((online_received + 1))
        else
            warn "  ⚠️  Node-$i: did NOT receive announcement"
        fi
    done
    
    # Offline node comes back
    log "Phase 8: Node-$OFFLINE_NODE coming back ONLINE..."
    start_node $OFFLINE_NODE
    sleep 2
    wait_for_api $OFFLINE_NODE
    
    # Rejoin topic (triggers sync)
    log "Phase 9: Node-$OFFLINE_NODE rejoining topic..."
    api_join_topic $OFFLINE_NODE "$INVITE" > /dev/null
    
    # Wait for sync
    log "Phase 10: Waiting for sync (75s)..."
    sleep 75
    
    # Verify offline node received
    log "Phase 11: Verifying node-$OFFLINE_NODE received file..."
    local offline_received=false
    
    if grep -q "SHARE: Received file announcement" "$NODES_DIR/node-$OFFLINE_NODE/output.log" 2>/dev/null; then
        log "  ✅ Node-$OFFLINE_NODE: received file announcement after coming online"
        offline_received=true
    else
        # Check via API
        local status=$(api_share_status $OFFLINE_NODE "$BLOB_HASH")
        if echo "$status" | grep -q '"hash"'; then
            log "  ✅ Node-$OFFLINE_NODE: knows about file (via API)"
            offline_received=true
        else
            warn "  ⚠️  Node-$OFFLINE_NODE: did NOT receive file announcement"
        fi
    fi
    
    # Check for file completion on previously offline node
    if grep -qi "blob complete" "$NODES_DIR/node-$OFFLINE_NODE/output.log" 2>/dev/null; then
        log "  ✅ Node-$OFFLINE_NODE: file transfer complete"
    else
        local status=$(api_share_status $OFFLINE_NODE "$BLOB_HASH")
        local state=$(echo "$status" | grep -o '"state":"[^"]*"' | cut -d'"' -f4)
        info "  ⏳ Node-$OFFLINE_NODE: file state = $state"
    fi
    
    # Cleanup
    rm -f "$TEST_FILE"
    
    # Summary
    log ""
    log "Verification Summary:"
    log "  Online nodes received: $online_received/3"
    log "  Offline node received after rejoining: $offline_received"
    
    if [ $online_received -ge 2 ] && [ "$offline_received" = true ]; then
        log "✅ Offline peer file sync: PASSED"
    elif [ "$offline_received" = true ]; then
        warn "⚠️  Offline peer sync worked, but online delivery was partial"
    else
        warn "⚠️  Offline peer file sync: NEEDS IMPROVEMENT"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: File Sharing with Peer Offline"
    log "=========================================="
}

