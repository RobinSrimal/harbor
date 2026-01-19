#!/bin/bash
#
# Scenario: File Sharing Retry Logic - Source Offline
#
# Tests that incomplete file transfers are automatically retried when SOURCE goes offline:
# 1. Source shares a large file to peers
# 2. Mid-transfer, source goes offline (simulating connection drop)
# 3. Peers have partial blobs
# 4. Source comes back online
# 5. Retry task should complete the transfers
#
# Verification: Logs checked for "Blob complete" after retry
#

scenario_share_retry_source() {
    log "=========================================="
    log "SCENARIO: File Sharing Retry - Source Offline"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=4  # Source + 3 receivers
    local SOURCE_NODE=1
    local TEST_FILE_SIZE_MB=20  # 20 MB = 40 chunks, ensures partial transfer before interruption
    local TEST_FILE="$NODES_DIR/test_share_retry.bin"
    
    # Create test file
    log "Phase 0: Creating ${TEST_FILE_SIZE_MB}MB test file..."
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
    log "Phase 3: Creating topic..."
    INVITE=$(api_create_topic $SOURCE_NODE)
    TOPIC_ID="${INVITE:0:64}"
    info "Topic ID: ${TOPIC_ID:0:16}..."
    
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        sleep 0.5
        current_invite=$(api_get_invite $SOURCE_NODE "$TOPIC_ID")
    done
    sleep 3
    
    # Source starts sharing
    log "Phase 4: Source (node-$SOURCE_NODE) starting file share..."
    local share_result=$(api_share_file $SOURCE_NODE "$TOPIC_ID" "$TEST_FILE")
    info "Share initiated: $share_result"
    
    local BLOB_HASH=$(echo "$share_result" | grep -o '"hash":"[a-f0-9]*"' | cut -d'"' -f4)
    if [ -z "$BLOB_HASH" ]; then
        warn "Could not extract blob hash"
        rm -f "$TEST_FILE"
        return
    fi
    info "Blob hash: ${BLOB_HASH:0:16}..."
    
    # Wait briefly for announcements to propagate and some chunks to transfer
    # Transfer starts ~1s after share API, completes in ~0.5s
    # Wait 1.5s to catch mid-transfer (some chunks but not all)
    log "Phase 5: Waiting for partial transfer (1.5s)..."
    sleep 1.5
    
    # Check that receivers got the announcement
    log "Phase 6: Verifying announcements received..."
    local announcement_count=0
    for i in $(seq 2 $TOPIC_MEMBERS); do
        if grep -q "SHARE: Received file announcement" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            announcement_count=$((announcement_count + 1))
            
            # Check how many chunks received so far
            local chunks
            chunks=$(grep -c "Received chunks from peer" "$NODES_DIR/node-$i/output.log" 2>/dev/null) || chunks=0
            info "  Node-$i: announcement received, $chunks chunks so far"
        fi
    done
    
    if [ $announcement_count -eq 0 ]; then
        warn "No announcements received - cannot test retry"
        rm -f "$TEST_FILE"
        return
    fi
    
    # INTERRUPT: Source goes offline mid-transfer
    log "Phase 7: SOURCE GOING OFFLINE (simulating connection drop)..."
    stop_node $SOURCE_NODE
    sleep 2
    
    # Check current state - should be partial
    log "Phase 8: Checking partial state..."
    local partial_count=0
    for i in $(seq 2 $TOPIC_MEMBERS); do
        local status=$(api_share_status $i "$BLOB_HASH" 2>/dev/null)
        local state=$(echo "$status" | grep -o '"state":"[^"]*"' | cut -d'"' -f4)
        local chunks_complete=$(echo "$status" | grep -o '"chunks_complete":[0-9]*' | cut -d':' -f2)
        local total_chunks=$(echo "$status" | grep -o '"total_chunks":[0-9]*' | cut -d':' -f2)
        
        if [ "$state" = "partial" ] || [ -n "$chunks_complete" ]; then
            info "  Node-$i: state=$state, progress=$chunks_complete/$total_chunks chunks"
            if [ "$state" = "partial" ]; then
                partial_count=$((partial_count + 1))
            fi
        fi
    done
    
    log "  Partial blobs: $partial_count nodes"
    
    # Wait a bit with source offline
    log "Phase 9: Waiting with source offline (20s)..."
    sleep 20
    
    # Verify transfers are stuck
    log "Phase 10: Verifying transfers stuck (no completions while source offline)..."
    local stuck_check=true
    for i in $(seq 2 $TOPIC_MEMBERS); do
        if grep -qi "blob complete" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            info "  Node-$i: completed (had full file before interruption)"
            stuck_check=false
        fi
    done
    
    if [ "$stuck_check" = true ]; then
        log "  ✅ Transfers are stuck as expected (source offline)"
    fi
    
    # Source comes back online
    # NOTE: DB persists across restarts, so node keeps same identity and topic membership
    log "Phase 11: SOURCE COMING BACK ONLINE..."
    start_node $SOURCE_NODE
    sleep 2
    wait_for_api $SOURCE_NODE
    # No need to rejoin topic - DB persistence preserves membership and blob metadata
    
    # Wait for retry task to kick in (testing mode interval: 10s)
    # We wait 90s to allow multiple retry cycles
    log "Phase 12: Waiting for retry task to complete transfers (90s)..."
    log "  (Retry task runs every 10s in testing mode)"
    
    local check_interval=15
    local max_wait=90
    local waited=0
    local all_complete=false
    
    local completed_nodes=""
    while [ $waited -lt $max_wait ]; do
        sleep $check_interval
        waited=$((waited + check_interval))
        
        local complete_count=0
        for i in $(seq 2 $TOPIC_MEMBERS); do
            if grep -qi "blob complete" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
                complete_count=$((complete_count + 1))
                # Log when a node newly completes
                if [[ ! "$completed_nodes" =~ "node-$i" ]]; then
                    info "  ✅ Node-$i received complete file!"
                    completed_nodes="$completed_nodes node-$i"
                fi
            fi
        done
        
        info "  After ${waited}s: $complete_count/$((TOPIC_MEMBERS - 1)) complete"
        
        if [ $complete_count -eq $((TOPIC_MEMBERS - 1)) ]; then
            all_complete=true
            break
        fi
    done
    
    # Final verification
    log "Phase 13: Final verification..."
    local final_complete=0
    local retry_evidence=0
    
    for i in $(seq 2 $TOPIC_MEMBERS); do
        # Check for completion
        if grep -qi "blob complete" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            log "  ✅ Node-$i: file complete"
            final_complete=$((final_complete + 1))
        else
            local status=$(api_share_status $i "$BLOB_HASH" 2>/dev/null)
            local state=$(echo "$status" | grep -o '"state":"[^"]*"' | cut -d'"' -f4)
            local chunks=$(echo "$status" | grep -o '"chunks_complete":[0-9]*' | cut -d':' -f2)
            local total=$(echo "$status" | grep -o '"total_chunks":[0-9]*' | cut -d':' -f2)
            warn "  ⚠️  Node-$i: state=$state, $chunks/$total chunks"
        fi
        
        # Look for evidence of retry task activity
        if grep -q "Attempting to pull incomplete blob" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            retry_evidence=$((retry_evidence + 1))
        fi
    done
    
    # Cleanup
    rm -f "$TEST_FILE"
    
    # Summary
    local expected=$((TOPIC_MEMBERS - 1))
    log ""
    log "Verification Summary:"
    log "  Announcements received: $announcement_count/$expected"
    log "  Partial blobs detected: $partial_count"
    log "  Retry task evidence: $retry_evidence nodes"
    log "  Final completions: $final_complete/$expected"
    
    if [ $final_complete -eq $expected ]; then
        log "✅ Source retry: PASSED - All transfers completed after source returned!"
    elif [ $final_complete -gt 0 ]; then
        warn "⚠️  Source retry: PARTIAL - $final_complete/$expected completed"
        if [ $retry_evidence -gt 0 ]; then
            info "   (Retry task was active, may need more time)"
        fi
    else
        warn "❌ Source retry: FAILED - No transfers completed after source returned"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: File Sharing Retry - Source Offline"
    log "=========================================="
}

