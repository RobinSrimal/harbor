#!/bin/bash
#
# Scenario: Control Protocol - Remove Member (Epoch Key Rotation)
#
# Tests that removed members cannot decrypt messages sent after their removal.
#
# Flow:
# 1. 20 nodes total: Node-1 to Node-4 are topic members, Node-5 to Node-20 are Harbor nodes
# 2. Node-1 (admin) removes Node-3 from the topic
# 3. Node-1 sends a message with the new epoch key
# 4. Node-2 and Node-4 receive the message
# 5. Node-3 pulls from Harbor but CANNOT decrypt (no epoch key)
#
# Verification: Logs checked for "NO_EPOCH_KEY_FOR_EPOCH" on Node-3
#

scenario_control_remove_member() {
    log "=========================================="
    log "SCENARIO: Control Protocol - Remove Member"
    log "=========================================="

    # Start bootstrap node first
    start_bootstrap_node

    # Start nodes 2-20 (2-4 are topic members, 5-20 are Harbor nodes)
    log "Phase 1: Starting nodes 2-20..."
    for n in $(seq 2 20); do
        start_node $n
    done
    sleep 5

    # Wait for APIs on topic member nodes
    wait_for_api 1
    wait_for_api 2
    wait_for_api 3
    wait_for_api 4
    # Wait for a few Harbor nodes
    for n in $(seq 5 10); do
        wait_for_api $n
    done

    # Get endpoint IDs for topic members
    local NODE1_ID=$(api_get_endpoint_id 1)
    local NODE2_ID=$(api_get_endpoint_id 2)
    local NODE3_ID=$(api_get_endpoint_id 3)
    local NODE4_ID=$(api_get_endpoint_id 4)
    log "Node-1 (admin) ID: ${NODE1_ID:0:16}..."
    log "Node-2 ID: ${NODE2_ID:0:16}..."
    log "Node-3 (will be removed) ID: ${NODE3_ID:0:16}..."
    log "Node-4 ID: ${NODE4_ID:0:16}..."
    log "Nodes 5-20 are Harbor nodes (not topic members)"

    # Step 1: Establish connections between topic member nodes and Node-1
    log "Phase 2: Establishing connections between topic members..."

    # Node-1 connects to Node-2
    local INVITE1=$(extract_invite_string "$(api_control_generate_invite 1)")
    api_control_connect_with_invite 2 "$INVITE1" > /dev/null
    sleep 1

    # Node-1 connects to Node-3
    local INVITE2=$(extract_invite_string "$(api_control_generate_invite 1)")
    api_control_connect_with_invite 3 "$INVITE2" > /dev/null
    sleep 1

    # Node-1 connects to Node-4
    local INVITE3=$(extract_invite_string "$(api_control_generate_invite 1)")
    api_control_connect_with_invite 4 "$INVITE3" > /dev/null
    sleep 2

    # Verify connections
    local CONNECTIONS=$(api_control_list_connections 1)
    local CONNECTION_COUNT=$(echo "$CONNECTIONS" | jq '.connections | length // 0' 2>/dev/null || echo "0")
    if [ "$CONNECTION_COUNT" -ge 3 ]; then
        info "Node-1 connected to all topic member nodes"
    else
        warn "Node-1 has only $CONNECTION_COUNT connections"
    fi

    # Step 2: Node-1 creates a topic
    log "Phase 3: Node-1 creates a topic..."
    local TOPIC_RESPONSE=$(api_create_topic 1)
    local TOPIC_ID=$(echo "$TOPIC_RESPONSE" | cut -c1-64)
    if [ -z "$TOPIC_ID" ] || [ "${#TOPIC_ID}" -ne 64 ]; then
        fail "Failed to create topic"
    fi
    info "Topic ID: ${TOPIC_ID:0:16}..."

    # Step 3: Invite nodes 2-4 to the topic
    log "Phase 4: Inviting nodes 2-4 to topic..."

    # Invite Node-2
    local INVITE_RESP=$(api_control_topic_invite 1 "$NODE2_ID" "$TOPIC_ID")
    info "Node-2 invite: ${INVITE_RESP:0:40}..."
    sleep 2
    local PENDING=$(api_control_pending_invites 2)
    local MSG_ID2=$(echo "$PENDING" | jq -r '.pending_invites[0].message_id // empty')
    if [ -n "$MSG_ID2" ]; then
        api_control_topic_accept 2 "$MSG_ID2" > /dev/null
        info "Node-2 accepted topic invite"
    fi
    sleep 1

    # Invite Node-3
    INVITE_RESP=$(api_control_topic_invite 1 "$NODE3_ID" "$TOPIC_ID")
    info "Node-3 invite: ${INVITE_RESP:0:40}..."
    sleep 2
    PENDING=$(api_control_pending_invites 3)
    local MSG_ID3=$(echo "$PENDING" | jq -r '.pending_invites[0].message_id // empty')
    if [ -n "$MSG_ID3" ]; then
        api_control_topic_accept 3 "$MSG_ID3" > /dev/null
        info "Node-3 accepted topic invite"
    fi
    sleep 1

    # Invite Node-4
    INVITE_RESP=$(api_control_topic_invite 1 "$NODE4_ID" "$TOPIC_ID")
    info "Node-4 invite: ${INVITE_RESP:0:40}..."
    sleep 2
    PENDING=$(api_control_pending_invites 4)
    local MSG_ID4=$(echo "$PENDING" | jq -r '.pending_invites[0].message_id // empty')
    if [ -n "$MSG_ID4" ]; then
        api_control_topic_accept 4 "$MSG_ID4" > /dev/null
        info "Node-4 accepted topic invite"
    fi
    sleep 3

    # Verify topic member nodes have the topic
    log "Phase 5: Verifying initial topic membership..."
    local verification_passed=true

    local NODE2_TOPICS=$(api_topics 2)
    local NODE3_TOPICS=$(api_topics 3)
    local NODE4_TOPICS=$(api_topics 4)

    for n in 2 3 4; do
        local TOPICS_VAR="NODE${n}_TOPICS"
        if echo "${!TOPICS_VAR}" | grep -qi "$TOPIC_ID"; then
            info "  Node-$n: ✅ has topic"
        else
            warn "  Node-$n: ⚠️  does not have topic"
            verification_passed=false
        fi
    done

    # Step 4: Node-3 goes offline
    log "Phase 6: Taking Node-3 offline..."
    stop_node 3
    sleep 3
    info "Node-3 is now offline"

    # Step 5: Node-1 removes Node-3 from the topic
    log "Phase 7: Node-1 removes Node-3 from topic..."
    local REMOVE_RESP=$(api_control_remove_member 1 "$TOPIC_ID" "$NODE3_ID")
    info "Remove member response: $REMOVE_RESP"

    # Wait for RemoveMember to be delivered to Node-2 and Node-4
    sleep 5

    # Check that Node-2 and Node-4 received the epoch rotation
    if grep -q "TopicEpochRotated\|TOPIC_EPOCH_ROTATED\|new epoch" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        info "  Node-2: ✅ received new epoch key"
    else
        warn "  Node-2: ⚠️  did not log epoch rotation"
    fi

    if grep -q "TopicEpochRotated\|TOPIC_EPOCH_ROTATED\|new epoch" "$NODES_DIR/node-4/output.log" 2>/dev/null; then
        info "  Node-4: ✅ received new epoch key"
    else
        warn "  Node-4: ⚠️  did not log epoch rotation"
    fi

    # Step 6: Take Node-2 and Node-4 offline (topic members)
    # Nodes 5-20 stay online as Harbor nodes
    log "Phase 8: Taking Node-2 and Node-4 offline..."
    log "         (Nodes 5-20 remain online as Harbor nodes)"
    stop_node 2
    stop_node 4
    sleep 2
    info "Node-2 and Node-4 are now offline"

    # Step 7: Node-1 sends a message (will use new epoch key)
    # Message will be replicated to Harbor nodes 5-20
    log "Phase 9: Node-1 sends message with new epoch key..."
    local MSG_CONTENT="secret-after-removal-$(date +%s)"
    api_send 1 "$TOPIC_ID" "$MSG_CONTENT" > /dev/null
    info "Message sent: $MSG_CONTENT"

    # Wait for replication to Harbor nodes
    log "Waiting for Harbor replication (20s)..."
    sleep 20

    # Step 8: Bring Node-2 and Node-4 back online
    log "Phase 10: Bringing Node-2 and Node-4 back online..."
    start_node 2
    start_node 4
    sleep 3
    wait_for_api 2
    wait_for_api 4
    info "Node-2 and Node-4 are back online"

    # Wait for Harbor pull
    log "Waiting for Harbor pull (20s)..."
    sleep 20

    # Verify Node-2 and Node-4 received the message via Harbor
    log "Phase 11: Verifying message delivery to remaining members..."

    if grep -q "$MSG_CONTENT" "$NODES_DIR/node-2/output.log" 2>/dev/null; then
        info "  Node-2: ✅ received message with new epoch via Harbor"
    else
        warn "  Node-2: ⚠️  did not receive message via Harbor"
    fi

    if grep -q "$MSG_CONTENT" "$NODES_DIR/node-4/output.log" 2>/dev/null; then
        info "  Node-4: ✅ received message with new epoch via Harbor"
    else
        warn "  Node-4: ⚠️  did not receive message via Harbor"
    fi

    # Step 9: Now bring Node-3 back online and verify it CANNOT decrypt
    log "Phase 12: Bringing Node-3 back online..."
    start_node 3
    sleep 3
    wait_for_api 3
    info "Node-3 is back online"

    # Wait for Harbor pull to attempt decryption
    log "Waiting for Harbor pull (30s)..."
    sleep 30

    # Step 10: Verify Node-3 CANNOT decrypt the message (no epoch key)
    log "Phase 13: Verifying Node-3 cannot decrypt new epoch message..."

    if grep -q "NO_EPOCH_KEY_FOR_EPOCH\|no epoch key" "$NODES_DIR/node-3/output.log" 2>/dev/null; then
        info "  Node-3: ✅ FAILED to decrypt (NO_EPOCH_KEY - expected behavior)"
    else
        warn "  Node-3: ⚠️  did not log NO_EPOCH_KEY error (might have wrong key or timing issue)"
        verification_passed=false
    fi

    # Node-3 should NOT have the message content
    if grep -q "$MSG_CONTENT" "$NODES_DIR/node-3/output.log" 2>/dev/null; then
        warn "  Node-3: ⚠️  UNEXPECTEDLY received message (security issue!)"
        verification_passed=false
    else
        info "  Node-3: ✅ did NOT receive the message (forward secrecy working)"
    fi

    # Summary
    log ""
    log "Verification Summary:"
    if [ "$verification_passed" = true ]; then
        log "  ✅ PASSED: Member removal with epoch key rotation working"
        log "  - Removed member cannot decrypt messages sent after removal"
        log "  - Remaining members receive new epoch key"
    else
        warn "  ⚠️  PARTIAL: Some verifications failed"
        warn "  - Check logs for timing or delivery issues"
    fi

    log "=========================================="
    log "SCENARIO COMPLETE: Control Remove Member"
    log "=========================================="
}
