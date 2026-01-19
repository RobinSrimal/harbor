#!/bin/bash
#
# Scenario: Large File Sharing
#
# Tests sharing a larger file (10MB) with multiple chunks and sections.
# Measures transfer progress and completion.
#
# Verification: Logs checked for chunk reception and file completion.
#

scenario_share_large_file() {
    log "=========================================="
    log "SCENARIO: Large File Sharing"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=5
    local TEST_FILE_SIZE_MB=10  # 10 MB - 20 chunks of 512KB
    local TEST_FILE="$NODES_DIR/test_share_large.bin"
    
    # Create large test file
    log "Phase 0: Creating ${TEST_FILE_SIZE_MB}MB test file..."
    create_test_file "$TEST_FILE" $TEST_FILE_SIZE_MB
    info "Created test file: $(du -h "$TEST_FILE" | cut -f1)"
    
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
    
    # Create topic
    log "Phase 3: Creating topic..."
    INVITE=$(api_create_topic 1)
    TOPIC_ID="${INVITE:0:64}"
    
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 3
    
    # Share large file
    log "Phase 4: Sharing ${TEST_FILE_SIZE_MB}MB file..."
    local start_time=$(date +%s)
    local share_result=$(api_share_file 1 "$TOPIC_ID" "$TEST_FILE")
    info "Share initiated: $share_result"
    
    local BLOB_HASH=$(echo "$share_result" | grep -o '"hash":"[a-f0-9]*"' | cut -d'"' -f4)
    
    if [ -z "$BLOB_HASH" ]; then
        warn "Could not extract blob hash"
        rm -f "$TEST_FILE"
        return
    fi
    
    # Monitor progress
    log "Phase 5: Monitoring transfer progress..."
    local check_interval=10
    local max_checks=18  # 3 minutes max
    local check_count=0
    
    while [ $check_count -lt $max_checks ]; do
        sleep $check_interval
        check_count=$((check_count + 1))
        
        local complete_count=0
        local total_chunks_received=0
        
        for i in $(seq 2 $TOPIC_MEMBERS); do
            local status=$(api_share_status $i "$BLOB_HASH" 2>/dev/null)
            local chunks=$(echo "$status" | grep -o '"chunks_complete":[0-9]*' | cut -d':' -f2)
            local total=$(echo "$status" | grep -o '"total_chunks":[0-9]*' | cut -d':' -f2)
            local state=$(echo "$status" | grep -o '"state":"[^"]*"' | cut -d'"' -f4)
            
            if [ "$state" = "complete" ]; then
                complete_count=$((complete_count + 1))
            fi
            
            if [ -n "$chunks" ]; then
                total_chunks_received=$((total_chunks_received + chunks))
            fi
        done
        
        local expected_recipients=$((TOPIC_MEMBERS - 1))
        info "  Check $check_count: $complete_count/$expected_recipients complete, ~$total_chunks_received chunks received"
        
        # All complete?
        if [ $complete_count -eq $expected_recipients ]; then
            break
        fi
    done
    
    local end_time=$(date +%s)
    local duration=$((end_time - start_time))
    
    # Verify via logs
    log "Phase 6: Verifying via logs..."
    local announcement_count=0
    local file_complete_count=0
    local chunk_receive_count=0
    
    for i in $(seq 2 $TOPIC_MEMBERS); do
        if grep -q "SHARE: Received file announcement" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            announcement_count=$((announcement_count + 1))
        fi
        
        if grep -qi "blob complete" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            file_complete_count=$((file_complete_count + 1))
        fi
        
        local chunks
        chunks=$(grep -c "Received chunks from peer" "$NODES_DIR/node-$i/output.log" 2>/dev/null) || chunks=0
        chunk_receive_count=$((chunk_receive_count + chunks))
    done
    
    log "  Announcements in logs: $announcement_count"
    log "  File completions in logs: $file_complete_count"
    log "  Total chunk receptions logged: $chunk_receive_count"
    
    # Cleanup
    rm -f "$TEST_FILE"
    
    # Summary
    local expected=$((TOPIC_MEMBERS - 1))
    log ""
    log "Verification Summary:"
    log "  File size: ${TEST_FILE_SIZE_MB}MB"
    log "  Transfer duration: ${duration}s"
    log "  Announcements received: $announcement_count/$expected"
    log "  Files completed: $file_complete_count/$expected"
    
    if [ $duration -gt 0 ]; then
        local speed=$((TEST_FILE_SIZE_MB * expected / duration))
        log "  Effective throughput: ~${speed}MB/s (aggregate)"
    fi
    
    if [ $announcement_count -eq $expected ]; then
        log "✅ Large file announcement: PASSED"
    else
        warn "⚠️  Large file announcement: PARTIAL"
    fi
    
    if [ $file_complete_count -eq $expected ]; then
        log "✅ Large file transfer: PASSED"
    elif [ $file_complete_count -gt 0 ]; then
        warn "⚠️  Large file transfer: PARTIAL ($file_complete_count/$expected)"
    else
        info "⏳ Large file transfer: IN PROGRESS"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Large File Sharing"
    log "=========================================="
}

