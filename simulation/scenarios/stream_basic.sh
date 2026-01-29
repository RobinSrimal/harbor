#!/bin/bash
#
# Scenario: Basic Stream Transport
#
# Tests the full stream pipeline:
# - Stream request (signaling via Send protocol)
# - Auto-accept on destination
# - MOQ connection establishment (StreamConnected on both sides)
# - Data transport via MOQ (publish → consume)
# Verification: Check logs for signaling events and data receipt.
#

scenario_stream_basic() {
    log "=========================================="
    log "SCENARIO: Basic Stream Transport"
    log "=========================================="

    local TEST_NODES=20
    local TOPIC_MEMBERS=2  # Source + destination

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

    # Wait for member list propagation
    log "Phase 3c: Waiting for member list propagation (10s)..."
    sleep 10

    # Get endpoint IDs
    local NODE2_ID=$(api_get_endpoint_id 2)
    info "Node-2 endpoint: ${NODE2_ID:0:16}..."

    # Node-1 requests stream to Node-2
    log "Phase 4: Node-1 requesting stream to Node-2..."
    local req_response=$(api_stream_request 1 "$TOPIC_ID" "$NODE2_ID" "test-stream")
    info "Request response: $req_response"
    local REQUEST_ID=$(extract_request_id "$req_response")
    if [ -z "$REQUEST_ID" ]; then
        warn "❌ FAILED: Could not get request_id from response"
        return
    fi
    info "Request ID: ${REQUEST_ID:0:16}..."

    # Wait for signaling + MOQ connection
    log "Phase 5: Waiting for stream establishment (20s)..."
    sleep 20

    # Check signaling events
    log "Phase 6a: Checking signaling events..."
    local n1_log="$NODES_DIR/node-1/output.log"
    local n2_log="$NODES_DIR/node-2/output.log"

    local n2_request=$(grep -c "Stream request received" "$n2_log" 2>/dev/null; true)
    local n2_accept=$(grep -c "Stream auto-accepted" "$n2_log" 2>/dev/null; true)
    local n1_accepted=$(grep -c "Stream accepted" "$n1_log" 2>/dev/null; true)

    log "  Node-2 StreamRequest events: $n2_request"
    log "  Node-2 auto-accepts: $n2_accept"
    log "  Node-1 StreamAccepted events: $n1_accepted"

    # Check MOQ connection events
    log "Phase 6b: Checking MOQ connection events..."
    local n1_connected=$(grep -c "MOQ stream connected" "$n1_log" 2>/dev/null; true)
    local n2_connected=$(grep -c "MOQ stream connected" "$n2_log" 2>/dev/null; true)

    log "  Node-1 StreamConnected: $n1_connected"
    log "  Node-2 StreamConnected: $n2_connected"

    # Publish test data
    log "Phase 7: Node-1 publishing test data..."
    local test_data=$(echo -n "HELLO-STREAM" | xxd -p | tr -d '\n')
    local pub_result=$(api_stream_publish 1 "$REQUEST_ID" "$test_data")
    info "Publish result: $pub_result"

    # Wait for data to arrive
    log "Phase 8: Waiting for data transport (10s)..."
    sleep 10

    # Check for received data
    log "Phase 9: Checking data receipt on Node-2..."
    local n2_data=$(grep -c "Stream data received" "$n2_log" 2>/dev/null; true)
    log "  Node-2 data received events: $n2_data"

    # Summary
    log ""
    log "Verification Summary:"
    local passed=0
    local total=4

    if [ "$n2_request" -gt 0 ]; then
        log "  ✅ Signaling: StreamRequest delivered to destination"
        passed=$((passed + 1))
    else
        warn "  ❌ Signaling: StreamRequest NOT received by destination"
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

    if [ "$n2_data" -gt 0 ]; then
        log "  ✅ Transport: Data received on destination"
        passed=$((passed + 1))
    else
        warn "  ❌ Transport: No data received on destination"
    fi

    log ""
    if [ $passed -eq $total ]; then
        log "✅ PASSED: Full stream pipeline working ($passed/$total)"
    elif [ $passed -gt 0 ]; then
        warn "⚠️  PARTIAL: Some checks passed ($passed/$total)"
    else
        warn "❌ FAILED: Stream pipeline not working ($passed/$total)"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Basic Stream Transport"
    log "=========================================="
}
