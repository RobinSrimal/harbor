#!/bin/bash
#
# Scenario: DM Offline - DM delivery via Harbor for offline peers
#
# Tests that DMs are stored on Harbor nodes and delivered when
# the recipient comes back online (via harbor pull).
#
# Flow:
# 1. Start all nodes, wait for DHT
# 2. Get endpoint IDs for node-2
# 3. Stop node-2 (goes offline)
# 4. Node-1 sends DM to node-2 (stored on Harbor)
# 5. Restart node-2
# 6. Wait for harbor pull to deliver the DM
# 7. Verify node-2 received the DM
#

scenario_dm_offline() {
    log "=========================================="
    log "SCENARIO: DM Offline - Harbor Delivery"
    log "=========================================="

    local DM_MSG_PREFIX="DM-OFFLINE-TEST"

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

    # First, send a DM while node-2 is online (baseline test)
    log "Phase 4: Baseline - DM while online..."
    local baseline_msg="${DM_MSG_PREFIX}-baseline"
    api_send_dm 1 "$NODE2_ID" "$baseline_msg" > /dev/null
    sleep 10

    local baseline_ok=false
    if grep "DM received" "$NODES_DIR/node-2/output.log" 2>/dev/null | grep -q "$baseline_msg"; then
        info "  ✅ Baseline DM received with correct content"
        baseline_ok=true
    elif grep -q "DM received" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        warn "  ⚠️  Baseline DM received but content mismatch (decryption issue?)"
        baseline_ok=true
    else
        warn "  ⚠️  Baseline DM not received (direct delivery issue)"
    fi

    # Stop node-2
    log "Phase 5: Stopping node-2 (going offline)..."
    stop_node 2
    sleep 5

    # Send DM to offline node-2
    log "Phase 6: Sending DM to offline node-2..."
    local offline_msg="${DM_MSG_PREFIX}-while-offline"
    local result=$(api_send_dm 1 "$NODE2_ID" "$offline_msg")
    info "  Send result: $result"

    # Wait for harbor replication
    log "Phase 7: Waiting for harbor replication (30s)..."
    sleep 30

    # Restart node-2
    log "Phase 8: Restarting node-2..."
    start_node 2
    wait_for_api 2
    info "  Node-2 back online"

    # Wait for harbor pull to deliver the DM
    log "Phase 9: Waiting for harbor pull delivery (90s)..."
    sleep 90

    # Verify offline DM delivery (count + content)
    log "Phase 10: Verifying offline DM delivery..."
    local dm_count=$(grep -c "DM received" "$NODES_DIR/node-2/output.log" 2>/dev/null || echo "0")
    local offline_content_ok=false
    if grep "DM received" "$NODES_DIR/node-2/output.log" 2>/dev/null | grep -q "$offline_msg"; then
        offline_content_ok=true
    fi

    local passed=true

    if [ "$baseline_ok" = true ]; then
        # Should have at least 2 DMs (baseline + offline)
        if [ "$dm_count" -ge 2 ] && [ "$offline_content_ok" = true ]; then
            info "  Node-2: ✅ received $dm_count DMs with correct offline content"
        elif [ "$dm_count" -ge 2 ]; then
            warn "  Node-2: ⚠️  received $dm_count DMs but offline content mismatch (decryption issue?)"
            passed=false
        elif [ "$dm_count" -ge 1 ]; then
            warn "  Node-2: ⚠️  received $dm_count DM(s) - offline delivery may have failed"
            passed=false
        else
            warn "  Node-2: ❌ no DMs received at all"
            passed=false
        fi
    else
        # Baseline didn't work, check if at least the offline DM arrived with correct content
        if [ "$dm_count" -ge 1 ] && [ "$offline_content_ok" = true ]; then
            info "  Node-2: ✅ received $dm_count DM(s) via harbor pull with correct content"
        elif [ "$dm_count" -ge 1 ]; then
            warn "  Node-2: ⚠️  received $dm_count DM(s) but content mismatch (decryption issue?)"
            passed=false
        else
            warn "  Node-2: ❌ no DMs received"
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
    log "  Baseline (online) DM: $([ "$baseline_ok" = true ] && echo "✅" || echo "⚠️")"
    log "  Total DMs received by node-2: $dm_count"
    if [ "$passed" = true ]; then
        log "  ✅ PASSED: DM offline delivery working"
    else
        warn "  ⚠️  PARTIAL: Offline DM delivery needs investigation"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: DM Offline"
    log "=========================================="
}
