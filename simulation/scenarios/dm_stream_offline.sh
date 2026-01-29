#!/bin/bash
#
# Scenario: DM Stream Offline - Stream request via Harbor for offline peers
#
# Tests that DM stream requests are stored on Harbor nodes and delivered
# when the recipient comes back online (via harbor pull + liveness check).
#
# Flow:
# 1. Start all nodes, wait for DHT
# 2. Get endpoint IDs
# 3. Stop node-2 (goes offline)
# 4. Node-1 sends DM stream request to node-2 (stored on Harbor)
# 5. Restart node-2
# 6. Wait for harbor pull to deliver the stream request
# 7. Node-2 sends liveness query, node-1 responds with StreamActive
# 8. Node-2 auto-accepts, MOQ connection established
# 9. Verify signaling + MOQ connection
#

scenario_dm_stream_offline() {
    log "=========================================="
    log "SCENARIO: DM Stream Offline - Harbor Delivery"
    log "=========================================="

    local TEST_NODES=20

    # Start bootstrap node
    start_bootstrap_node

    # Start other nodes
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

    # Get endpoint IDs while both nodes are online
    log "Phase 3: Getting endpoint IDs..."
    local NODE1_ID=$(api_get_endpoint_id 1)
    local NODE2_ID=$(api_get_endpoint_id 2)

    if [ -z "$NODE1_ID" ] || [ -z "$NODE2_ID" ]; then
        fail "Failed to get endpoint IDs"
    fi

    info "Node-1 endpoint: ${NODE1_ID:0:16}..."
    info "Node-2 endpoint: ${NODE2_ID:0:16}..."

    # Stop node-2 (goes offline)
    log "Phase 4: Stopping node-2 (going offline)..."
    stop_node 2
    sleep 5

    # Node-1 requests DM stream to offline node-2
    log "Phase 5: Node-1 requesting DM stream to offline node-2..."
    local req_response=$(api_dm_stream_request 1 "$NODE2_ID" "offline-dm-stream")
    info "Request response: $req_response"
    local REQUEST_ID=$(extract_request_id "$req_response")
    if [ -z "$REQUEST_ID" ]; then
        warn "FAILED: Could not get request_id from response"
        return
    fi
    info "Request ID: ${REQUEST_ID:0:16}..."

    # Wait for harbor replication
    log "Phase 6: Waiting for harbor replication (30s)..."
    sleep 30

    # Restart node-2
    log "Phase 7: Restarting node-2..."
    start_node 2
    wait_for_api 2
    info "  Node-2 back online"

    # Wait for harbor pull + liveness check + MOQ connection
    log "Phase 8: Waiting for harbor pull + liveness + MOQ (120s)..."
    sleep 120

    # Check signaling events
    log "Phase 9: Checking events..."
    local n1_log="$NODES_DIR/node-1/output.log"
    local n2_log="$NODES_DIR/node-2/output.log"

    local n2_request=$(grep -c "Stream request received" "$n2_log" 2>/dev/null; true)
    local n2_accept=$(grep -c "Stream auto-accepted" "$n2_log" 2>/dev/null; true)
    local n1_accepted=$(grep -c "Stream accepted" "$n1_log" 2>/dev/null; true)
    local n1_connected=$(grep -c "MOQ stream connected" "$n1_log" 2>/dev/null; true)
    local n2_connected=$(grep -c "MOQ stream connected" "$n2_log" 2>/dev/null; true)

    # Liveness check: node-2 should have queried, node-1 should have responded
    local n2_query=$(grep -c "stream liveness query" "$n2_log" 2>/dev/null; true)
    local n1_active=$(grep -c "StreamActive\|stream.*active" "$n1_log" 2>/dev/null; true)

    log "  Node-2 StreamRequest events: $n2_request"
    log "  Node-2 auto-accepts: $n2_accept"
    log "  Node-1 StreamAccepted events: $n1_accepted"
    log "  Node-1 StreamConnected: $n1_connected"
    log "  Node-2 StreamConnected: $n2_connected"

    # Publish test data if connected
    if [ "$n1_connected" -gt 0 ] && [ "$n2_connected" -gt 0 ]; then
        log "Phase 10: Publishing test data..."
        local test_data=$(echo -n "HELLO-OFFLINE-DM-STREAM" | xxd -p | tr -d '\n')
        local pub_result=$(api_stream_publish 1 "$REQUEST_ID" "$test_data")
        info "Publish result: $pub_result"

        sleep 10

        local n2_data=$(grep -c "Stream data received" "$n2_log" 2>/dev/null; true)
        log "  Node-2 data received events: $n2_data"
    fi

    # Summary
    log ""
    log "Verification Summary:"
    local passed=0
    local total=4

    if [ "$n2_request" -gt 0 ]; then
        log "  ✅ Harbor Pull: DM StreamRequest delivered to offline destination"
        passed=$((passed + 1))
    else
        warn "  ❌ Harbor Pull: DM StreamRequest NOT received by destination"
    fi

    if [ "$n1_accepted" -gt 0 ]; then
        log "  ✅ Signaling: StreamAccepted delivered to source"
        passed=$((passed + 1))
    else
        warn "  ❌ Signaling: StreamAccepted NOT received by source"
    fi

    if [ "$n1_connected" -gt 0 ] && [ "$n2_connected" -gt 0 ]; then
        log "  ✅ MOQ: StreamConnected on both sides"
        passed=$((passed + 1))
    else
        warn "  ❌ MOQ: StreamConnected missing (source=$n1_connected, dest=$n2_connected)"
    fi

    local n2_data=${n2_data:-0}
    if [ "$n2_data" -gt 0 ]; then
        log "  ✅ Transport: Data received on destination after reconnect"
        passed=$((passed + 1))
    else
        if [ "$n1_connected" -gt 0 ] && [ "$n2_connected" -gt 0 ]; then
            warn "  ❌ Transport: No data received on destination"
        else
            warn "  ❌ Transport: Skipped (no MOQ connection)"
        fi
    fi

    log ""
    if [ $passed -eq $total ]; then
        log "✅ PASSED: DM stream offline pipeline working ($passed/$total)"
    elif [ $passed -gt 0 ]; then
        warn "⚠️  PARTIAL: Some checks passed ($passed/$total)"
    else
        warn "❌ FAILED: DM stream offline pipeline not working ($passed/$total)"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: DM Stream Offline"
    log "=========================================="
}
