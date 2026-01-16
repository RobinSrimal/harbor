#!/bin/bash
#
# Scenario: Multi-Topic Isolation
#
# Tests that messages in one topic don't leak to other topics.
#

scenario_multi_topic() {
    log "=========================================="
    log "SCENARIO: Multi-Topic Isolation"
    log "=========================================="
    
    local TEST_NODES=20
    # Two topics with 5 members each (max 10 total across both, 10 non-members as Harbor nodes)
    
    start_bootstrap_node
    
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
    done
    
    log "Phase 2: Waiting for DHT convergence (30s)..."
    sleep 30
    
    log "Phase 3: Creating two separate topics..."
    # Topic A: nodes 1, 2, 3, 4, 5
    local topic_a_invite=$(api_create_topic 1)
    local topic_a=$(echo "$topic_a_invite" | head -c 64)
    log "  Topic A: ${topic_a:0:16}... (nodes 1-5)"
    
    for i in 2 3 4 5; do
        api_join_topic $i "$topic_a_invite" > /dev/null
    done
    sleep 5
    
    # Topic B: nodes 6, 7, 8, 9, 10
    local topic_b_invite=$(api_create_topic 6)
    local topic_b=$(echo "$topic_b_invite" | head -c 64)
    log "  Topic B: ${topic_b:0:16}... (nodes 6-10)"
    
    for i in 7 8 9 10; do
        api_join_topic $i "$topic_b_invite" > /dev/null
    done
    sleep 5
    
    log "Phase 4: Sending messages to each topic..."
    api_send 1 "$topic_a" "TopicA-Secret-Message" > /dev/null
    api_send 6 "$topic_b" "TopicB-Secret-Message" > /dev/null
    sleep 5
    
    log "Phase 5: Verifying topic isolation..."
    local isolated=true
    
    # Topic A members should NOT see Topic B messages
    for i in 1 2 3 4 5; do
        if grep -q "TopicB-Secret-Message" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            warn "  ❌ Node-$i (Topic A) received Topic B message!"
            isolated=false
        fi
    done
    
    # Topic B members should NOT see Topic A messages
    for i in 6 7 8 9 10; do
        if grep -q "TopicA-Secret-Message" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            warn "  ❌ Node-$i (Topic B) received Topic A message!"
            isolated=false
        fi
    done
    
    # Verify messages DID arrive at correct topics
    local a_delivered=0
    local b_delivered=0
    for i in 2 3 4 5; do
        if grep -q "TopicA-Secret-Message" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            a_delivered=$((a_delivered + 1))
        fi
    done
    for i in 7 8 9 10; do
        if grep -q "TopicB-Secret-Message" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
            b_delivered=$((b_delivered + 1))
        fi
    done
    
    log "  Topic A: $a_delivered/4 members received message"
    log "  Topic B: $b_delivered/4 members received message"
    
    if $isolated && [ $a_delivered -ge 3 ] && [ $b_delivered -ge 3 ]; then
        log "✅ Topic isolation verified!"
    else
        warn "⚠️  Topic isolation issues detected"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Multi-Topic Isolation"
    log "=========================================="
}

