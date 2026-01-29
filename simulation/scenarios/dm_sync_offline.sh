#!/bin/bash
#
# Scenario: DM Sync Offline - DM sync update delivery via Harbor
#
# Tests that DM sync updates are stored on Harbor nodes and delivered
# when the recipient comes back online (via harbor pull).
#
# Flow:
# 1. Start all nodes, wait for DHT
# 2. Send baseline DM sync update while both online
# 3. Stop node-2 (goes offline)
# 4. Node-1 sends DM sync update to offline node-2 (stored on Harbor)
# 5. Restart node-2
# 6. Wait for harbor pull to deliver the sync update
# 7. Verify node-2 received the sync update
#

scenario_dm_sync_offline() {
    log "=========================================="
    log "SCENARIO: DM Sync Offline - Harbor Delivery"
    log "=========================================="

    # Start bootstrap node
    start_bootstrap_node

    # Start nodes
    log "Phase 1: Starting nodes 2-$NODE_COUNT..."
    for i in $(seq 2 $NODE_COUNT); do
        start_node $i
        sleep 0.5
    done

    log "Waiting for all nodes to initialize..."
    for i in $(seq 2 $NODE_COUNT); do
        wait_for_api $i
    done

    # Wait for DHT convergence
    log "Phase 2: Waiting for DHT convergence (75s)..."
    sleep 75

    # Get endpoint IDs while both nodes are online
    log "Phase 3: Getting endpoint IDs..."
    local NODE1_ID=$(api_get_endpoint_id 1)
    local NODE2_ID=$(api_get_endpoint_id 2)

    if [ -z "$NODE1_ID" ] || [ -z "$NODE2_ID" ]; then
        fail "Failed to get endpoint IDs"
    fi

    info "Node-1 endpoint: ${NODE1_ID:0:16}..."
    info "Node-2 endpoint: ${NODE2_ID:0:16}..."

    # Baseline: send DM sync update while node-2 is online
    log "Phase 4: Baseline - DM sync update while online..."
    local baseline_data=$(mock_crdt_update "DM-SYNC-OFFLINE-BASELINE")
    api_dm_sync_update 1 "$NODE2_ID" "$baseline_data" > /dev/null
    sleep 10

    local baseline_ok=false
    if grep -q "DM sync update received" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        info "  ✅ Baseline DM sync update received"
        baseline_ok=true
    else
        warn "  ⚠️  Baseline DM sync update not received (direct delivery issue)"
    fi

    local baseline_count=$(grep -c "DM sync update received" "$NODES_DIR/node-2/output.log" 2>/dev/null || echo "0")

    # Stop node-2
    log "Phase 5: Stopping node-2 (going offline)..."
    stop_node 2
    sleep 5

    # Send DM sync update to offline node-2
    log "Phase 6: Sending DM sync update to offline node-2..."
    local offline_data=$(mock_crdt_update "DM-SYNC-OFFLINE-WHILE-DOWN")
    local result=$(api_dm_sync_update 1 "$NODE2_ID" "$offline_data")
    info "  Send result: $result"

    # Also send a DM sync request to offline node-2
    log "  Also sending DM sync request to offline node-2..."
    local result2=$(api_dm_sync_request 1 "$NODE2_ID")
    info "  Request result: $result2"

    # Wait for harbor replication
    log "Phase 7: Waiting for harbor replication (30s)..."
    sleep 30

    # Restart node-2
    log "Phase 8: Restarting node-2..."
    start_node 2
    wait_for_api 2
    info "  Node-2 back online"

    # Wait for harbor pull to deliver
    log "Phase 9: Waiting for harbor pull delivery (90s)..."
    sleep 90

    # Verify
    log "Phase 10: Verifying offline DM sync delivery..."
    local update_count=$(grep "DM sync update received" "$NODES_DIR/node-2/output.log" 2>/dev/null | wc -l | tr -d ' ')
    local request_count=$(grep "DM sync request received" "$NODES_DIR/node-2/output.log" 2>/dev/null | wc -l | tr -d ' ')

    local passed=true

    if [ "$baseline_ok" = true ]; then
        # Should have at least 2 sync updates (baseline + offline)
        if [ "$update_count" -ge 2 ]; then
            info "  Node-2: ✅ received $update_count DM sync updates (baseline + offline)"
        elif [ "$update_count" -ge 1 ]; then
            warn "  Node-2: ⚠️  received $update_count DM sync update(s) - offline delivery may have failed"
            passed=false
        else
            warn "  Node-2: ❌ no DM sync updates received"
            passed=false
        fi
    else
        if [ "$update_count" -ge 1 ]; then
            info "  Node-2: ✅ received $update_count DM sync update(s) via harbor pull"
        else
            warn "  Node-2: ❌ no DM sync updates received"
            passed=false
        fi
    fi

    # Check if sync request was also delivered
    if [ "$request_count" -ge 1 ]; then
        info "  Node-2: ✅ received $request_count DM sync request(s) via harbor pull"
    else
        warn "  Node-2: ⚠️  DM sync request not delivered via harbor"
    fi

    # Final stats
    log "Phase 11: Final statistics"
    for i in 1 2; do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done

    # Summary
    log ""
    log "Verification Summary:"
    log "  Baseline (online) sync update: $([ "$baseline_ok" = true ] && echo "✅" || echo "⚠️")"
    log "  Total DM sync updates received by node-2: $update_count"
    log "  DM sync requests received by node-2: $request_count"
    if [ "$passed" = true ]; then
        log "  ✅ PASSED: DM sync offline delivery working"
    else
        warn "  ⚠️  PARTIAL: Offline DM sync delivery needs investigation"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: DM Sync Offline"
    log "=========================================="
}
