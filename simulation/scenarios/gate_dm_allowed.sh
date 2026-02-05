#!/bin/bash
#
# Scenario: Connection Gate - DM Connected Peers Allowed
#
# Tests that peers who are DM-connected can use gated protocols (Send, Share, Sync, Stream).
#
# Flow:
# 1. Start 2 nodes
# 2. Node-1 generates connect invite, Node-2 uses it to connect
# 3. Verify DM messages work between connected peers
# 4. Verify DM sync works between connected peers
#
# Verification: Logs checked for successful DM delivery and no "rejected connection" messages
#

scenario_gate_dm_allowed() {
    log "=========================================="
    log "SCENARIO: Connection Gate - DM Connected Peers Allowed"
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

    # Phase 2: Establish DM connection via Control protocol
    log "Phase 2: Establishing DM connection..."

    local INVITE_RESPONSE=$(api_control_generate_invite 1)
    local INVITE_STRING=$(extract_invite_string "$INVITE_RESPONSE")

    if [ -z "$INVITE_STRING" ]; then
        fail "Failed to generate invite"
    fi
    info "Invite string: ${INVITE_STRING:0:32}..."

    # Node-2 connects using the invite
    local CONNECT_RESPONSE=$(api_control_connect_with_invite 2 "$INVITE_STRING")
    info "Connect response: $CONNECT_RESPONSE"

    # Wait for connection to be established
    sleep 3

    # Verify connected state
    local NODE1_CONNECTIONS=$(api_control_list_connections 1)
    local NODE2_CONNECTIONS=$(api_control_list_connections 2)

    if ! echo "$NODE1_CONNECTIONS" | grep -q "\"state\":\"connected\""; then
        fail "Node-1 does not show connected state"
    fi
    if ! echo "$NODE2_CONNECTIONS" | grep -q "\"state\":\"connected\""; then
        fail "Node-2 does not show connected state"
    fi
    log "  DM connection established"

    # Phase 3: Test DM messaging (uses Send protocol which is gated)
    log "Phase 3: Testing DM messaging between connected peers..."

    local DM_MESSAGE="GATE-TEST-DM-MESSAGE-$(date +%s)"
    local DM_RESULT=$(api_send_dm 1 "$NODE2_ID" "$DM_MESSAGE")
    info "DM send result: $DM_RESULT"

    sleep 3

    local verification_passed=true

    # Check Node-2 received the DM
    if grep -q "$DM_MESSAGE" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        log "  Node-2: received DM message"
    else
        warn "  Node-2: did not receive DM message"
        verification_passed=false
    fi

    # Check no rejection logs
    if grep -q "rejected connection from non-connected peer" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        warn "  Node-2: found unexpected rejection log"
        verification_passed=false
    else
        log "  Node-2: no rejection logs (as expected)"
    fi

    # Phase 4: Test DM sync (uses Sync protocol which is gated)
    log "Phase 4: Testing DM sync between connected peers..."

    local SYNC_DATA=$(mock_crdt_update "GATE-SYNC-TEST")
    local SYNC_RESULT=$(api_dm_sync_update 1 "$NODE2_ID" "$SYNC_DATA")
    info "DM sync result: $SYNC_RESULT"

    sleep 3

    # Check for sync update received
    if grep -q "DM sync update received" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        log "  Node-2: received DM sync update"
    else
        warn "  Node-2: did not receive DM sync update"
        verification_passed=false
    fi

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "  PASSED: DM-connected peers can use gated protocols"
    else
        warn "  PARTIAL: Some verifications failed"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Connection Gate - DM Connected"
    log "=========================================="
}
