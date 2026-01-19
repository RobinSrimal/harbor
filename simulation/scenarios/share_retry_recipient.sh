#!/bin/bash
#
# Scenario: File Sharing Retry Logic - Recipient Offline
#
# Tests that incomplete file transfers are automatically retried when RECIPIENT goes offline:
# 1. Source shares a large file to peers
# 2. Mid-transfer, one recipient goes offline
# 3. Other recipients complete
# 4. Offline recipient comes back online
# 5. Retry task on recipient should complete the download
#
# Verification: Logs checked for "Blob complete" after retry
#

scenario_share_retry_recipient() {
    log "=========================================="
    log "SCENARIO: File Sharing Retry - Recipient Offline"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=4  # Source + 3 receivers
    local SOURCE_NODE=1
    local OFFLINE_RECIPIENT=2  # This recipient will go offline
    local TEST_FILE_SIZE_MB=20
    local TEST_FILE="$NODES_DIR/test_share_retry_recipient.bin"
    
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
    log "Phase 4: Source starting file share..."
    local share_result=$(api_share_file $SOURCE_NODE "$TOPIC_ID" "$TEST_FILE")
    info "Share initiated: $share_result"
    
    local BLOB_HASH=$(echo "$share_result" | grep -o '"hash":"[a-f0-9]*"' | cut -d'"' -f4)
    if [ -z "$BLOB_HASH" ]; then
        warn "Could not extract blob hash"
        rm -f "$TEST_FILE"
        return
    fi
    info "Blob hash: ${BLOB_HASH:0:16}..."
    
    # Wait for partial transfer to recipient
    # Transfer starts ~1s after share API, completes in ~0.5s
    # Wait 1.5s to catch mid-transfer (some chunks but not all)
    log "Phase 5: Waiting for partial transfer (1.5s)..."
    sleep 1.5
    
    # Check recipient state before going offline
    log "Phase 6: Checking recipient-$OFFLINE_RECIPIENT state..."
    local status=$(api_share_status $OFFLINE_RECIPIENT "$BLOB_HASH" 2>/dev/null)
    local chunks_before=$(echo "$status" | grep -o '"chunks_complete":[0-9]*' | cut -d':' -f2)
    info "  Recipient-$OFFLINE_RECIPIENT: $chunks_before chunks before offline"
    
    # INTERRUPT: Recipient goes offline mid-transfer
    log "Phase 7: RECIPIENT-$OFFLINE_RECIPIENT GOING OFFLINE..."
    stop_node $OFFLINE_RECIPIENT
    sleep 2
    
    # Wait for other recipients to complete (source stays online)
    log "Phase 8: Waiting for other recipients to complete (60s)..."
    local check_interval=15
    local max_wait=60
    local waited=0
    
    local completed_nodes=""
    while [ $waited -lt $max_wait ]; do
        sleep $check_interval
        waited=$((waited + check_interval))
        
        local others_complete=0
        for i in $(seq 3 $TOPIC_MEMBERS); do
            if grep -qi "blob complete" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
                others_complete=$((others_complete + 1))
                # Log when a node newly completes
                if [[ ! "$completed_nodes" =~ "node-$i" ]]; then
                    info "  ✅ Node-$i received complete file!"
                    completed_nodes="$completed_nodes node-$i"
                fi
            fi
        done
        info "  After ${waited}s: $others_complete/$((TOPIC_MEMBERS - 2)) other recipients complete"
        
        if [ $others_complete -eq $((TOPIC_MEMBERS - 2)) ]; then
            break
        fi
    done
    
    # Recipient comes back online
    # NOTE: DB persists across restarts, so node keeps same identity and topic membership
    log "Phase 9: RECIPIENT-$OFFLINE_RECIPIENT COMING BACK ONLINE..."
    start_node $OFFLINE_RECIPIENT
    sleep 2
    wait_for_api $OFFLINE_RECIPIENT
    # No need to rejoin topic - DB persistence preserves membership and blob metadata
    
    # Wait for retry task to complete the download
    log "Phase 10: Waiting for recipient's retry task (90s)..."
    local recipient_complete=false
    local max_retry_wait=90
    local retry_waited=0
    
    while [ $retry_waited -lt $max_retry_wait ]; do
        sleep 15
        retry_waited=$((retry_waited + 15))
        
        if grep -qi "blob complete" "$NODES_DIR/node-$OFFLINE_RECIPIENT/output.log" 2>/dev/null; then
            recipient_complete=true
            info "  After ${retry_waited}s: Recipient-$OFFLINE_RECIPIENT completed!"
            break
        else
            local status=$(api_share_status $OFFLINE_RECIPIENT "$BLOB_HASH" 2>/dev/null)
            local chunks=$(echo "$status" | grep -o '"chunks_complete":[0-9]*' | cut -d':' -f2)
            info "  After ${retry_waited}s: Recipient-$OFFLINE_RECIPIENT at $chunks chunks"
        fi
    done
    
    # Final verification
    log "Phase 11: Final verification..."
    local retry_evidence=0
    if grep -q "Attempting to pull incomplete blob" "$NODES_DIR/node-$OFFLINE_RECIPIENT/output.log" 2>/dev/null; then
        retry_evidence=1
    fi
    
    # Cleanup
    rm -f "$TEST_FILE"
    
    # Summary
    log ""
    log "Verification Summary:"
    log "  Chunks before offline: $chunks_before"
    log "  Retry task evidence: $retry_evidence"
    log "  Recipient completed: $recipient_complete"
    
    if [ "$recipient_complete" = true ]; then
        log "✅ Recipient retry: PASSED - Transfer completed after recipient reconnected!"
    elif [ $retry_evidence -gt 0 ]; then
        warn "⚠️  Recipient retry: IN PROGRESS - Retry task active but not yet complete"
    else
        warn "❌ Recipient retry: FAILED - Transfer not completed after recipient returned"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: File Sharing Retry - Recipient Offline"
    log "=========================================="
}

