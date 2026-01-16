#!/bin/bash
#
# Scenario: Member Join Flow
#
# Tests the member join flow and message delivery to new members.
# Verification: Logs checked for JOIN events and message delivery.
#

scenario_member_join() {
    log "=========================================="
    log "SCENARIO: Member Join Flow"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=10  # Max 10 topic members
    local MSG_PREFIX="JOIN-FLOW-MSG"  # Unique prefix for verification
    
    # Start bootstrap node
    start_bootstrap_node
    
    # Start all other nodes
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
        sleep 0.3
    done
    for i in $(seq 2 $TEST_NODES); do
        wait_for_api $i
    done
    
    # Wait for DHT
    log "Phase 2: DHT convergence (75s)..."
    sleep 75
    
    # Node-1 creates topic, becomes admin
    log "Phase 3: Node-1 creates topic..."
    INVITE=$(api_create_topic 1)
    TOPIC_ID="${INVITE:0:64}"
    info "Topic created: ${TOPIC_ID:0:16}..."
    
    # Only nodes 1-10 join (max 10 members, nodes 11-20 remain as Harbor nodes)
    log "Phase 4: Nodes 2-$TOPIC_MEMBERS joining via invite..."
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        info "  Node-$i joined"
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 2
    
    # Verify topic members see the topic
    log "Phase 5: Verifying member lists..."
    for i in $(seq 1 $TOPIC_MEMBERS); do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done
    
    # Check for join announcements in logs
    log "Phase 5b: Verifying JOIN announcements..."
    local join_count=0
    for i in $(seq 2 $TOPIC_MEMBERS); do
        # Check if node-1 saw the join from node-i
        if grep -q "member joined" "$NODES_DIR/node-1/output.log" 2>/dev/null || \
           grep -q "joiner.*$i" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
            join_count=$((join_count + 1))
        fi
    done
    info "  Node-1 recorded $join_count join events"
    
    # Multiple nodes send messages
    log "Phase 6: Multiple nodes sending messages..."
    local senders="2 5 7 10"
    for sender in $senders; do
        api_send $sender "$TOPIC_ID" "${MSG_PREFIX}-from-Node${sender}" > /dev/null
        info "  Node-$sender sent message"
        sleep 1
    done
    
    # Wait for propagation
    sleep 5
    
    # Check if Node-1 received messages from multiple senders
    log "Phase 7: Verifying message delivery to Node-1..."
    local received=0
    local expected=4
    for sender in $senders; do
        if grep -q "${MSG_PREFIX}-from-Node${sender}" "$NODES_DIR/node-1/output.log" 2>/dev/null; then
            log "  ✅ Node-1 received message from Node-$sender"
            received=$((received + 1))
        else
            warn "  ⚠️  Message from Node-$sender not found in Node-1 logs"
        fi
    done
    
    # Also check other nodes received messages
    log "Phase 7b: Verifying message delivery to other members..."
    local other_received=0
    for receiver in 3 6 9; do
        local recv_count=0
        for sender in $senders; do
            if [ $sender -ne $receiver ]; then
                if grep -q "${MSG_PREFIX}-from-Node${sender}" "$NODES_DIR/node-$receiver/output.log" 2>/dev/null; then
                    recv_count=$((recv_count + 1))
                fi
            fi
        done
        info "  Node-$receiver received $recv_count messages"
        other_received=$((other_received + recv_count))
    done
    
    # Summary
    log ""
    log "Verification Summary:"
    log "  Node-1 received: $received/$expected messages"
    log "  Other nodes verified: $other_received messages"
    
    if [ $received -ge 3 ]; then
        log "  ✅ PASSED: Member join flow working"
    else
        warn "  ⚠️  PARTIAL: Only $received/$expected messages received"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Member Join Flow"
    log "=========================================="
}
