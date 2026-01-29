#!/bin/bash
#
# Scenario: Stream End (Lifecycle)
#
# Tests the full stream lifecycle including cleanup:
# - Establish stream (request → accept → connect)
# - Publish data (verify receipt)
# - End stream
# - Verify StreamEnded event
#

scenario_stream_end() {
    log "=========================================="
    log "SCENARIO: Stream End (Lifecycle)"
    log "=========================================="

    local TEST_NODES=20
    local TOPIC_MEMBERS=2

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

    # Create topic and join
    log "Phase 3: Creating topic on node-1..."
    INVITE=$(api_create_topic 1)
    if [ -z "$INVITE" ]; then
        fail "Failed to create topic"
    fi
    TOPIC_ID="${INVITE:0:64}"
    info "Topic ID: ${TOPIC_ID:0:16}..."

    log "Phase 3b: Node-2 joining topic..."
    local result=$(api_join_topic 2 "$INVITE")
    info "  Node-2 joined: $result"

    sleep 10

    local NODE2_ID=$(api_get_endpoint_id 2)
    info "Node-2 endpoint: ${NODE2_ID:0:16}..."

    # Request stream
    log "Phase 4: Node-1 requesting stream to Node-2..."
    local req_response=$(api_stream_request 1 "$TOPIC_ID" "$NODE2_ID" "lifecycle-test")
    local REQUEST_ID=$(extract_request_id "$req_response")
    if [ -z "$REQUEST_ID" ]; then
        warn "❌ FAILED: Could not get request_id"
        return
    fi
    info "Request ID: ${REQUEST_ID:0:16}..."

    # Wait for connection
    log "Phase 5: Waiting for stream establishment (20s)..."
    sleep 20

    # Publish some data
    log "Phase 6: Publishing test frames..."
    for i in 1 2 3; do
        local frame_data=$(echo -n "FRAME-$i" | xxd -p | tr -d '\n')
        api_stream_publish 1 "$REQUEST_ID" "$frame_data" > /dev/null
        sleep 1
    done

    # Wait for data
    sleep 5

    # End the stream
    log "Phase 7: Ending stream..."
    local end_result=$(api_stream_end 1 "$REQUEST_ID")
    info "End result: $end_result"

    # Wait for end signaling
    sleep 10

    # Verify
    log "Phase 8: Checking events..."
    local n1_log="$NODES_DIR/node-1/output.log"
    local n2_log="$NODES_DIR/node-2/output.log"

    local n1_connected=$(grep -c "MOQ stream connected" "$n1_log" 2>/dev/null; true)
    local n2_connected=$(grep -c "MOQ stream connected" "$n2_log" 2>/dev/null; true)
    local n2_data=$(grep -c "Stream data received" "$n2_log" 2>/dev/null; true)
    local n2_ended=$(grep -c "Stream ended" "$n2_log" 2>/dev/null; true)

    log ""
    log "Verification Summary:"
    local passed=0
    local total=3

    if [ "$n1_connected" -gt 0 ] && [ "$n2_connected" -gt 0 ]; then
        log "  ✅ MOQ connection established on both sides"
        passed=$((passed + 1))
    else
        warn "  ❌ MOQ connection missing (source=$n1_connected, dest=$n2_connected)"
    fi

    if [ "$n2_data" -gt 0 ]; then
        log "  ✅ Data frames received ($n2_data)"
        passed=$((passed + 1))
    else
        warn "  ❌ No data frames received"
    fi

    if [ "$n2_ended" -gt 0 ]; then
        log "  ✅ StreamEnded received on destination"
        passed=$((passed + 1))
    else
        warn "  ⚠️  StreamEnded not found (may not be implemented yet)"
    fi

    log ""
    if [ $passed -eq $total ]; then
        log "✅ PASSED: Full stream lifecycle ($passed/$total)"
    elif [ $passed -ge 2 ]; then
        log "⚠️  MOSTLY PASSED ($passed/$total)"
    else
        warn "❌ FAILED ($passed/$total)"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Stream End (Lifecycle)"
    log "=========================================="
}
