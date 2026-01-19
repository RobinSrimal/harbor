#!/bin/bash
#
# Scenario: Initial Sync for New Member
#
# Tests that a new member joining an existing topic with sync data
# receives the full document state via initial sync.
# Verification: New member has complete document after joining.
#

scenario_sync_initial() {
    log "=========================================="
    log "SCENARIO: Initial Sync for New Member"
    log "=========================================="
    
    local TEST_NODES=10
    local INITIAL_MEMBERS=2
    local NEW_MEMBER=3
    local CONTAINER="initial-sync-test"
    
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
    
    # Create topic with initial members only
    log "Phase 3: Creating topic with initial members (1-$INITIAL_MEMBERS)..."
    INVITE=$(api_create_topic 1)
    TOPIC_ID="${INVITE:0:64}"
    info "Topic ID: ${TOPIC_ID:0:16}..."
    
    local current_invite="$INVITE"
    for i in $(seq 2 $INITIAL_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 3
    
    # Enable sync on initial members
    log "Phase 4: Enabling sync on initial members..."
    for i in $(seq 1 $INITIAL_MEMBERS); do
        api_sync_enable $i "$TOPIC_ID" > /dev/null
    done
    sleep 2
    
    # Build up document history
    log "Phase 5: Building document history..."
    api_sync_text_insert 1 "$TOPIC_ID" "$CONTAINER" 0 "LINE1-" > /dev/null
    sleep 3
    api_sync_text_insert 2 "$TOPIC_ID" "$CONTAINER" 6 "LINE2-" > /dev/null
    sleep 3
    api_sync_text_insert 1 "$TOPIC_ID" "$CONTAINER" 12 "LINE3-" > /dev/null
    sleep 3
    api_sync_text_insert 2 "$TOPIC_ID" "$CONTAINER" 18 "FINAL" > /dev/null
    sleep 5
    
    # Verify document state on existing members
    local existing_text=$(api_sync_get_text 1 "$TOPIC_ID" "$CONTAINER")
    local expected_text=$(extract_sync_text "$existing_text")
    info "Existing members document: $expected_text"
    
    # Wait for sync propagation between initial members
    log "Phase 6: Waiting for sync between initial members (15s)..."
    sleep 15
    
    # New member joins
    log "Phase 7: New member (Node-$NEW_MEMBER) joining topic..."
    local fresh_invite=$(api_get_invite 1 "$TOPIC_ID")
    api_join_topic $NEW_MEMBER "$fresh_invite" > /dev/null
    sleep 3
    
    # Enable sync on new member (triggers initial sync request)
    log "Phase 8: Enabling sync on new member (triggers initial sync)..."
    api_sync_enable $NEW_MEMBER "$TOPIC_ID" > /dev/null
    
    # Wait for initial sync response
    log "Phase 9: Waiting for initial sync (30s)..."
    sleep 30
    
    # Verify new member has full document
    log "Phase 10: Verifying new member received full document..."
    local new_member_text=$(api_sync_get_text $NEW_MEMBER "$TOPIC_ID" "$CONTAINER")
    local new_member_value=$(extract_sync_text "$new_member_text")
    info "New member document: $new_member_value"
    
    # Compare with existing member
    local verification_passed=true
    
    if [ "$new_member_value" = "$expected_text" ]; then
        log "  ✅ New member has complete document (exact match)"
    else
        # Check for key content
        local has_line1=false
        local has_line2=false
        local has_line3=false
        local has_final=false
        
        echo "$new_member_value" | grep -q "LINE1" && has_line1=true
        echo "$new_member_value" | grep -q "LINE2" && has_line2=true
        echo "$new_member_value" | grep -q "LINE3" && has_line3=true
        echo "$new_member_value" | grep -q "FINAL" && has_final=true
        
        if [ "$has_line1" = true ] && [ "$has_line2" = true ] && \
           [ "$has_line3" = true ] && [ "$has_final" = true ]; then
            log "  ✅ New member has all content (order may differ)"
        else
            verification_passed=false
            warn "  ⚠️  New member missing content:"
            [ "$has_line1" = false ] && warn "    - Missing LINE1"
            [ "$has_line2" = false ] && warn "    - Missing LINE2"
            [ "$has_line3" = false ] && warn "    - Missing LINE3"
            [ "$has_final" = false ] && warn "    - Missing FINAL"
        fi
    fi
    
    # Check sync status
    log "Phase 11: Sync status..."
    for i in 1 2 $NEW_MEMBER; do
        local status=$(api_sync_status $i "$TOPIC_ID")
        info "  Node-$i: $status"
    done
    
    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "✅ PASSED: New member received full document via initial sync"
    else
        warn "❌ FAILED: New member did not receive complete document"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Initial Sync for New Member"
    log "=========================================="
}

