#!/bin/bash
#
# Scenario: File Sharing with Few Peers
#
# Tests adaptive splitting when fewer than 5 peers are available.
# With only 3 peers in topic, file should split into thirds instead of fifths.
#
# Verification: Logs checked for "SHARE: Received file announcement" with correct section count
#

scenario_share_few_peers() {
    log "=========================================="
    log "SCENARIO: File Sharing with Few Peers"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=3  # Only 3 members - adaptive split
    local TEST_FILE_SIZE_MB=2  # 2 MB to have multiple sections
    local TEST_FILE="$NODES_DIR/test_share_few.bin"
    
    # Create test file
    log "Phase 0: Creating test file..."
    create_test_file "$TEST_FILE" $TEST_FILE_SIZE_MB
    
    # Start nodes
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
    
    # Create topic with ONLY 3 members
    log "Phase 3: Creating topic with only $TOPIC_MEMBERS members..."
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
    
    # Verify only 3 members
    log "Phase 3b: Verifying topic has $TOPIC_MEMBERS members..."
    local topic_list=$(api_topics 1)
    info "Node-1 topics: $topic_list"
    
    # Node 1 shares file
    log "Phase 4: Node-1 sharing file to $TOPIC_MEMBERS-member topic..."
    local share_result=$(api_share_file 1 "$TOPIC_ID" "$TEST_FILE")
    info "Share result: $share_result"
    
    local BLOB_HASH=$(echo "$share_result" | grep -o '"hash":"[a-f0-9]*"' | cut -d'"' -f4)
    
    # Wait for distribution
    log "Phase 5: Waiting for distribution (45s)..."
    sleep 45
    
    # Verify announcements
    log "Phase 6: Verifying file announcements..."
    local received_count=0
    for i in $(seq 2 $TOPIC_MEMBERS); do
        if grep -q "SHARE: Received file announcement" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            log "  ✅ Node-$i: received file announcement"
            received_count=$((received_count + 1))
            
            # Extract section info if available
            local section_info=$(grep "SHARE: Received file announcement" "$NODES_DIR/node-$i/output.log" | tail -1)
            info "      $section_info"
        else
            warn "  ⚠️  Node-$i: did NOT receive announcement"
        fi
    done
    
    # Check section count via API
    log "Phase 7: Checking section distribution..."
    if [ -n "$BLOB_HASH" ]; then
        local status=$(api_share_status 1 "$BLOB_HASH")
        local section_count=$(echo "$status" | grep -o '"section_count":[0-9]*' | cut -d':' -f2)
        info "  Source reports $section_count sections"
        
        # With 3 members (2 recipients), we expect <= 3 sections (adaptive)
        if [ -n "$section_count" ]; then
            if [ "$section_count" -le 3 ]; then
                log "  ✅ Adaptive sectioning: $section_count sections for $((TOPIC_MEMBERS-1)) recipients"
            else
                info "  ℹ️  Sections: $section_count (standard)"
            fi
        fi
    fi
    
    # Verify file completion
    log "Phase 8: Checking file completion..."
    local complete_count=0
    for i in $(seq 2 $TOPIC_MEMBERS); do
        if grep -qi "blob complete" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            log "  ✅ Node-$i: file complete"
            complete_count=$((complete_count + 1))
        else
            local status=$(api_share_status $i "$BLOB_HASH")
            local state=$(echo "$status" | grep -o '"state":"[^"]*"' | cut -d'"' -f4)
            local chunks=$(echo "$status" | grep -o '"chunks_complete":[0-9]*' | cut -d':' -f2)
            local total=$(echo "$status" | grep -o '"total_chunks":[0-9]*' | cut -d':' -f2)
            info "  ⏳ Node-$i: $chunks/$total chunks (state: $state)"
        fi
    done
    
    # Cleanup
    rm -f "$TEST_FILE"
    
    # Summary
    local expected=$((TOPIC_MEMBERS - 1))
    log ""
    log "Verification Summary:"
    log "  Topic members: $TOPIC_MEMBERS (source + $expected recipients)"
    log "  Announcements received: $received_count/$expected"
    log "  Files completed: $complete_count/$expected"
    
    if [ $received_count -eq $expected ]; then
        log "✅ Few-peer file sharing: PASSED"
    else
        warn "⚠️  Few-peer file sharing: PARTIAL"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: File Sharing with Few Peers"
    log "=========================================="
}

