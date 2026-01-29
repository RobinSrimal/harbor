#!/bin/bash
#
# Scenario: DM Basic - Direct Messaging
#
# Tests sending direct messages between two peers without a topic.
# Verification: Logs checked for "DM received" with expected sender.
#

scenario_dm_basic() {
    log "=========================================="
    log "SCENARIO: DM Basic - Direct Messaging"
    log "=========================================="

    local DM_MSG_PREFIX="DM-BASIC-TEST"

    # Start bootstrap node first
    start_bootstrap_node

    # Start a few nodes (DM only needs 2, but we need DHT infrastructure)
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

    # Get endpoint IDs for node-1 and node-2
    log "Phase 3: Getting endpoint IDs..."
    local NODE1_ID=$(api_get_endpoint_id 1)
    local NODE2_ID=$(api_get_endpoint_id 2)

    if [ -z "$NODE1_ID" ] || [ -z "$NODE2_ID" ]; then
        fail "Failed to get endpoint IDs"
    fi

    info "Node-1 endpoint: ${NODE1_ID:0:16}..."
    info "Node-2 endpoint: ${NODE2_ID:0:16}..."

    # Send DM from node-1 to node-2
    log "Phase 4: Sending DMs..."
    local msg1="${DM_MSG_PREFIX}-from-Node1"
    local result1=$(api_send_dm 1 "$NODE2_ID" "$msg1")
    info "  Node-1 → Node-2: $result1"

    sleep 2

    # Send DM from node-2 to node-1
    local msg2="${DM_MSG_PREFIX}-from-Node2"
    local result2=$(api_send_dm 2 "$NODE1_ID" "$msg2")
    info "  Node-2 → Node-1: $result2"

    # Wait for delivery
    log "Phase 5: Waiting for DM delivery (15s)..."
    sleep 15

    # Verify DM reception via logs (sender + decrypted content)
    log "Phase 6: Verifying DM delivery and content..."
    local passed=true

    # Node-2 should have received DM from Node-1 with correct content
    if grep -q "DM received" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        local sender_match=$(grep "DM received" "$NODES_DIR/node-2/output.log" | grep -c "${NODE1_ID:0:16}")
        local content_match=$(grep "DM received" "$NODES_DIR/node-2/output.log" | grep -c "$msg1")
        if [ "$sender_match" -ge 1 ] && [ "$content_match" -ge 1 ]; then
            info "  Node-2: ✅ received DM from Node-1 with correct content"
        elif [ "$sender_match" -ge 1 ]; then
            warn "  Node-2: ⚠️  received DM from Node-1 but content mismatch (decryption issue?)"
            passed=false
        else
            warn "  Node-2: ⚠️  received DM but sender mismatch"
            passed=false
        fi
    else
        warn "  Node-2: ❌ no DM received"
        passed=false
    fi

    # Node-1 should have received DM from Node-2 with correct content
    if grep -q "DM received" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        local sender_match=$(grep "DM received" "$NODES_DIR/node-1/output.log" | grep -c "${NODE2_ID:0:16}")
        local content_match=$(grep "DM received" "$NODES_DIR/node-1/output.log" | grep -c "$msg2")
        if [ "$sender_match" -ge 1 ] && [ "$content_match" -ge 1 ]; then
            info "  Node-1: ✅ received DM from Node-2 with correct content"
        elif [ "$sender_match" -ge 1 ]; then
            warn "  Node-1: ⚠️  received DM from Node-2 but content mismatch (decryption issue?)"
            passed=false
        else
            warn "  Node-1: ⚠️  received DM but sender mismatch"
            passed=false
        fi
    else
        warn "  Node-1: ❌ no DM received"
        passed=false
    fi

    # Also send DMs between more node pairs to test robustness
    log "Phase 7: Multi-pair DM test..."
    local NODE3_ID=$(api_get_endpoint_id 3)
    local NODE4_ID=$(api_get_endpoint_id 4)

    if [ -n "$NODE3_ID" ] && [ -n "$NODE4_ID" ]; then
        local msg3="${DM_MSG_PREFIX}-from-Node3"
        api_send_dm 3 "$NODE4_ID" "$msg3" > /dev/null
        sleep 2
        local msg4="${DM_MSG_PREFIX}-from-Node4"
        api_send_dm 4 "$NODE3_ID" "$msg4" > /dev/null

        sleep 15

        if grep -q "DM received" "$NODES_DIR/node-4/output.log" 2>/dev/null; then
            if grep "DM received" "$NODES_DIR/node-4/output.log" | grep -q "$msg3"; then
                info "  Node-4: ✅ received DM from Node-3 with correct content"
            else
                warn "  Node-4: ⚠️  received DM from Node-3 but content mismatch"
            fi
        else
            warn "  Node-4: ⚠️  no DM from Node-3"
        fi

        if grep -q "DM received" "$NODES_DIR/node-3/output.log" 2>/dev/null; then
            if grep "DM received" "$NODES_DIR/node-3/output.log" | grep -q "$msg4"; then
                info "  Node-3: ✅ received DM from Node-4 with correct content"
            else
                warn "  Node-3: ⚠️  received DM from Node-4 but content mismatch"
            fi
        else
            warn "  Node-3: ⚠️  no DM from Node-4"
        fi
    fi

    # Final stats
    log "Phase 8: Final statistics"
    for i in $(seq 1 4); do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$passed" = true ]; then
        log "  ✅ PASSED: DM basic messaging working"
    else
        warn "  ⚠️  PARTIAL: Some DMs not delivered"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: DM Basic"
    log "=========================================="
}
