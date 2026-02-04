#!/bin/bash
#
# Scenario: Control Protocol - Connection Flow
#
# Tests the peer connection request/accept flow via Control ALPN.
#
# Verification: Logs checked for "connection auto-accepted via token" and "CONNECTION_ACCEPTED"
#

scenario_control_connect() {
    log "=========================================="
    log "SCENARIO: Control Protocol - Connection Flow"
    log "=========================================="

    # Start bootstrap node first
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

    # Step 1: Node-1 generates a connect invite
    log "Phase 2: Node-1 generates connect invite..."
    local INVITE_RESPONSE=$(api_control_generate_invite 1)
    info "Invite response: $INVITE_RESPONSE"

    local INVITE_STRING=$(extract_invite_string "$INVITE_RESPONSE")
    if [ -z "$INVITE_STRING" ]; then
        fail "Failed to generate invite"
    fi
    info "Invite string: ${INVITE_STRING:0:32}..."

    # Step 2: Node-2 connects using the invite
    log "Phase 3: Node-2 connects with invite..."
    local CONNECT_RESPONSE=$(api_control_connect_with_invite 2 "$INVITE_STRING")
    info "Connect response: $CONNECT_RESPONSE"

    local REQUEST_ID=$(extract_request_id "$CONNECT_RESPONSE")
    if [ -z "$REQUEST_ID" ]; then
        warn "Failed to extract request ID"
    else
        info "Request ID: ${REQUEST_ID:0:16}..."
    fi

    # Wait for connection to be established
    sleep 3

    # Step 3: Verify both nodes show connected state
    log "Phase 4: Verifying connection state..."

    local NODE1_CONNECTIONS=$(api_control_list_connections 1)
    info "Node-1 connections: $NODE1_CONNECTIONS"

    local NODE2_CONNECTIONS=$(api_control_list_connections 2)
    info "Node-2 connections: $NODE2_CONNECTIONS"

    local verification_passed=true

    # Check Node-1 sees Node-2 as connected
    if echo "$NODE1_CONNECTIONS" | grep -q "\"state\":\"connected\""; then
        info "  Node-1: ✅ shows connected state"
    else
        warn "  Node-1: ⚠️  does not show connected state"
        verification_passed=false
    fi

    # Check Node-2 sees Node-1 as connected
    if echo "$NODE2_CONNECTIONS" | grep -q "\"state\":\"connected\""; then
        info "  Node-2: ✅ shows connected state"
    else
        warn "  Node-2: ⚠️  does not show connected state"
        verification_passed=false
    fi

    # Step 4: Verify log events
    log "Phase 5: Checking log events..."

    if grep -q "connection auto-accepted via token" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        info "  Node-1: ✅ auto-accepted connection via token"
    else
        warn "  Node-1: ⚠️  did not log auto-accept event"
        verification_passed=false
    fi

    if grep -q "CONNECTION_ACCEPTED\|ConnectionAccepted" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        info "  Node-2: ✅ received CONNECTION_ACCEPTED event"
    else
        warn "  Node-2: ⚠️  did not log CONNECTION_ACCEPTED event"
        verification_passed=false
    fi

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "  ✅ PASSED: Control connection flow working"
    else
        warn "  ⚠️  PARTIAL: Some verifications failed"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Control Connection Flow"
    log "=========================================="
}
