#!/bin/bash
#
# Scenario: Concurrent Sync Updates
#
# Tests concurrent sync updates from multiple nodes:
# - Multiple nodes send sync updates simultaneously
# - All updates should be delivered to all members
# Verification: Check that all nodes receive all updates.
#

scenario_sync_concurrent() {
    log "=========================================="
    log "SCENARIO: Concurrent Sync Updates"
    log "=========================================="

    local TEST_NODES=20
    local TOPIC_MEMBERS=4
    local UPDATES_PER_NODE=3

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
        api_join_topic $i "$current_invite" > /dev/null
        sleep 2
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done

    # Wait for member list to sync
    log "Phase 3c: Waiting for member list propagation (10s)..."
    sleep 10

    # All nodes send concurrent updates
    log "Phase 4: All nodes sending concurrent updates..."
    for i in $(seq 1 $TOPIC_MEMBERS); do
        for j in $(seq 1 $UPDATES_PER_NODE); do
            local data=$(mock_crdt_update "NODE-$i-UPDATE-$j")
            api_sync_update $i "$TOPIC_ID" "$data" > /dev/null &
        done
    done
    wait

    # Wait for all updates to propagate
    log "Phase 5: Waiting for update propagation (20s)..."
    sleep 20

    # Count SyncUpdate events in logs
    log "Phase 6: Counting SyncUpdate events per node..."
    # Each node should receive updates from OTHER nodes only (not its own)
    local expected_per_node=$(((TOPIC_MEMBERS - 1) * UPDATES_PER_NODE))
    info "Expected updates per node: $expected_per_node (from other nodes)"

    local all_passed=true
    for i in $(seq 1 $TOPIC_MEMBERS); do
        local log_file="$NODES_DIR/node-$i/output.log"
        if [ -f "$log_file" ]; then
            local count=$(grep -c "SyncUpdate" "$log_file" 2>/dev/null || echo "0")
            if [ $count -eq $expected_per_node ]; then
                log "  Node-$i: ✅ received $count/$expected_per_node SyncUpdate events"
            else
                log "  Node-$i: ❌ received $count/$expected_per_node SyncUpdate events"
                all_passed=false
            fi
        else
            log "  Node-$i: ❌ log file not found"
            all_passed=false
        fi
    done

    log ""
    log "Verification Summary:"
    log "  Expected updates per node: $expected_per_node"
    log "  Each node must receive ALL updates from other nodes (100% delivery)"

    if [ "$all_passed" = true ]; then
        log "✅ PASSED: All nodes received all expected updates"
    else
        warn "❌ FAILED: Some nodes did not receive all updates"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Concurrent Sync Updates"
    log "=========================================="
}
