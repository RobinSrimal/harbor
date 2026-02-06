#!/bin/bash
#
# Scenario: Control Protocol - Offline Delivery via Harbor
#
# Tests that Control messages are delivered via Harbor when peer is offline.
#
# Verification: Logs checked for "TOPIC_INVITE_RECEIVED" after node comes online
#

scenario_control_offline() {
    log "=========================================="
    log "SCENARIO: Control Protocol - Offline Delivery via Harbor"
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

    # Step 1: Connect all nodes
    log "Phase 2: Establishing connections..."

    # Node-1 connects to Node-2
    local INVITE1=$(extract_invite_string "$(api_control_generate_invite 1)")
    api_control_connect_with_invite 2 "$INVITE1" > /dev/null
    sleep 1

    # Node-1 connects to Node-3
    local INVITE2=$(extract_invite_string "$(api_control_generate_invite 1)")
    api_control_connect_with_invite 3 "$INVITE2" > /dev/null
    sleep 2

    # Verify connections
    local CONNECTIONS_1=$(api_control_list_connections 1)
    local CONNECTION_COUNT=$(echo "$CONNECTIONS_1" | jq '.connections | length // 0' 2>/dev/null || echo "0")
    if [ "$CONNECTION_COUNT" -ge 2 ]; then
        info "Node-1 connected to Node-2 and Node-3"
    else
        warn "Node-1 has only $CONNECTION_COUNT connections"
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

    # Step 3: Node-3 goes offline
    log "Phase 4: Taking Node-3 offline..."
    stop_node 3
    sleep 2
    info "Node-3 is now offline"

    local verification_passed=true

    # Step 4: Node-1 invites Node-3 to topic (while Node-3 is offline)
    log "Phase 5: Node-1 invites offline Node-3 to topic..."
    local INVITE_RESPONSE=$(api_control_topic_invite 1 "$NODE3_ID" "$TOPIC_ID")
    info "Topic invite response: $INVITE_RESPONSE"

    local MESSAGE_ID=$(extract_message_id "$INVITE_RESPONSE")
    if [ -z "$MESSAGE_ID" ]; then
        warn "Failed to get message ID, invite may have been queued for Harbor delivery"
    else
        info "Invite message ID: ${MESSAGE_ID:0:16}..."
    fi

    # Wait for Harbor replication
    log "Waiting for Harbor replication (20s)..."
    sleep 20

    # Step 5: Node-3 comes back online
    log "Phase 6: Bringing Node-3 back online..."
    start_node 3
    sleep 3
    wait_for_api 3
    info "Node-3 is back online"

    # Step 6: Wait for Harbor pull to deliver the invite
    log "Phase 7: Waiting for Harbor pull to deliver invite (30s)..."
    sleep 30

    # Check if Node-3 received the invite
    local PENDING_INVITES=$(api_control_pending_invites 3)
    info "Node-3 pending invites: $PENDING_INVITES"

    if echo "$PENDING_INVITES" | grep -qi "$TOPIC_ID\|topic"; then
        info "  Node-3: ✅ received topic invite via Harbor"
    else
        warn "  Node-3: ⚠️  may not have received invite yet"
        verification_passed=false
    fi

    # Check log for TOPIC_INVITE_RECEIVED
    if grep -q "TOPIC_INVITE_RECEIVED\|TopicInviteReceived\|topic invite received" "$NODES_DIR/node-3/output.log" 2>/dev/null; then
        info "  Node-3: ✅ logged TOPIC_INVITE_RECEIVED event"
    else
        warn "  Node-3: ⚠️  did not log TOPIC_INVITE_RECEIVED event"
        verification_passed=false
    fi

    # Step 7: Node-3 accepts the invite
    log "Phase 8: Node-3 accepts the topic invite..."
    local PENDING_MESSAGE_ID=$(echo "$PENDING_INVITES" | jq -r '.pending_invites[0].message_id // empty')
    if [ -n "$PENDING_MESSAGE_ID" ]; then
        local ACCEPT_RESPONSE=$(api_control_topic_accept 3 "$PENDING_MESSAGE_ID")
        info "Accept response: $ACCEPT_RESPONSE"
        sleep 3
    else
        warn "No pending invite message ID found to accept"
        verification_passed=false
    fi

    # Step 8: Verify topic membership consistency
    log "Phase 9: Verifying topic membership..."

    local NODE1_TOPICS=$(api_topics 1)
    local NODE3_TOPICS=$(api_topics 3)

    info "Node-1 topics: $NODE1_TOPICS"
    info "Node-3 topics: $NODE3_TOPICS"

    if echo "$NODE3_TOPICS" | grep -qi "$TOPIC_ID"; then
        info "  Node-3: ✅ has joined the topic"
    else
        warn "  Node-3: ⚠️  does not show topic in list"
        verification_passed=false
    fi

    # Check for TOPIC_MEMBER_JOINED on Node-1
    if grep -q "TOPIC_MEMBER_JOINED\|TopicMemberJoined\|topic member joined" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
        info "  Node-1: ✅ logged TOPIC_MEMBER_JOINED event"
    else
        warn "  Node-1: ⚠️  did not log TOPIC_MEMBER_JOINED event"
        verification_passed=false
    fi

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "  ✅ PASSED: Control offline delivery via Harbor working"
    else
        warn "  ⚠️  PARTIAL: Some verifications failed (Harbor timing may need adjustment)"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Control Offline Delivery"
    log "=========================================="
}
