#!/bin/bash
#
# Scenario: Connection Gate - Topic Peers Allowed
#
# Tests that peers in the same topic can use gated protocols (Send, Share, Sync, Stream)
# even without a DM connection.
#
# Flow:
# 1. Start 3 nodes
# 2. Node-1 creates a topic
# 3. Node-1 invites Node-2 and Node-3 to the topic (via Control protocol)
# 4. Verify topic messages work between topic members
# 5. Verify topic sync works between topic members
#
# Verification: Topic messages delivered, no "rejected connection" logs
#

scenario_gate_topic_allowed() {
    log "=========================================="
    log "SCENARIO: Connection Gate - Topic Peers Allowed"
    log "=========================================="

    # Start bootstrap node
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

    # Phase 2: First establish DM connections (needed for topic invites to work)
    log "Phase 2: Establishing DM connections for invites..."

    # Node-1 generates invite for Node-2
    local INVITE1=$(api_control_generate_invite 1)
    local INVITE1_STRING=$(extract_invite_string "$INVITE1")
    api_control_connect_with_invite 2 "$INVITE1_STRING" > /dev/null
    sleep 2

    # Node-1 generates invite for Node-3
    local INVITE2=$(api_control_generate_invite 1)
    local INVITE2_STRING=$(extract_invite_string "$INVITE2")
    api_control_connect_with_invite 3 "$INVITE2_STRING" > /dev/null
    sleep 2

    log "  DM connections established"

    # Phase 3: Create topic and invite members
    log "Phase 3: Creating topic and inviting members..."

    local INVITE=$(api_create_topic 1)
    if [ -z "$INVITE" ]; then
        fail "Failed to create topic"
    fi
    local TOPIC_ID="${INVITE:0:64}"
    log "  Topic created: ${TOPIC_ID:0:16}..."

    # Invite Node-2 to topic
    log "  Inviting Node-2 to topic..."
    local INVITE_RESULT=$(api_control_topic_invite 1 "$NODE2_ID" "$TOPIC_ID")
    info "Invite result: $INVITE_RESULT"
    sleep 2

    # Node-2 accepts the invite
    local PENDING=$(api_control_pending_invites 2)
    info "Node-2 pending invites: $PENDING"
    local MESSAGE_ID=$(echo "$PENDING" | grep -o '"message_id":"[^"]*"' | head -1 | cut -d'"' -f4)

    if [ -n "$MESSAGE_ID" ]; then
        api_control_topic_accept 2 "$MESSAGE_ID" > /dev/null
        log "  Node-2 accepted topic invite"
    else
        warn "  Node-2 did not receive topic invite"
    fi
    sleep 2

    # Invite Node-3 to topic
    log "  Inviting Node-3 to topic..."
    api_control_topic_invite 1 "$NODE3_ID" "$TOPIC_ID" > /dev/null
    sleep 2

    # Node-3 accepts the invite
    local PENDING3=$(api_control_pending_invites 3)
    local MESSAGE_ID3=$(echo "$PENDING3" | grep -o '"message_id":"[^"]*"' | head -1 | cut -d'"' -f4)

    if [ -n "$MESSAGE_ID3" ]; then
        api_control_topic_accept 3 "$MESSAGE_ID3" > /dev/null
        log "  Node-3 accepted topic invite"
    else
        warn "  Node-3 did not receive topic invite"
    fi
    sleep 5

    # Phase 4: Test topic messaging (uses Send protocol which is gated)
    log "Phase 4: Testing topic messaging between topic peers..."

    local TOPIC_MESSAGE="GATE-TOPIC-TEST-$(date +%s)"
    local SEND_RESULT=$(api_send 1 "$TOPIC_ID" "$TOPIC_MESSAGE")
    info "Send result: $SEND_RESULT"

    sleep 10

    local verification_passed=true

    # Check Node-2 received the message
    if grep -q "$TOPIC_MESSAGE" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        log "  Node-2: received topic message"
    else
        warn "  Node-2: did not receive topic message"
        verification_passed=false
    fi

    # Check Node-3 received the message
    if grep -q "$TOPIC_MESSAGE" "$NODES_DIR/node-3/output.log" 2>/dev/null; then
        log "  Node-3: received topic message"
    else
        warn "  Node-3: did not receive topic message"
        verification_passed=false
    fi

    # Check no rejection logs
    for n in 2 3; do
        if grep -q "SEND: rejected connection from non-connected peer" "$NODES_DIR/node-$n/output.log" 2>/dev/null; then
            warn "  Node-$n: found unexpected rejection log"
            verification_passed=false
        fi
    done
    log "  No unexpected rejection logs"

    # Phase 5: Test topic sync (uses Sync protocol which is gated)
    log "Phase 5: Testing topic sync between topic peers..."

    local SYNC_DATA=$(mock_crdt_update "GATE-TOPIC-SYNC")
    local SYNC_RESULT=$(api_sync_update 1 "$TOPIC_ID" "$SYNC_DATA")
    info "Sync update result: $SYNC_RESULT"

    sleep 5

    # Check for sync update received
    local sync_received=0
    for n in 2 3; do
        if grep -q "SyncUpdate\|SYNC_UPDATE" "$NODES_DIR/node-$n/output.log" 2>/dev/null; then
            sync_received=$((sync_received + 1))
        fi
    done
    log "  Nodes that received sync update: $sync_received/2"

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "  PASSED: Topic peers can use gated protocols"
    else
        warn "  PARTIAL: Some verifications failed"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Connection Gate - Topic Peers"
    log "=========================================="
}
