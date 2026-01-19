#!/bin/bash
#
# Scenario: Basic CRDT Sync
#
# Tests basic collaborative text editing between topic members.
# One node makes edits, verifies other nodes receive them via sync.
# Verification: Check sync text values match across nodes.
#

scenario_sync_basic() {
    log "=========================================="
    log "SCENARIO: Basic CRDT Sync"
    log "=========================================="
    
    local TEST_NODES=10
    local TOPIC_MEMBERS=3  # Keep small for sync testing
    local CONTAINER="doc1"
    local TEST_TEXT="SYNC-BASIC-TEST"
    
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
    log "Phase 3: Creating topic on node-1..."
    INVITE=$(api_create_topic 1)
    if [ -z "$INVITE" ]; then
        fail "Failed to create topic"
    fi
    TOPIC_ID="${INVITE:0:64}"
    info "Topic ID: ${TOPIC_ID:0:16}..."
    
    log "Phase 3b: Nodes 2-$TOPIC_MEMBERS joining topic..."
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        local result=$(api_join_topic $i "$current_invite")
        info "  Node-$i joined: $result"
        sleep 2  # Wait for JOIN message to propagate to all members
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    
    # Wait for member list to sync across all nodes
    log "Phase 3c: Waiting for member list propagation (10s)..."
    sleep 10
    
    # Enable sync on all members
    log "Phase 4: Enabling sync on all members..."
    for i in $(seq 1 $TOPIC_MEMBERS); do
        local result=$(api_sync_enable $i "$TOPIC_ID")
        info "  Node-$i sync enable: $result"
    done
    sleep 2
    
    # Node 1 makes an edit
    log "Phase 5: Node-1 inserting text..."
    local insert_result=$(api_sync_text_insert 1 "$TOPIC_ID" "$CONTAINER" 0 "$TEST_TEXT")
    info "Insert result: $insert_result"
    
    # Verify local state immediately
    local local_text=$(api_sync_get_text 1 "$TOPIC_ID" "$CONTAINER")
    local local_value=$(extract_sync_text "$local_text")
    info "Node-1 local text: $local_value"
    
    # Wait for sync propagation (batched sending ~1 second + network)
    log "Phase 6: Waiting for sync propagation (15s)..."
    sleep 15
    
    # Verify all members have the same text
    log "Phase 7: Verifying sync state across members..."
    local sync_success=0
    for i in $(seq 1 $TOPIC_MEMBERS); do
        local text_json=$(api_sync_get_text $i "$TOPIC_ID" "$CONTAINER")
        local text_value=$(extract_sync_text "$text_json")
        
        if [ "$text_value" = "$TEST_TEXT" ]; then
            log "  ✅ Node-$i: text matches ($text_value)"
            sync_success=$((sync_success + 1))
        else
            warn "  ⚠️  Node-$i: text mismatch (got: '$text_value', expected: '$TEST_TEXT')"
        fi
    done
    
    # Check sync status
    log "Phase 8: Sync status..."
    for i in $(seq 1 $TOPIC_MEMBERS); do
        local status=$(api_sync_status $i "$TOPIC_ID")
        info "  Node-$i: $status"
    done
    
    # Summary
    log ""
    log "Verification Summary:"
    log "  Nodes with correct text: $sync_success/$TOPIC_MEMBERS"
    
    if [ $sync_success -eq $TOPIC_MEMBERS ]; then
        log "✅ PASSED: Basic CRDT sync working"
    elif [ $sync_success -gt 0 ]; then
        warn "⚠️  PARTIAL: Some nodes synced ($sync_success/$TOPIC_MEMBERS)"
    else
        warn "❌ FAILED: No nodes received sync update"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Basic CRDT Sync"
    log "=========================================="
}

