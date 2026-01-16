#!/bin/bash
#
# Scenario: Scale Members
#
# Tests with many nodes in a single topic (max 10 members).
#

scenario_scale_members() {
    log "=========================================="
    log "SCENARIO: Scale Members"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=10  # Max 10 topic members
    
    start_bootstrap_node
    
    log "Phase 1: Starting $((TEST_NODES - 1)) additional nodes..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
        # Small delay to avoid overwhelming the system
        if [ $((i % 5)) -eq 0 ]; then
            sleep 1
        fi
    done
    
    log "Phase 2: Waiting for DHT convergence (90s for large network)..."
    sleep 90
    
    log "Phase 3: Creating topic and joining $TOPIC_MEMBERS nodes..."
    local invite=$(api_create_topic 1)
    local topic=$(echo "$invite" | head -c 64)
    
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$invite" > /dev/null
        sleep 0.5
    done
    sleep 15
    
    log "Phase 4: Node-1 sends broadcast message..."
    api_send 1 "$topic" "Broadcast-to-$TOPIC_MEMBERS-members" > /dev/null
    
    log "Phase 5: Waiting for fanout delivery (45s)..."
    sleep 45
    
    log "Phase 6: Verifying delivery to all members..."
    local delivered=0
    for i in $(seq 2 $TOPIC_MEMBERS); do
        if grep -q "Broadcast-to-$TOPIC_MEMBERS-members" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            delivered=$((delivered + 1))
        fi
    done
    
    local expected=$((TOPIC_MEMBERS - 1))
    local success_rate=$((delivered * 100 / expected))
    log "  Delivered to: $delivered/$expected nodes ($success_rate%)"
    
    if [ $success_rate -ge 85 ]; then
        log "✅ Scale test passed!"
    else
        warn "⚠️  Delivery rate below threshold"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Scale Members"
    log "=========================================="
}

