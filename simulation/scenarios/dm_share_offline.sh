#!/bin/bash
#
# Scenario: DM Share Offline - File announcement delivery via Harbor
#
# Tests that DM file announcements are stored on Harbor nodes and delivered
# when the recipient comes back online (via harbor pull).
#
# Flow:
# 1. Start all nodes, wait for DHT
# 2. Send baseline DM file share while both online
# 3. Stop node-2 (goes offline)
# 4. Node-1 shares file with offline node-2 (stored on Harbor)
# 5. Restart node-2
# 6. Wait for harbor pull to deliver the file announcement
# 7. Verify node-2 received the announcement
#

scenario_dm_share_offline() {
    log "=========================================="
    log "SCENARIO: DM Share Offline - Harbor Delivery"
    log "=========================================="

    local TEST_FILE="$NODES_DIR/test_dm_share_offline.bin"

    # Create test file (1 MB)
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

    # Baseline: share file while node-2 is online
    log "Phase 4: Baseline - DM file share while online..."
    local share_result=$(api_dm_share 1 "$NODE2_ID" "$TEST_FILE")
    info "  Share result: $share_result"
    sleep 10

    local baseline_ok=false
    if grep -q "DM file announced" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        info "  ✅ Baseline DM file announcement received"
        baseline_ok=true
    else
        warn "  ⚠️  Baseline DM file announcement not received (direct delivery issue)"
    fi

    local baseline_count=$(grep -c "DM file announced" "$NODES_DIR/node-2/output.log" 2>/dev/null || echo "0")

    # Stop node-2
    log "Phase 5: Stopping node-2 (going offline)..."
    stop_node 2
    sleep 5

    # Share file with offline node-2
    log "Phase 6: Sharing file with offline node-2..."
    # Create a different test file to distinguish
    local TEST_FILE2="$NODES_DIR/test_dm_share_offline2.bin"
    create_test_file "$TEST_FILE2" 1
    local share_result2=$(api_dm_share 1 "$NODE2_ID" "$TEST_FILE2")
    info "  Share result: $share_result2"

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
    log "Phase 10: Verifying offline DM file share delivery..."
    local announce_count=$(grep "DM file announced" "$NODES_DIR/node-2/output.log" 2>/dev/null | wc -l | tr -d ' ')

    local passed=true

    if [ "$baseline_ok" = true ]; then
        # Should have at least 2 announcements (baseline + offline)
        if [ "$announce_count" -ge 2 ]; then
            info "  Node-2: ✅ received $announce_count DM file announcements (baseline + offline)"
        elif [ "$announce_count" -ge 1 ]; then
            warn "  Node-2: ⚠️  received $announce_count DM file announcement(s) - offline delivery may have failed"
            passed=false
        else
            warn "  Node-2: ❌ no DM file announcements received"
            passed=false
        fi
    else
        if [ "$announce_count" -ge 1 ]; then
            info "  Node-2: ✅ received $announce_count DM file announcement(s) via harbor pull"
        else
            warn "  Node-2: ❌ no DM file announcements received"
            passed=false
        fi
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
    log "  Baseline (online) file announcement: $([ "$baseline_ok" = true ] && echo "✅" || echo "⚠️")"
    log "  Total DM file announcements received by node-2: $announce_count"
    if [ "$passed" = true ]; then
        log "  ✅ PASSED: DM file share offline delivery working"
    else
        warn "  ⚠️  PARTIAL: Offline DM file share delivery needs investigation"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: DM Share Offline"
    log "=========================================="
}
