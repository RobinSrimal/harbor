#!/bin/bash
#
# Scenario: Harbor Node Churn
#
# Tests that packets aren't lost when Harbor nodes join/leave.
#

scenario_harbor_churn() {
    log "=========================================="
    log "SCENARIO: Harbor Node Churn"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=10  # Max 10 topic members
    
    start_bootstrap_node
    
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
    done
    
    log "Phase 2: Waiting for DHT convergence (45s)..."
    sleep 45
    
    log "Phase 3: Creating topic with $TOPIC_MEMBERS members..."
    local invite=$(api_create_topic 1)
    if [ -z "$invite" ]; then
        warn "Failed to create topic"
        log ""
        log "=========================================="
        log "❌ SCENARIO FAILED: Harbor Node Churn"
        log "=========================================="
        log ""
        log "Could not create topic on node-1."
        return 1
    fi
    local topic=$(echo "$invite" | head -c 64)
    info "Created topic: ${topic:0:16}..."
    
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$invite" > /dev/null 2>&1 || warn "Node-$i failed to join"
    done
    
    log "Waiting for member discovery via Harbor (60s)..."
    sleep 60
    
    log "Phase 4: Stopping target node (node-5) and sending messages..."
    stop_node 5
    sleep 3
    
    api_send 1 "$topic" "Before-Churn-Message" > /dev/null 2>&1 || warn "Failed to send pre-churn message"
    sleep 5
    
    log "Phase 5: Causing Harbor node churn - stopping nodes 15,16,17 (non-members)..."
    for i in 15 16 17; do
        stop_node $i
    done
    sleep 5
    
    log "Phase 6: Sending more messages during churn..."
    api_send 2 "$topic" "During-Churn-Message" > /dev/null 2>&1 || warn "Failed to send during-churn message"
    sleep 5
    
    log "Phase 7: Restarting churned nodes..."
    for i in 15 16 17; do
        start_node $i
    done
    sleep 10
    
    log "Phase 8: Restarting target node (node-5)..."
    start_node 5
    wait_for_api 5 || true
    sleep 3
    api_join_topic 5 "$invite" > /dev/null 2>&1 || warn "Node-5 failed to rejoin topic"
    
    log "Phase 9: Waiting for Harbor pull (90s)..."
    sleep 90
    
    log "Phase 10: Verifying node-5 received messages despite churn..."
    
    # Safe grep count - handles newlines and non-numeric output
    safe_count() {
        local result
        result=$(grep -c "$1" "$2" 2>/dev/null | tr -d '[:space:]' | head -1)
        if [[ "$result" =~ ^[0-9]+$ ]]; then
            echo "$result"
        else
            echo "0"
        fi
    }
    
    local before=$(safe_count "Before-Churn-Message" "$NODES_DIR/node-5/output.log")
    local during=$(safe_count "During-Churn-Message" "$NODES_DIR/node-5/output.log")
    
    info "  Before-Churn-Message: $before copies"
    info "  During-Churn-Message: $during copies"
    
    local passed=true
    log ""
    if [ "$before" -ge 1 ]; then
        info "  ✅ Pre-churn message delivered"
    else
        warn "  ❌ Pre-churn message lost"
        passed=false
    fi
    
    if [ "$during" -ge 1 ]; then
        info "  ✅ During-churn message delivered"
    else
        warn "  ❌ During-churn message lost"
        passed=false
    fi
    
    log ""
    log "=========================================="
    if $passed; then
        log "✅ SCENARIO PASSED: Harbor Node Churn"
        log "=========================================="
        log ""
        log "Messages survived Harbor node churn. Redundant storage working."
    else
        log "❌ SCENARIO FAILED: Harbor Node Churn"
        log "=========================================="
        log ""
        log "Some messages were lost during Harbor node churn."
        log ""
        log "Troubleshooting:"
        log "  1. Check node-5 logs: cat $NODES_DIR/node-5/output.log"
        log "  2. Verify Harbor pull occurred: grep 'Harbor PULL' $NODES_DIR/node-5/output.log"
        log "  3. Check if messages were stored: grep 'Harbor STORE' $NODES_DIR/node-*/output.log"
    fi
}

