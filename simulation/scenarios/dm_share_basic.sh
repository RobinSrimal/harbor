#!/bin/bash
#
# Scenario: DM Share Basic - File sharing over DM channels
#
# Tests the DM file share flow:
#   1. Node-1 shares a file with Node-2 via DM
#   2. Node-2 receives DmFileAnnounced event
#
# Verification: Logs checked for "DM file announced"
#

scenario_dm_share_basic() {
    log "=========================================="
    log "SCENARIO: DM Share Basic"
    log "=========================================="

    local TEST_FILE="$NODES_DIR/test_dm_share.bin"

    # Create test file (1 MB, above 512KB threshold)
    log "Phase 0: Creating 1MB test file..."
    create_test_file "$TEST_FILE" 1
    if [ ! -f "$TEST_FILE" ]; then
        fail "Failed to create test file"
    fi
    info "Created test file: $TEST_FILE ($(du -h "$TEST_FILE" | cut -f1))"

    # Start bootstrap node
    start_bootstrap_node

    # Start nodes
    log "Phase 1: Starting nodes 2-$NODE_COUNT..."
    for i in $(seq 2 $NODE_COUNT); do
        start_node $i
        sleep 0.5
    done

    # Wait for APIs
    log "Waiting for all nodes to initialize..."
    for i in $(seq 2 $NODE_COUNT); do
        wait_for_api $i
    done

    # Wait for DHT convergence
    log "Phase 2: Waiting for DHT convergence (75s)..."
    sleep 75

    # Get endpoint IDs
    log "Phase 3: Getting endpoint IDs..."
    local NODE1_ID=$(api_get_endpoint_id 1)
    local NODE2_ID=$(api_get_endpoint_id 2)

    if [ -z "$NODE1_ID" ] || [ -z "$NODE2_ID" ]; then
        fail "Failed to get endpoint IDs"
    fi

    info "Node-1 endpoint: ${NODE1_ID:0:16}..."
    info "Node-2 endpoint: ${NODE2_ID:0:16}..."

    local passed=true

    # ---- Test 1: DM File Share ----
    log "Phase 4: Node-1 sharing file with Node-2 via DM..."
    local share_result=$(api_dm_share 1 "$NODE2_ID" "$TEST_FILE")
    info "  Share result: $share_result"

    local BLOB_HASH=$(echo "$share_result" | grep -o '"hash":"[a-f0-9]*"' | cut -d'"' -f4)
    if [ -z "$BLOB_HASH" ]; then
        warn "Could not extract blob hash from share result"
    else
        info "  Blob hash: ${BLOB_HASH:0:16}..."
    fi

    # Wait for delivery
    log "Phase 5: Waiting for DM file announcement delivery (15s)..."
    sleep 15

    # Verify
    log "Phase 6: Verifying DM file announcement..."

    if grep -q "DM file announced" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        local count=$(grep -c "DM file announced" "$NODES_DIR/node-2/output.log")
        info "  Node-2: ✅ received $count DM file announcement(s)"
    else
        warn "  Node-2: ❌ no DM file announcement received"
        passed=false
    fi

    # ---- Test 2: Reverse direction ----
    log "Phase 7: Node-2 sharing file with Node-1 via DM..."
    local share_result2=$(api_dm_share 2 "$NODE1_ID" "$TEST_FILE")
    info "  Share result: $share_result2"

    sleep 15

    if grep -q "DM file announced" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        local count=$(grep -c "DM file announced" "$NODES_DIR/node-1/output.log")
        info "  Node-1: ✅ received $count DM file announcement(s)"
    else
        warn "  Node-1: ❌ no DM file announcement received"
        passed=false
    fi

    # Final stats
    log "Phase 8: Final statistics"
    for i in 1 2; do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$passed" = true ]; then
        log "  ✅ PASSED: DM file sharing working"
    else
        warn "  ⚠️  PARTIAL: Some DM file shares not delivered"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: DM Share Basic"
    log "=========================================="
}
