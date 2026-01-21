#!/bin/bash
#
# Scenario: Sync Request and Response
#
# Tests sync request/response flow:
# - One node requests sync state via Send protocol (broadcast)
# - Other members respond via direct SYNC_ALPN connection
# Verification: Check that SyncRequest and SyncResponse events are delivered.
#

scenario_sync_initial() {
    log "=========================================="
    log "SCENARIO: Sync Request and Response"
    log "=========================================="

    local TEST_NODES=20
    local TOPIC_MEMBERS=3
    local TEST_DATA="FULL-STATE-DATA"

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

    # Create topic and join members
    log "Phase 3: Creating topic on node-1..."
    INVITE=$(api_create_topic 1)
    if [ -z "$INVITE" ]; then
        fail "Failed to create topic"
    fi
    TOPIC_ID="${INVITE:0:64}"
    info "Topic ID: ${TOPIC_ID:0:16}..."

    log "Phase 3b: Nodes 2-$TOPIC_MEMBERS joining topic..."
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        local result=$(api_join_topic $i "$current_invite")
        info "  Node-$i joined: $result"
        sleep 2
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done

    # Wait for member list to sync
    log "Phase 3c: Waiting for member list propagation (10s)..."
    sleep 10

    # Node 1 broadcasts a sync request
    log "Phase 4: Node-1 broadcasting sync request..."
    local result=$(api_sync_request 1 "$TOPIC_ID")
    info "Request result: $result"

    # Wait for request to propagate via Harbor
    log "Phase 5: Waiting for sync request propagation (15s)..."
    sleep 15

    # Check logs for SyncRequest events on all members
    log "Phase 6: Checking for SyncRequest events in logs..."
    local request_received=0
    for i in $(seq 1 $TOPIC_MEMBERS); do
        local log_file="$NODES_DIR/node-$i/output.log"
        if [ -f "$log_file" ]; then
            local request_count=$(grep -c "SyncRequest" "$log_file" 2>/dev/null || echo "0")
            if [ "$request_count" -gt 0 ]; then
                log "  ✅ Node-$i: received SyncRequest event(s) ($request_count)"
                request_received=$((request_received + 1))
            else
                warn "  ⚠️  Node-$i: no SyncRequest events found"
            fi
        else
            warn "  ⚠️  Node-$i: log file not found"
        fi
    done

    # Now test direct sync response (SYNC_ALPN)
    # Node 2 sends a direct response to Node 1
    log "Phase 7: Node-2 sending direct sync response to Node-1..."

    # Get Node-1's endpoint ID
    local node1_id=$(api_get_endpoint_id 1)
    info "Node-1 endpoint ID: ${node1_id:0:16}..."

    # Node 2 sends full state snapshot
    local snapshot_data=$(mock_crdt_snapshot "$TEST_DATA")
    info "Snapshot data (hex): ${snapshot_data:0:32}... (${#snapshot_data} chars)"
    local response_result=$(api_sync_respond 2 "$TOPIC_ID" "$node1_id" "$snapshot_data")
    info "Response result: $response_result"

    # Wait for direct connection
    log "Phase 8: Waiting for sync response delivery (10s)..."
    sleep 10

    # Check Node-1's logs for SyncResponse event
    log "Phase 9: Checking for SyncResponse event on Node-1..."
    local log_file="$NODES_DIR/node-1/output.log"
    if [ -f "$log_file" ]; then
        local response_count=$(grep -c "SyncResponse" "$log_file" 2>/dev/null || echo "0")
        if [ "$response_count" -gt 0 ]; then
            log "  ✅ Node-1: received SyncResponse event(s) ($response_count)"
        else
            warn "  ⚠️  Node-1: no SyncResponse events found"
        fi
    fi

    # Summary
    log ""
    log "Verification Summary:"
    log "  Nodes that received SyncRequest: $request_received/$TOPIC_MEMBERS"
    if [ "$response_count" -gt 0 ]; then
        log "  Node-1 received SyncResponse: ✅"
    else
        log "  Node-1 received SyncResponse: ❌"
    fi

    local success=0
    if [ $request_received -ge 2 ] && [ "$response_count" -gt 0 ]; then
        log "✅ PASSED: Sync request/response transport working"
        success=1
    else
        warn "❌ FAILED: Sync request/response not working properly"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Sync Request/Response"
    log "=========================================="
}
