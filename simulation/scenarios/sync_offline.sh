#!/bin/bash
#
# Scenario: Sync with Offline Node
#
# Tests that offline nodes receive sync updates when they come back online.
# Node goes offline, others make edits, node comes back and syncs.
# Verification: Returning node has all edits made while it was offline.
#

scenario_sync_offline() {
    log "=========================================="
    log "SCENARIO: Sync with Offline Node"
    log "=========================================="
    
    local TEST_NODES=10
    local TOPIC_MEMBERS=3
    local OFFLINE_NODE=3
    local CONTAINER="offline-test"
    
    # Start bootstrap node
    start_bootstrap_node
    
    # Start other nodes
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
        sleep 0.3
    done
    
    for i in $(seq 2 $TEST_NODES); do
        wait_for_api $i
    done
    
    # Wait for DHT convergence
    log "Phase 2: DHT convergence (75s)..."
    sleep 75
    
    # Create topic and join members
    log "Phase 3: Creating topic..."
    INVITE=$(api_create_topic 1)
    TOPIC_ID="${INVITE:0:64}"
    info "Topic ID: ${TOPIC_ID:0:16}..."
    
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 3
    
    # Enable sync on all members
    log "Phase 4: Enabling sync on all members..."
    for i in $(seq 1 $TOPIC_MEMBERS); do
        api_sync_enable $i "$TOPIC_ID" > /dev/null
    done
    sleep 2
    
    # Initial text to ensure all nodes are synced
    log "Phase 5: Node-1 inserting initial text..."
    api_sync_text_insert 1 "$TOPIC_ID" "$CONTAINER" 0 "INITIAL-" > /dev/null
    sleep 10
    
    # Verify initial sync
    local initial_text=$(api_sync_get_text $OFFLINE_NODE "$TOPIC_ID" "$CONTAINER")
    info "Node-$OFFLINE_NODE initial text: $(extract_sync_text "$initial_text")"
    
    # Node goes offline
    log "Phase 6: Node-$OFFLINE_NODE going OFFLINE..."
    stop_node $OFFLINE_NODE
    sleep 2
    
    # Online nodes make edits
    log "Phase 7: Online nodes making edits while Node-$OFFLINE_NODE offline..."
    api_sync_text_insert 1 "$TOPIC_ID" "$CONTAINER" 8 "EDIT1-" > /dev/null
    sleep 2
    api_sync_text_insert 2 "$TOPIC_ID" "$CONTAINER" 14 "EDIT2-" > /dev/null
    sleep 2
    
    # Verify online nodes have edits
    local online_text=$(api_sync_get_text 1 "$TOPIC_ID" "$CONTAINER")
    info "Online nodes text: $(extract_sync_text "$online_text")"
    
    # Wait for Harbor node storage
    log "Phase 8: Waiting for Harbor node storage (30s)..."
    sleep 30
    
    # Node comes back online
    log "Phase 9: Node-$OFFLINE_NODE coming back ONLINE..."
    start_node $OFFLINE_NODE
    wait_for_api $OFFLINE_NODE
    
    # Re-enable sync (loads persisted state)
    api_sync_enable $OFFLINE_NODE "$TOPIC_ID" > /dev/null
    
    # Wait for sync
    log "Phase 10: Waiting for offline sync (45s)..."
    sleep 45
    
    # Verify returning node has all edits
    log "Phase 11: Verifying offline node received updates..."
    local final_text=$(api_sync_get_text $OFFLINE_NODE "$TOPIC_ID" "$CONTAINER")
    local final_value=$(extract_sync_text "$final_text")
    info "Node-$OFFLINE_NODE final text: $final_value"
    
    # Check for edits made while offline
    local has_edit1=false
    local has_edit2=false
    
    if echo "$final_value" | grep -q "EDIT1"; then
        has_edit1=true
        log "  ✅ EDIT1 found"
    else
        warn "  ⚠️  EDIT1 missing"
    fi
    
    if echo "$final_value" | grep -q "EDIT2"; then
        has_edit2=true
        log "  ✅ EDIT2 found"
    else
        warn "  ⚠️  EDIT2 missing"
    fi
    
    # Summary
    log ""
    log "Verification Summary:"
    if [ "$has_edit1" = true ] && [ "$has_edit2" = true ]; then
        log "✅ PASSED: Offline node received all updates"
    elif [ "$has_edit1" = true ] || [ "$has_edit2" = true ]; then
        warn "⚠️  PARTIAL: Offline node received some updates"
    else
        warn "❌ FAILED: Offline node did not receive updates"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Sync with Offline Node"
    log "=========================================="
}

