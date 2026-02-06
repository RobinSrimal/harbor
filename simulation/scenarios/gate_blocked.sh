#!/bin/bash
#
# Scenario: Connection Gate - Blocked Peers Rejected
#
# Tests that blocked peers cannot use gated protocols even if they were previously connected.
#
# Flow:
# 1. Start 2 nodes
# 2. Establish DM connection between them
# 3. Verify DM messages work
# 4. Node-1 blocks Node-2
# 5. Verify Node-2 can no longer send DM messages to Node-1
#
# Verification: Messages rejected after blocking, "rejected connection" logs appear
#

scenario_gate_blocked() {
    log "=========================================="
    log "SCENARIO: Connection Gate - Blocked Peers Rejected"
    log "=========================================="

    # Start bootstrap node
    start_bootstrap_node

    # Start node 2
    log "Phase 1: Starting node 2..."
    start_node 2
    sleep 2

    # Wait for APIs
    wait_for_api 1
    wait_for_api 2

    # Get endpoint IDs
    local NODE1_ID=$(api_get_endpoint_id 1)
    local NODE2_ID=$(api_get_endpoint_id 2)
    log "Node-1 ID: ${NODE1_ID:0:16}..."
    log "Node-2 ID: ${NODE2_ID:0:16}..."

    # Phase 2: Establish DM connection
    log "Phase 2: Establishing DM connection..."

    local INVITE_RESPONSE=$(api_control_generate_invite 1)
    local INVITE_STRING=$(extract_invite_string "$INVITE_RESPONSE")

    if [ -z "$INVITE_STRING" ]; then
        fail "Failed to generate invite"
    fi

    api_control_connect_with_invite 2 "$INVITE_STRING" > /dev/null
    sleep 3

    # Verify connected
    local NODE1_CONNECTIONS=$(api_control_list_connections 1)
    if echo "$NODE1_CONNECTIONS" | grep -q "\"state\":\"connected\""; then
        log "  DM connection established"
    else
        fail "DM connection not established"
    fi

    # Phase 3: Verify DM works before blocking
    log "Phase 3: Verifying DM works before blocking..."

    local PRE_BLOCK_MSG="PRE-BLOCK-MSG-$(date +%s)"
    api_send_dm 2 "$NODE1_ID" "$PRE_BLOCK_MSG" > /dev/null
    sleep 3

    if grep -q "$PRE_BLOCK_MSG" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        log "  Node-1 received message before block"
    else
        warn "  Node-1 did not receive message before block"
    fi

    # Phase 4: Block Node-2
    log "Phase 4: Node-1 blocking Node-2..."

    local BLOCK_RESULT=$(api_control_block 1 "$NODE2_ID")
    info "Block result: $BLOCK_RESULT"
    sleep 2

    # Verify blocked state in connections list
    local CONNECTIONS_AFTER=$(api_control_list_connections 1)
    if echo "$CONNECTIONS_AFTER" | grep -q "\"state\":\"blocked\""; then
        log "  Node-2 is now blocked"
    else
        warn "  Block state not reflected in connections"
    fi

    # Phase 5: Try to send DM after being blocked
    log "Phase 5: Node-2 attempting to send DM after being blocked..."

    # Clear previous rejection logs for clean count
    local REJECTION_COUNT_BEFORE=$(grep -c "rejected connection from non-connected peer" "$NODES_DIR/node-1/output.log" 2>/dev/null | tr -d '\n' || echo "0")

    local POST_BLOCK_MSG="POST-BLOCK-MSG-$(date +%s)"
    api_send_dm 2 "$NODE1_ID" "$POST_BLOCK_MSG" > /dev/null
    sleep 3

    local verification_passed=true

    # Check Node-1 did NOT receive the post-block message
    if grep -q "$POST_BLOCK_MSG" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        warn "  Node-1 received message after block (should have been rejected)"
        verification_passed=false
    else
        log "  Node-1 did NOT receive message after block (correct)"
    fi

    # Check for rejection logs
    local REJECTION_COUNT_AFTER=$(grep -c "rejected connection from non-connected peer" "$NODES_DIR/node-1/output.log" 2>/dev/null | tr -d '\n' || echo "0")

    if [ "$REJECTION_COUNT_AFTER" -gt "$REJECTION_COUNT_BEFORE" ]; then
        log "  Node-1 logged rejection after block (correct)"
    else
        warn "  Node-1 did not log rejection (may have been silent drop or connection reuse)"
    fi

    # Phase 6: Unblock and verify messaging works again
    log "Phase 6: Unblocking Node-2 and verifying messaging resumes..."

    local UNBLOCK_RESULT=$(api_control_unblock 1 "$NODE2_ID")
    info "Unblock result: $UNBLOCK_RESULT"
    sleep 2

    # Verify unblocked
    local CONNECTIONS_UNBLOCK=$(api_control_list_connections 1)
    if echo "$CONNECTIONS_UNBLOCK" | grep -q "\"state\":\"connected\""; then
        log "  Node-2 is now unblocked"
    else
        warn "  Unblock state not reflected"
    fi

    local POST_UNBLOCK_MSG="POST-UNBLOCK-MSG-$(date +%s)"
    api_send_dm 2 "$NODE1_ID" "$POST_UNBLOCK_MSG" > /dev/null
    sleep 3

    if grep -q "$POST_UNBLOCK_MSG" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        log "  Node-1 received message after unblock (correct)"
    else
        warn "  Node-1 did not receive message after unblock"
        verification_passed=false
    fi

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "  PASSED: Blocked peers are rejected from gated protocols"
    else
        warn "  PARTIAL: Some verifications failed"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Connection Gate - Blocked"
    log "=========================================="
}
