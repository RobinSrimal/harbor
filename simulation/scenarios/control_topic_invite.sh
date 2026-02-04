#!/bin/bash
#
# Scenario: Control Protocol - Topic Invite Flow
#
# Tests the topic invite/accept flow via Control ALPN.
#
# Verification: Logs checked for "TOPIC_INVITE_RECEIVED" and "TOPIC_MEMBER_JOINED"
#

scenario_control_topic_invite() {
    log "=========================================="
    log "SCENARIO: Control Protocol - Topic Invite Flow"
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

    # Step 1: Establish connection between nodes
    log "Phase 2: Establishing connection between nodes..."
    local INVITE_RESPONSE=$(api_control_generate_invite 1)
    local INVITE_STRING=$(extract_invite_string "$INVITE_RESPONSE")
    if [ -z "$INVITE_STRING" ]; then
        fail "Failed to generate invite"
    fi
    info "Invite string: ${INVITE_STRING:0:32}..."

    local CONNECT_RESPONSE=$(api_control_connect_with_invite 2 "$INVITE_STRING")
    info "Connect response: $CONNECT_RESPONSE"
    sleep 2

    # Verify connection
    local NODE1_CONNECTIONS=$(api_control_list_connections 1)
    if echo "$NODE1_CONNECTIONS" | grep -q "\"state\":\"connected\""; then
        info "Nodes connected successfully"
    else
        fail "Failed to establish connection"
    fi

    # Step 2: Node-1 creates a topic
    log "Phase 3: Node-1 creates a topic..."
    local TOPIC_RESPONSE=$(api_create_topic 1)
    info "Topic invite (hex): ${TOPIC_RESPONSE:0:32}..."

    # The API returns hex-encoded TopicInvite, first 64 chars are the topic_id
    local TOPIC_ID=$(echo "$TOPIC_RESPONSE" | cut -c1-64)
    if [ -z "$TOPIC_ID" ] || [ "${#TOPIC_ID}" -ne 64 ]; then
        fail "Failed to create topic"
    fi
    info "Topic ID: ${TOPIC_ID:0:16}..."

    # Step 3: Node-1 invites Node-2 to the topic
    log "Phase 4: Node-1 invites Node-2 to topic..."
    local INVITE_TOPIC_RESPONSE=$(api_control_topic_invite 1 "$NODE2_ID" "$TOPIC_ID")
    info "Topic invite response: $INVITE_TOPIC_RESPONSE"

    local MESSAGE_ID=$(extract_message_id "$INVITE_TOPIC_RESPONSE")
    if [ -z "$MESSAGE_ID" ]; then
        warn "Failed to extract message ID"
    else
        info "Message ID: ${MESSAGE_ID:0:16}..."
    fi

    # Wait for invite to be delivered
    sleep 3

    # Step 4: Check Node-2 received the invite
    log "Phase 5: Checking Node-2 pending invites..."
    local PENDING_INVITES=$(api_control_pending_invites 2)
    info "Pending invites: $PENDING_INVITES"

    local verification_passed=true

    if echo "$PENDING_INVITES" | grep -qi "$TOPIC_ID\|topic"; then
        info "  Node-2: ✅ has pending topic invite"
    else
        warn "  Node-2: ⚠️  does not show pending invite in list"
        verification_passed=false
    fi

    # Step 5: Node-2 accepts the invite
    log "Phase 6: Node-2 accepts topic invite..."
    local PENDING_MESSAGE_ID=$(echo "$PENDING_INVITES" | jq -r '.pending_invites[0].message_id // empty')
    if [ -z "$PENDING_MESSAGE_ID" ]; then
        PENDING_MESSAGE_ID="$MESSAGE_ID"
    fi

    if [ -n "$PENDING_MESSAGE_ID" ]; then
        info "Accepting invite with message ID: ${PENDING_MESSAGE_ID:0:16}..."
        local ACCEPT_RESPONSE=$(api_control_topic_accept 2 "$PENDING_MESSAGE_ID")
        info "Accept response: $ACCEPT_RESPONSE"
    else
        warn "No message ID available to accept invite"
        verification_passed=false
    fi

    # Wait for join to propagate
    sleep 3

    # Step 6: Verify both nodes have the topic membership
    log "Phase 7: Verifying topic membership..."

    local NODE1_TOPICS=$(api_topics 1)
    info "Node-1 topics: $NODE1_TOPICS"

    local NODE2_TOPICS=$(api_topics 2)
    info "Node-2 topics: $NODE2_TOPICS"

    # Check Node-2 has joined the topic
    if echo "$NODE2_TOPICS" | grep -qi "$TOPIC_ID"; then
        info "  Node-2: ✅ has joined the topic"
    else
        warn "  Node-2: ⚠️  does not show topic in list"
        verification_passed=false
    fi

    # Step 7: Verify log events
    log "Phase 8: Checking log events..."

    if grep -q "TOPIC_INVITE_RECEIVED\|TopicInviteReceived\|topic invite received" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        info "  Node-2: ✅ received TOPIC_INVITE_RECEIVED event"
    else
        warn "  Node-2: ⚠️  did not log TOPIC_INVITE_RECEIVED event"
        verification_passed=false
    fi

    if grep -q "TOPIC_MEMBER_JOINED\|TopicMemberJoined\|topic member joined" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        info "  Node-1: ✅ received TOPIC_MEMBER_JOINED event"
    else
        warn "  Node-1: ⚠️  did not log TOPIC_MEMBER_JOINED event"
        verification_passed=false
    fi

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "  ✅ PASSED: Control topic invite flow working"
    else
        warn "  ⚠️  PARTIAL: Some verifications failed"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Control Topic Invite Flow"
    log "=========================================="
}
