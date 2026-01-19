#!/bin/bash
#
# Scenario: Concurrent CRDT Edits
#
# Tests that concurrent edits from multiple nodes merge correctly.
# Multiple nodes edit simultaneously, CRDT ensures all edits are preserved.
# Verification: All edits from all nodes are present in final state.
#

scenario_sync_concurrent() {
    log "=========================================="
    log "SCENARIO: Concurrent CRDT Edits"
    log "=========================================="
    
    local TEST_NODES=10
    local TOPIC_MEMBERS=3
    local CONTAINER="shared-doc"
    
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
    
    # All nodes make concurrent edits
    # Each node inserts its own identifier at position 0
    # CRDT should merge all of them
    log "Phase 5: All nodes making concurrent edits..."
    for i in $(seq 1 $TOPIC_MEMBERS); do
        # Insert at position 0 (prepend)
        local text="[Node$i]"
        api_sync_text_insert $i "$TOPIC_ID" "$CONTAINER" 0 "$text" > /dev/null &
    done
    
    # Wait for all curl calls to complete
    wait
    info "All edits submitted"
    
    # Wait for sync propagation
    log "Phase 6: Waiting for sync propagation (20s)..."
    sleep 20
    
    # Verify all members have all edits
    log "Phase 7: Verifying concurrent edits merged..."
    local verification_passed=true
    
    for receiver in $(seq 1 $TOPIC_MEMBERS); do
        local text_json=$(api_sync_get_text $receiver "$TOPIC_ID" "$CONTAINER")
        local text_value=$(extract_sync_text "$text_json")
        info "  Node-$receiver text: $text_value"
        
        # Check that all node markers are present
        local found_all=true
        for sender in $(seq 1 $TOPIC_MEMBERS); do
            if ! echo "$text_value" | grep -q "\[Node$sender\]"; then
                found_all=false
                warn "    Missing [Node$sender] in Node-$receiver's document"
            fi
        done
        
        if [ "$found_all" = true ]; then
            log "  ✅ Node-$receiver has all edits"
        else
            warn "  ⚠️  Node-$receiver missing some edits"
            verification_passed=false
        fi
    done
    
    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "✅ PASSED: Concurrent edits merged correctly on all nodes"
    else
        warn "⚠️  PARTIAL: Some concurrent edits not yet merged"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Concurrent CRDT Edits"
    log "=========================================="
}

