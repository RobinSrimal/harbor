#!/bin/bash
#
# Scenario: Control Protocol - Peer Suggestion Flow
#
# Tests the peer introduction/suggestion flow via Control ALPN.
#
# Verification: Logs checked for "PEER_SUGGESTED" event
#

scenario_control_suggest() {
    log "=========================================="
    log "SCENARIO: Control Protocol - Peer Suggestion Flow"
    log "=========================================="

    # Start bootstrap node first
    start_bootstrap_node

    # Start nodes 2 and 3
    log "Phase 1: Starting nodes 2-3..."
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

    # Step 1: Node-1 connects to Node-2
    log "Phase 2: Node-1 connects to Node-2..."
    local INVITE1_RESPONSE=$(api_control_generate_invite 1)
    local INVITE1_STRING=$(extract_invite_string "$INVITE1_RESPONSE")
    if [ -z "$INVITE1_STRING" ]; then
        fail "Failed to generate invite for Node-2"
    fi

    local CONNECT1_RESPONSE=$(api_control_connect_with_invite 2 "$INVITE1_STRING")
    info "Node-2 connect response: $CONNECT1_RESPONSE"
    sleep 5

    # Verify connection 1-2
    local CONNECTIONS_1=$(api_control_list_connections 1)
    if echo "$CONNECTIONS_1" | grep -q "\"state\":\"connected\""; then
        info "Node-1 and Node-2 connected"
    else
        warn "Node-1 and Node-2 connection not established"
    fi

    # Step 2: Node-1 connects to Node-3
    log "Phase 3: Node-1 connects to Node-3..."
    local INVITE2_RESPONSE=$(api_control_generate_invite 1)
    local INVITE2_STRING=$(extract_invite_string "$INVITE2_RESPONSE")
    if [ -z "$INVITE2_STRING" ]; then
        fail "Failed to generate invite for Node-3"
    fi

    local CONNECT2_RESPONSE=$(api_control_connect_with_invite 3 "$INVITE2_STRING")
    info "Node-3 connect response: $CONNECT2_RESPONSE"
    sleep 5

    # Verify connection 1-3
    CONNECTIONS_1=$(api_control_list_connections 1)
    local CONNECTION_COUNT=$(echo "$CONNECTIONS_1" | jq '.connections | length // 0' 2>/dev/null || echo "0")
    info "Node-1 has $CONNECTION_COUNT connections"
    if [ "$CONNECTION_COUNT" -ge 2 ]; then
        info "Node-1 connected to both Node-2 and Node-3"
    else
        warn "Node-1 does not have 2 connections"
    fi

    # Step 3: Verify Node-2 is NOT connected to Node-3
    log "Phase 4: Verifying Node-2 is not connected to Node-3..."
    local CONNECTIONS_2=$(api_control_list_connections 2)
    if echo "$CONNECTIONS_2" | grep -q "$NODE3_ID"; then
        warn "Node-2 is already connected to Node-3 (unexpected)"
    else
        info "Node-2 is not connected to Node-3 (as expected)"
    fi

    local verification_passed=true

    # Step 4: Node-1 suggests Node-3 to Node-2
    log "Phase 5: Node-1 suggests Node-3 to Node-2..."
    local SUGGEST_RESPONSE=$(api_control_suggest 1 "$NODE2_ID" "$NODE3_ID" "You should connect with Node-3!")
    info "Suggest response: $SUGGEST_RESPONSE"

    local MESSAGE_ID=$(extract_message_id "$SUGGEST_RESPONSE")
    if [ -z "$MESSAGE_ID" ]; then
        warn "Failed to get message ID from suggest response"
    else
        info "Suggestion message ID: ${MESSAGE_ID:0:16}..."
    fi

    # Wait for suggestion to be delivered
    sleep 5

    # Step 5: Check log for PeerSuggested event
    log "Phase 6: Checking for PeerSuggested event..."

    if grep -q "PEER_SUGGESTED\|PeerSuggested\|peer suggestion received" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        info "  Node-2: ✅ received PEER_SUGGESTED event"
    else
        warn "  Node-2: ⚠️  did not log PEER_SUGGESTED event"
        verification_passed=false
    fi

    # Step 6: Node-2 can now connect to Node-3 using suggested info
    log "Phase 7: Node-2 initiates connection to suggested Node-3..."
    local INVITE3_RESPONSE=$(api_control_generate_invite 3)
    local INVITE3_STRING=$(extract_invite_string "$INVITE3_RESPONSE")
    if [ -z "$INVITE3_STRING" ]; then
        warn "Failed to generate invite from Node-3"
        verification_passed=false
    else
        local CONNECT3_RESPONSE=$(api_control_connect_with_invite 2 "$INVITE3_STRING")
        info "Node-2 -> Node-3 connect response: $CONNECT3_RESPONSE"
        sleep 5

        # Verify Node-2 now connected to Node-3
        CONNECTIONS_2=$(api_control_list_connections 2)
        local CONNECTION_COUNT_2=$(echo "$CONNECTIONS_2" | jq '.connections | length // 0' 2>/dev/null || echo "0")
        info "Node-2 now has $CONNECTION_COUNT_2 connections"

        if [ "$CONNECTION_COUNT_2" -ge 2 ]; then
            info "  Node-2: ✅ connected to both Node-1 and Node-3"
        else
            warn "  Node-2: ⚠️  does not have 2 connections"
            verification_passed=false
        fi
    fi

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "  ✅ PASSED: Control peer suggestion flow working"
    else
        warn "  ⚠️  PARTIAL: Some verifications failed"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Control Peer Suggestion Flow"
    log "=========================================="
}
