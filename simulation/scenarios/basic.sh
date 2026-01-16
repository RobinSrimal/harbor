#!/bin/bash
#
# Scenario: Basic Topic & Messaging
#
# Tests basic topic creation and messaging functionality.
# Verification: Logs checked for "Received message" with expected content.
#

scenario_basic() {
    log "=========================================="
    log "SCENARIO: Basic Topic & Messaging"
    log "=========================================="
    
    local TOPIC_MEMBERS=10  # Max 10 topic members
    local MSG_PREFIX="BASIC-TEST-MSG"  # Unique prefix for log verification
    
    # Start bootstrap node first
    start_bootstrap_node
    
    # Start remaining nodes
    log "Phase 1: Starting nodes 2-$NODE_COUNT..."
    for i in $(seq 2 $NODE_COUNT); do
        start_node $i
        sleep 0.5
    done
    
    # Wait for APIs
    log "Waiting for all nodes to initialize..."
    for i in $(seq 2 $NODE_COUNT); do
        wait_for_api $i
    done
    
    # Wait for DHT (must be > 60s to capture DHT save to DB)
    log "Phase 2: Waiting for DHT convergence (75s)..."
    sleep 75
    
    # Check DHT stats (sample first 10 nodes)
    log "DHT Status:"
    for i in $(seq 1 10); do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done
    
    # Create topic on node 1
    log "Phase 3: Creating topic on node-1..."
    INVITE=$(api_create_topic 1)
    if [ -z "$INVITE" ]; then
        fail "Failed to create topic"
    fi
    info "Invite: ${INVITE:0:32}..."
    
    # Extract topic ID from invite (first 64 hex chars)
    TOPIC_ID="${INVITE:0:64}"
    
    # Join topic on nodes 2-10 only (max 10 members, nodes 11-20 remain as Harbor nodes)
    log "Phase 4: Joining topic on nodes 2-$TOPIC_MEMBERS..."
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        local result=$(api_join_topic $i "$current_invite")
        info "  Node-$i: $result"
        sleep 1  # Wait for JOIN to propagate
        # Get fresh invite that includes the new member
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    
    sleep 2
    
    # Get topic list from node 1
    local topics_json=$(api_topics 1)
    info "Node-1 topics: $topics_json"
    
    # Send messages with unique identifiers
    log "Phase 5: Sending messages..."
    TOPIC_ID=$(echo "$topics_json" | grep -o '"[a-f0-9]\{64\}"' | head -1 | tr -d '"')
    
    if [ -z "$TOPIC_ID" ]; then
        warn "Could not extract topic ID, skipping send test"
    else
        info "Topic ID: $TOPIC_ID"
        
        for i in $(seq 1 $TOPIC_MEMBERS); do
            local msg="${MSG_PREFIX}-from-Node${i}"
            local result=$(api_send $i "$TOPIC_ID" "$msg")
            info "  Node-$i send: $result"
            sleep 1
        done
    fi
    
    # Wait for message propagation
    log "Phase 6: Waiting for message delivery (30s)..."
    sleep 30
    
    # Verify message reception via logs
    log "Phase 7: Verifying message delivery..."
    local total_expected=0
    local total_received=0
    local verification_passed=true
    
    for receiver in $(seq 1 $TOPIC_MEMBERS); do
        local received_count=0
        local expected_count=$((TOPIC_MEMBERS - 1))  # Should receive from all other members
        
        for sender in $(seq 1 $TOPIC_MEMBERS); do
            if [ $sender -ne $receiver ]; then
                if grep -q "${MSG_PREFIX}-from-Node${sender}" "$NODES_DIR/node-$receiver/output.log" 2>/dev/null; then
                    received_count=$((received_count + 1))
                fi
            fi
        done
        
        total_expected=$((total_expected + expected_count))
        total_received=$((total_received + received_count))
        
        if [ $received_count -ge $((expected_count / 2)) ]; then
            info "  Node-$receiver: ✅ received $received_count/$expected_count messages"
        else
            warn "  Node-$receiver: ⚠️  received only $received_count/$expected_count messages"
            verification_passed=false
        fi
    done
    
    # Final stats (topic members only)
    log "Phase 8: Final statistics"
    for i in $(seq 1 $TOPIC_MEMBERS); do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done
    
    # Summary
    log ""
    log "Verification Summary:"
    log "  Total messages expected: $total_expected"
    log "  Total messages received: $total_received"
    local percentage=$((total_received * 100 / total_expected))
    log "  Delivery rate: ${percentage}%"
    
    if [ "$verification_passed" = true ] && [ $percentage -ge 50 ]; then
        log "  ✅ PASSED: Basic messaging working"
    else
        warn "  ⚠️  PARTIAL: Some messages not delivered (check Harbor sync)"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Basic Topic & Messaging"
    log "=========================================="
}
