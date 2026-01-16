#!/bin/bash
#
# Scenario: Large Messages
#
# Tests messages at or near size limits (1KB, 10KB, 100KB).
#

scenario_large_message() {
    log "=========================================="
    log "SCENARIO: Large Messages"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=5  # Small topic for this test
    
    start_bootstrap_node
    
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
    done
    
    log "Phase 2: Waiting for DHT convergence (30s)..."
    sleep 30
    
    log "Phase 3: Creating topic and joining nodes 1-$TOPIC_MEMBERS..."
    local invite=$(api_create_topic 1)
    local topic=$(echo "$invite" | head -c 64)
    
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$invite" > /dev/null
    done
    sleep 5
    
    log "Phase 4: Sending large messages..."
    
    # Test different sizes: 1KB, 10KB, 100KB
    local sizes=(1024 10240 102400)
    local size_names=("1KB" "10KB" "100KB")
    
    for idx in 0 1 2; do
        local size=${sizes[$idx]}
        local name=${size_names[$idx]}
        
        # Generate message of specified size (using base64 for safe chars)
        local large_msg="LARGE-$name-$(head -c $size /dev/urandom | base64 | head -c $size | tr -d '\n')"
        large_msg="${large_msg:0:$size}"  # Ensure exact size
        
        log "  Sending $name message..."
        api_send 1 "$topic" "$large_msg" > /dev/null
        sleep 3
    done
    
    log "Phase 5: Waiting for delivery (15s)..."
    sleep 15
    
    log "Phase 6: Verifying large message delivery..."
    local success=0
    for name in "${size_names[@]}"; do
        local delivered=0
        for i in $(seq 2 $TOPIC_MEMBERS); do
            if grep -q "LARGE-$name-" "$NODES_DIR/node-$i/output.log" 2>/dev/null; then
                delivered=$((delivered + 1))
            fi
        done
        local expected=$((TOPIC_MEMBERS - 1))
        if [ $delivered -ge $((expected - 1)) ]; then
            log "  ✅ $name message: $delivered/$expected nodes"
            success=$((success + 1))
        else
            warn "  ⚠️ $name message: $delivered/$expected nodes"
        fi
    done
    
    if [ $success -eq 3 ]; then
        log "✅ Large message delivery successful!"
    else
        warn "⚠️  Some large messages not fully delivered"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Large Messages"
    log "=========================================="
}

