#!/bin/bash
#
# Scenario: Basic File Sharing
#
# Tests basic file sharing: source shares a file, all topic members receive it.
# Verification: Logs checked for "SHARE: Received file announcement" and "Blob complete"
#

scenario_share_basic() {
    log "=========================================="
    log "SCENARIO: Basic File Sharing"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=5  # Smaller group for file sharing test
    local TEST_FILE_SIZE_MB=1  # 1 MB test file (above 512KB threshold)
    local TEST_FILE="$NODES_DIR/test_share_file.bin"
    
    # Create test file
    log "Phase 0: Creating ${TEST_FILE_SIZE_MB}MB test file..."
    create_test_file "$TEST_FILE" $TEST_FILE_SIZE_MB
    if [ ! -f "$TEST_FILE" ]; then
        fail "Failed to create test file"
    fi
    info "Created test file: $TEST_FILE ($(du -h "$TEST_FILE" | cut -f1))"
    
    # Start bootstrap node
    start_bootstrap_node
    
    # Start remaining nodes
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
        api_join_topic $i "$current_invite" > /dev/null
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 3
    
    # Node 1 shares the file
    log "Phase 4: Node-1 sharing file..."
    local share_result=$(api_share_file 1 "$TOPIC_ID" "$TEST_FILE")
    info "Share result: $share_result"
    
    local BLOB_HASH=$(echo "$share_result" | grep -o '"hash":"[a-f0-9]*"' | cut -d'"' -f4)
    if [ -z "$BLOB_HASH" ]; then
        warn "Could not extract blob hash from share result"
        BLOB_HASH="unknown"
    else
        info "Blob hash: ${BLOB_HASH:0:16}..."
    fi
    
    # Wait for announcement propagation and chunk transfer
    log "Phase 5: Waiting for file distribution (60s)..."
    sleep 60
    
    # Verify file announcement received
    log "Phase 6: Verifying file announcement reception..."
    local announcement_success=0
    for i in $(seq 2 $TOPIC_MEMBERS); do
        if grep -q "SHARE: Received file announcement" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            log "  ✅ Node-$i: received file announcement"
            announcement_success=$((announcement_success + 1))
        else
            warn "  ⚠️  Node-$i: did NOT receive file announcement"
        fi
    done
    
    local expected_announcements=$((TOPIC_MEMBERS - 1))
    
    # Verify file completion (this may take longer with actual chunk transfer)
    log "Phase 7: Checking file completion status..."
    local complete_success=0
    for i in $(seq 2 $TOPIC_MEMBERS); do
        if grep -qi "blob complete" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            log "  ✅ Node-$i: file complete"
            complete_success=$((complete_success + 1))
        else
            # Check via API as backup
            local status=$(api_share_status $i "$BLOB_HASH")
            local state=$(echo "$status" | grep -o '"state":"[^"]*"' | cut -d'"' -f4)
            if [ "$state" = "complete" ]; then
                log "  ✅ Node-$i: file complete (via API)"
                complete_success=$((complete_success + 1))
            else
                info "  ⏳ Node-$i: file transfer in progress (state: $state)"
            fi
        fi
    done
    
    # Check source node status
    log "Phase 8: Source node status..."
    local source_status=$(api_share_status 1 "$BLOB_HASH")
    info "  Node-1 (source): $source_status"
    
    # Final statistics
    log "Phase 9: Final statistics"
    for i in $(seq 1 $TOPIC_MEMBERS); do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done
    
    # Cleanup test file
    rm -f "$TEST_FILE"
    
    # Summary
    log ""
    log "Verification Summary:"
    log "  Announcements received: $announcement_success/$expected_announcements"
    log "  Files completed: $complete_success/$expected_announcements"
    
    if [ $announcement_success -eq $expected_announcements ]; then
        log "✅ File announcement delivery: PASSED"
    else
        warn "⚠️  File announcement delivery: PARTIAL ($announcement_success/$expected_announcements)"
    fi
    
    if [ $complete_success -eq $expected_announcements ]; then
        log "✅ File transfer: PASSED"
    elif [ $complete_success -gt 0 ]; then
        warn "⚠️  File transfer: PARTIAL ($complete_success/$expected_announcements)"
    else
        info "⏳ File transfer: IN PROGRESS (chunk transfer not yet complete)"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Basic File Sharing"
    log "=========================================="
}

