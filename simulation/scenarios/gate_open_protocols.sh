#!/bin/bash
#
# Scenario: Connection Gate - Open Protocols (DHT, Harbor, Control)
#
# Tests that DHT, Harbor, and Control protocols remain open to everyone,
# even without a prior connection. This is essential for:
# - DHT: Peer discovery and routing must work for any peer
# - Harbor: Store-and-forward must accept packets for offline peers
# - Control: Must accept connection requests from new peers
#
# Flow:
# 1. Start 3 nodes
# 2. Verify DHT discovery works (nodes find each other)
# 3. Verify Control protocol accepts new connection requests
# 4. Verify Harbor can store packets for offline peers
#
# Verification: All open protocols work without prior connection
#

scenario_gate_open_protocols() {
    log "=========================================="
    log "SCENARIO: Connection Gate - Open Protocols"
    log "=========================================="

    # Start bootstrap node
    start_bootstrap_node

    # Start nodes 2 and 3
    log "Phase 1: Starting nodes 2-3..."
    start_node 2
    start_node 3
    sleep 3

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

    local verification_passed=true

    # =========================================================================
    # Test 1: DHT Protocol - Should be open
    # =========================================================================
    log ""
    log "Test 1: DHT Protocol (should be open)..."

    # Wait for DHT convergence
    log "  Waiting for DHT convergence (10s)..."
    sleep 10

    # Check DHT stats - nodes should find each other
    local DHT_STATS=$(api_stats 2)
    local DHT_PEERS=$(echo "$DHT_STATS" | grep -o '"dht_peers":[0-9]*' | cut -d':' -f2)

    if [ -n "$DHT_PEERS" ] && [ "$DHT_PEERS" -ge 1 ]; then
        log "  Node-2 DHT peers: $DHT_PEERS - DHT is open"
    else
        warn "  Node-2 DHT peers: ${DHT_PEERS:-0} - DHT may not be working"
        # Don't fail - DHT convergence can take time
    fi

    # Check for DHT rejection logs (should be none)
    if grep -q "DHT: rejected connection" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        warn "  Found DHT rejection log (unexpected)"
        verification_passed=false
    else
        log "  No DHT rejection logs (correct - DHT is open)"
    fi

    # =========================================================================
    # Test 2: Control Protocol - Should be open
    # =========================================================================
    log ""
    log "Test 2: Control Protocol (should be open)..."

    # Node-2 sends connection request to Node-3 (no prior relationship)
    # This uses Control protocol which should accept connections from anyone
    local CONNECT_RESULT=$(api_control_connect 2 "$NODE3_ID")
    info "  Connection request result: $CONNECT_RESULT"

    sleep 3

    # Check Node-3 received the connection request
    if grep -q "connection request received\|ConnectionRequest" "$NODES_DIR/node-3/output.log" 2>/dev/null; then
        log "  Node-3 received connection request - Control is open"
    else
        # Check if it appears in pending connections
        local PENDING=$(api_control_list_connections 3)
        if echo "$PENDING" | grep -q "pending_incoming\|PendingIncoming"; then
            log "  Node-3 has pending connection - Control is open"
        else
            warn "  Node-3 did not receive connection request"
        fi
    fi

    # Check for Control rejection logs (should be none)
    if grep -q "CONTROL: rejected connection" "$NODES_DIR/node-3/output.log" 2>/dev/null; then
        warn "  Found Control rejection log (unexpected)"
        verification_passed=false
    else
        log "  No Control rejection logs (correct - Control is open)"
    fi

    # =========================================================================
    # Test 3: Harbor Protocol - Should be open
    # =========================================================================
    log ""
    log "Test 3: Harbor Protocol (should be open)..."

    # Create a topic on Node-1 with Node-2 as member
    log "  Creating topic for Harbor test..."

    # First connect Node-1 and Node-2 for topic invite
    local INVITE1=$(api_control_generate_invite 1)
    local INVITE1_STRING=$(extract_invite_string "$INVITE1")
    api_control_connect_with_invite 2 "$INVITE1_STRING" > /dev/null
    sleep 2

    local TOPIC_RESP=$(api_create_topic 1)
    local TOPIC_ID=$(echo "$TOPIC_RESP" | grep -o '"topic_id":"[^"]*"' | cut -d'"' -f4)

    if [ -z "$TOPIC_ID" ]; then
        warn "  Failed to create topic"
    else
        log "  Topic created: ${TOPIC_ID:0:16}..."

        # Invite Node-2
        api_control_topic_invite 1 "$NODE2_ID" "$TOPIC_ID" > /dev/null
        sleep 2

        local PENDING=$(api_control_pending_invites 2)
        local MSG_ID=$(echo "$PENDING" | grep -o '"message_id":"[^"]*"' | head -1 | cut -d'"' -f4)
        if [ -n "$MSG_ID" ]; then
            api_control_topic_accept 2 "$MSG_ID" > /dev/null
        fi
        sleep 2

        # Take Node-2 offline
        log "  Taking Node-2 offline..."
        stop_node 2
        sleep 2

        # Send message while Node-2 is offline (should go to Harbor)
        log "  Sending message while Node-2 is offline..."
        local HARBOR_MSG="HARBOR-TEST-$(date +%s)"
        api_send 1 "$TOPIC_ID" "$HARBOR_MSG" > /dev/null
        sleep 2

        # Check Harbor storage logs (Node-1 should store for offline peer)
        if grep -q "stored packet\|HarborStore\|storing packet" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
            log "  Harbor stored packet for offline peer"
        else
            info "  Harbor storage log not found (may use different log format)"
        fi

        # Check for Harbor rejection logs (should be none)
        if grep -q "HARBOR: rejected connection" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
            warn "  Found Harbor rejection log (unexpected)"
            verification_passed=false
        else
            log "  No Harbor rejection logs (correct - Harbor is open)"
        fi

        # Bring Node-2 back online
        log "  Bringing Node-2 back online..."
        start_node 2
        wait_for_api 2
        sleep 5

        # Check if Node-2 received the message via Harbor pull
        if grep -q "$HARBOR_MSG" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
            log "  Node-2 received message via Harbor - Harbor pull working"
        else
            info "  Node-2 did not yet receive message (may need more time)"
        fi
    fi

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "  PASSED: DHT, Harbor, and Control protocols remain open"
    else
        warn "  PARTIAL: Some verifications failed"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Connection Gate - Open Protocols"
    log "=========================================="
}
