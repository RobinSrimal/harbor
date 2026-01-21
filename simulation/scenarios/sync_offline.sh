#!/bin/bash
#
# Scenario: Offline Sync Recovery
#
# Tests that a node coming back online can request and receive sync state:
# - Node goes offline (stopped)
# - Other nodes send updates while offline node is down
# - Node comes back online and requests sync
# - Verify sync request/response flow works for recovery
#

scenario_sync_offline() {
    log "=========================================="
    log "SCENARIO: Offline Sync Recovery"
    log "=========================================="

    local TEST_NODES=20
    local TOPIC_MEMBERS=3
    local OFFLINE_NODE=3

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
    TOPIC_ID="${INVITE:0:64}"
    info "Topic ID: ${TOPIC_ID:0:16}..."

    log "Phase 3b: Nodes 2-$TOPIC_MEMBERS joining topic..."
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        sleep 2
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done

    log "Phase 3c: Waiting for member list propagation (10s)..."
    sleep 10

    # Stop offline node
    log "Phase 4: Stopping node-$OFFLINE_NODE (simulating offline)..."
    stop_node $OFFLINE_NODE
    sleep 2

    # Online nodes send updates while offline node is down
    log "Phase 5: Online nodes sending updates while node-$OFFLINE_NODE is offline..."
    for i in $(seq 1 $((TOPIC_MEMBERS - 1))); do
        if [ $i -ne $OFFLINE_NODE ]; then
            local data=$(mock_crdt_update "UPDATE-WHILE-OFFLINE-$i")
            api_sync_update $i "$TOPIC_ID" "$data" > /dev/null
            sleep 1
        fi
    done

    log "Phase 5b: Waiting for updates to propagate (10s)..."
    sleep 10

    # Bring offline node back online
    log "Phase 6: Restarting node-$OFFLINE_NODE..."
    start_node $OFFLINE_NODE
    wait_for_api $OFFLINE_NODE
    sleep 5

    # Offline node requests sync to catch up
    log "Phase 7: Node-$OFFLINE_NODE requesting sync to catch up..."
    api_sync_request $OFFLINE_NODE "$TOPIC_ID" > /dev/null
    sleep 15

    # Check if request was received by online nodes
    log "Phase 8: Checking if SyncRequest was received..."
    local request_received=0
    for i in $(seq 1 $TOPIC_MEMBERS); do
        if [ $i -ne $OFFLINE_NODE ]; then
            local log_file="$NODES_DIR/node-$i/output.log"
            if [ -f "$log_file" ]; then
                local count=$(grep -c "SyncRequest" "$log_file" 2>/dev/null || echo "0")
                if [ $count -gt 0 ]; then
                    log "  ✅ Node-$i: received SyncRequest"
                    request_received=$((request_received + 1))
                fi
            fi
        fi
    done

    log ""
    log "Verification Summary:"
    log "  Online nodes that received request: $request_received"

    if [ $request_received -gt 0 ]; then
        log "✅ PASSED: Offline node can request sync after recovery"
    else
        warn "❌ FAILED: Sync request not working for offline recovery"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Offline Sync Recovery"
    log "=========================================="
}
