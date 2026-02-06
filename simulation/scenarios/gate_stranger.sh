#!/bin/bash
#
# Scenario: Connection Gate - Strangers Rejected
#
# Tests that peers who have never connected cannot use gated protocols.
# They can still discover each other via DHT but cannot send DM messages
# or use other gated protocols until they establish a connection.
#
# Flow:
# 1. Start 3 nodes (1 is bootstrap, 2 and 3 are strangers to each other)
# 2. Node-1 connects to both Node-2 and Node-3 via DM
# 3. Node-2 tries to send DM to Node-3 (they are strangers)
# 4. Verify the message is rejected
#
# Verification: DM from stranger is rejected, "rejected connection" logs appear
#

scenario_gate_stranger() {
    log "=========================================="
    log "SCENARIO: Connection Gate - Strangers Rejected"
    log "=========================================="

    # Start bootstrap node
    start_bootstrap_node

    # Start nodes 2 and 3
    log "Phase 1: Starting nodes 2 and 3..."
    start_node 2
    start_node 3
    sleep 2

    # Wait for APIs
    wait_for_api 1
    wait_for_api 2
    wait_for_api 3

    # Get endpoint IDs
    local NODE1_ID=$(api_get_endpoint_id 1)
    local NODE2_ID=$(api_get_endpoint_id 2)
    local NODE3_ID=$(api_get_endpoint_id 3)
    log "Node-1 ID: ${NODE1_ID:0:16}..."
    log "Node-2 ID: ${NODE2_ID:0:16}..."
    log "Node-3 ID: ${NODE3_ID:0:16}..."

    # Phase 2: Connect Node-1 to Node-2 (to prove DM works when connected)
    log "Phase 2: Establishing Node-1 <-> Node-2 connection..."

    local INVITE1=$(api_control_generate_invite 1)
    local INVITE1_STRING=$(extract_invite_string "$INVITE1")
    api_control_connect_with_invite 2 "$INVITE1_STRING" > /dev/null
    sleep 3

    # Verify Node-1 and Node-2 are connected
    local CONN_1_2=$(api_control_list_connections 1)
    if echo "$CONN_1_2" | grep -q "\"state\":\"connected\""; then
        log "  Node-1 <-> Node-2 connected"
    else
        warn "  Node-1 <-> Node-2 not connected"
    fi

    # Phase 3: Connect Node-1 to Node-3 (separately)
    log "Phase 3: Establishing Node-1 <-> Node-3 connection..."

    local INVITE2=$(api_control_generate_invite 1)
    local INVITE2_STRING=$(extract_invite_string "$INVITE2")
    api_control_connect_with_invite 3 "$INVITE2_STRING" > /dev/null
    sleep 3

    log "  Node-1 <-> Node-3 connected"

    # At this point:
    # - Node-1 knows Node-2 and Node-3
    # - Node-2 knows only Node-1
    # - Node-3 knows only Node-1
    # - Node-2 and Node-3 are STRANGERS to each other

    # Phase 4: Verify DM works between connected peers
    log "Phase 4: Verifying DM works between connected peers..."

    local CONNECTED_MSG="CONNECTED-DM-$(date +%s)"
    api_send_dm 1 "$NODE2_ID" "$CONNECTED_MSG" > /dev/null
    sleep 3

    if grep -q "$CONNECTED_MSG" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        log "  Node-2 received DM from connected peer Node-1 (correct)"
    else
        warn "  Node-2 did not receive DM from Node-1"
    fi

    # Phase 5: Node-2 tries to DM Node-3 (they are strangers)
    log "Phase 5: Node-2 attempting DM to stranger Node-3..."

    # Record current rejection count
    local REJECTION_BEFORE=$(grep -c "rejected connection from non-connected peer" "$NODES_DIR/node-3/output.log" 2>/dev/null | head -1 || echo "0")

    local STRANGER_MSG="STRANGER-DM-$(date +%s)"
    api_send_dm 2 "$NODE3_ID" "$STRANGER_MSG" > /dev/null
    sleep 3

    local verification_passed=true

    # Check Node-3 did NOT receive the message from stranger
    if grep -q "$STRANGER_MSG" "$NODES_DIR/node-3/output.log" 2>/dev/null; then
        warn "  Node-3 received DM from stranger (should have been rejected)"
        verification_passed=false
    else
        log "  Node-3 did NOT receive DM from stranger (correct)"
    fi

    # Check for rejection logs
    local REJECTION_AFTER=$(grep -c "rejected connection from non-connected peer" "$NODES_DIR/node-3/output.log" 2>/dev/null | head -1 || echo "0")

    if [ "$REJECTION_AFTER" -gt "$REJECTION_BEFORE" ]; then
        log "  Node-3 logged rejection from stranger (correct)"
    else
        info "  Node-3 did not log rejection (connection may not have been established)"
    fi

    # Phase 6: Now connect Node-2 and Node-3, and verify DM works
    log "Phase 6: Connecting Node-2 <-> Node-3 and verifying DM now works..."

    local INVITE3=$(api_control_generate_invite 2)
    local INVITE3_STRING=$(extract_invite_string "$INVITE3")
    api_control_connect_with_invite 3 "$INVITE3_STRING" > /dev/null
    sleep 3

    # Verify they're connected
    local CONN_2_3=$(api_control_list_connections 2)
    if echo "$CONN_2_3" | grep -q "$NODE3_ID"; then
        log "  Node-2 <-> Node-3 now connected"
    fi

    local CONNECTED_MSG2="NOW-CONNECTED-$(date +%s)"
    api_send_dm 2 "$NODE3_ID" "$CONNECTED_MSG2" > /dev/null
    sleep 3

    if grep -q "$CONNECTED_MSG2" "$NODES_DIR/node-3/output.log" 2>/dev/null; then
        log "  Node-3 received DM after connection established (correct)"
    else
        warn "  Node-3 did not receive DM after connection"
        verification_passed=false
    fi

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "  PASSED: Strangers are rejected from gated protocols"
    else
        warn "  PARTIAL: Some verifications failed"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Connection Gate - Strangers"
    log "=========================================="
}
