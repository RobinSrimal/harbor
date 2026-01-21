#!/bin/bash
#
# Scenario: Basic Sync Transport
#
# Tests basic sync transport layer between topic members:
# - Sync updates (deltas) broadcast via Send protocol
# - Sync updates pulled from Harbor nodes
# Verification: Check that sync update events are delivered to all members.
#

scenario_sync_basic() {
    log "=========================================="
    log "SCENARIO: Basic Sync Transport"
    log "=========================================="

    local TEST_NODES=20
    local TOPIC_MEMBERS=3  # Keep small for sync testing
    local TEST_DATA="SYNC-UPDATE-TEST"

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
        sleep 2  # Wait for JOIN message to propagate
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done

    # Wait for member list to sync
    log "Phase 3c: Waiting for member list propagation (10s)..."
    sleep 10

    # Node 1 sends a sync update
    log "Phase 4: Node-1 sending sync update..."
    local update_data=$(mock_crdt_update "$TEST_DATA")
    info "Update data (hex): ${update_data:0:32}... (${#update_data} chars)"
    local result=$(api_sync_update 1 "$TOPIC_ID" "$update_data")
    info "Send result: $result"

    # Wait for sync propagation via Harbor
    log "Phase 5: Waiting for sync propagation (15s)..."
    sleep 15

    # Check logs for sync events on receiver nodes (not the sender)
    log "Phase 6: Checking for SyncUpdate events in logs (receivers only)..."
    local sync_received=0
    for i in $(seq 2 $TOPIC_MEMBERS); do
        local log_file="$NODES_DIR/node-$i/output.log"
        if [ -f "$log_file" ]; then
            local sync_count=$(grep -c "SyncUpdate" "$log_file" 2>/dev/null || echo "0")
            sync_count=$(echo "$sync_count" | head -1)
            if [ "$sync_count" -gt 0 ] 2>/dev/null; then
                log "  ✅ Node-$i: received SyncUpdate event(s) ($sync_count)"
                sync_received=$((sync_received + 1))
            else
                warn "  ⚠️  Node-$i: no SyncUpdate events found"
            fi
        else
            warn "  ⚠️  Node-$i: log file not found"
        fi
    done

    # Summary
    log ""
    log "Verification Summary:"
    local expected_receivers=$((TOPIC_MEMBERS - 1))  # Sender doesn't receive own update
    log "  Receiver nodes that got sync updates: $sync_received/$expected_receivers"

    if [ $sync_received -eq $expected_receivers ]; then
        log "✅ PASSED: Sync transport working (all receivers got updates)"
    elif [ $sync_received -gt 0 ]; then
        warn "⚠️  PARTIAL: Some receivers got updates ($sync_received/$expected_receivers)"
    else
        warn "❌ FAILED: Sync updates not propagating"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Basic Sync Transport"
    log "=========================================="
}
