#!/bin/bash
#
# Scenario: DM Sync Basic - CRDT sync over DM channels
#
# Tests the full DM sync flow:
#   1. Send sync updates between two peers via DM
#   2. Request sync state via DM
#   3. Respond with full state via SYNC_ALPN
#
# Verification: Logs checked for "DM sync update received",
# "DM sync request received", and "DM sync response received".
#

scenario_dm_sync_basic() {
    log "=========================================="
    log "SCENARIO: DM Sync Basic"
    log "=========================================="

    # Start bootstrap node first
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

    # ---- Test 1: DM Sync Update ----
    log "Phase 4: Sending DM sync updates..."
    local update_data=$(mock_crdt_update "DM-SYNC-UPDATE-TEST")
    local result1=$(api_dm_sync_update 1 "$NODE2_ID" "$update_data")
    info "  Node-1 → Node-2 sync update: $result1"

    sleep 2

    local update_data2=$(mock_crdt_update "DM-SYNC-UPDATE-REVERSE")
    local result2=$(api_dm_sync_update 2 "$NODE1_ID" "$update_data2")
    info "  Node-2 → Node-1 sync update: $result2"

    # Wait for delivery
    log "Phase 5: Waiting for DM sync delivery (15s)..."
    sleep 15

    # Verify sync updates
    log "Phase 6: Verifying DM sync updates..."

    if grep -q "DM sync update received" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        local count=$(grep -c "DM sync update received" "$NODES_DIR/node-2/output.log")
        info "  Node-2: ✅ received $count DM sync update(s)"
    else
        warn "  Node-2: ❌ no DM sync update received"
        passed=false
    fi

    if grep -q "DM sync update received" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        local count=$(grep -c "DM sync update received" "$NODES_DIR/node-1/output.log")
        info "  Node-1: ✅ received $count DM sync update(s)"
    else
        warn "  Node-1: ❌ no DM sync update received"
        passed=false
    fi

    # ---- Test 2: DM Sync Request + Response ----
    log "Phase 7: Testing DM sync request/response flow..."

    # Node-1 requests sync state from Node-2
    local result3=$(api_dm_sync_request 1 "$NODE2_ID")
    info "  Node-1 requests sync from Node-2: $result3"

    # Wait for request delivery
    sleep 15

    # Check Node-2 received the request
    if grep -q "DM sync request received" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        info "  Node-2: ✅ received DM sync request"
    else
        warn "  Node-2: ❌ no DM sync request received"
        passed=false
    fi

    # Node-2 responds with full state via SYNC_ALPN
    log "Phase 8: Node-2 responding with full state via SYNC_ALPN..."
    local snapshot_data=$(mock_crdt_snapshot "DM-FULL-STATE-SNAPSHOT")
    local result4=$(api_dm_sync_respond 2 "$NODE1_ID" "$snapshot_data")
    info "  Node-2 → Node-1 sync response: $result4"

    # Wait for SYNC_ALPN delivery
    sleep 15

    # Check Node-1 received the response
    if grep -q "DM sync response received" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        info "  Node-1: ✅ received DM sync response"
    else
        warn "  Node-1: ❌ no DM sync response received"
        passed=false
    fi

    # ---- Test 3: Multi-pair sync ----
    log "Phase 9: Multi-pair DM sync test..."
    local NODE3_ID=$(api_get_endpoint_id 3)
    local NODE4_ID=$(api_get_endpoint_id 4)

    if [ -n "$NODE3_ID" ] && [ -n "$NODE4_ID" ]; then
        local update3=$(mock_crdt_update "DM-SYNC-PAIR2-TEST")
        api_dm_sync_update 3 "$NODE4_ID" "$update3" > /dev/null
        sleep 15

        if grep -q "DM sync update received" "$NODES_DIR/node-4/output.log" 2>/dev/null; then
            info "  Node-4: ✅ received DM sync update from Node-3"
        else
            warn "  Node-4: ⚠️  no DM sync update from Node-3"
        fi
    fi

    # Final stats
    log "Phase 10: Final statistics"
    for i in $(seq 1 4); do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$passed" = true ]; then
        log "  ✅ PASSED: DM sync basic working (updates + request/response)"
    else
        warn "  ⚠️  PARTIAL: Some DM sync operations not delivered"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: DM Sync Basic"
    log "=========================================="
}
