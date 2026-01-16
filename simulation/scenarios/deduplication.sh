#!/bin/bash
#
# Scenario: Duplicate Message Prevention
#
# Tests that messages are only processed once, even if
# pulled multiple times from Harbor.
#

scenario_deduplication() {
    log "=========================================="
    log "SCENARIO: Duplicate Message Prevention"
    log "=========================================="
    
    local TEST_NODES=20
    local TOPIC_MEMBERS=10  # Max 10 topic members
    
    # Start bootstrap and all nodes
    start_bootstrap_node
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
        sleep 0.3
    done
    for i in $(seq 2 $TEST_NODES); do
        wait_for_api $i
    done
    
    # Wait for DHT
    log "Phase 1: DHT convergence (75s)..."
    sleep 75
    
    # Create topic and have nodes 1-10 join (max 10 members)
    log "Phase 2: Creating topic and joining nodes 1-$TOPIC_MEMBERS..."
    INVITE=$(api_create_topic 1)
    TOPIC_ID="${INVITE:0:64}"
    local current_invite="$INVITE"
    for i in $(seq 2 $TOPIC_MEMBERS); do
        api_join_topic $i "$current_invite" > /dev/null
        sleep 0.5
        current_invite=$(api_get_invite 1 "$TOPIC_ID")
    done
    sleep 2
    
    # Multiple nodes send unique messages
    log "Phase 3: Multiple nodes sending unique messages..."
    for sender in 1 3 5 7 9; do
        api_send $sender "$TOPIC_ID" "Unique message from Node-$sender" > /dev/null
        sleep 1
    done
    sleep 3
    
    # Count occurrences in logs (should be 1 each)
    log "Phase 4: Counting message occurrences before Harbor sync..."
    local all_good=true
    for sender in 1 3 5 7 9; do
        for receiver in 2 4 6 8 10; do
            if [ $sender -ne $receiver ]; then
                local count=$(grep -c "Unique message from Node-$sender" "$NODES_DIR/node-$receiver/output.log" 2>/dev/null || echo "0")
                if [ "$count" != "0" ] && [ "$count" != "1" ]; then
                    warn "  Node-$receiver has $count copies of message from Node-$sender"
                    all_good=false
                fi
            fi
        done
    done
    
    # Wait for Harbor sync (messages may be pulled again)
    log "Phase 5: Waiting for Harbor sync (may pull same messages)..."
    sleep 75
    
    # Count again (should still be 1 due to deduplication)
    log "Phase 6: Counting message occurrences after Harbor sync..."
    local dedup_working=true
    for sender in 1 3 5 7 9; do
        for receiver in 2 4 6 8 10; do
            if [ $sender -ne $receiver ]; then
                local count=$(grep -c "Unique message from Node-$sender" "$NODES_DIR/node-$receiver/output.log" 2>/dev/null || echo "0")
                if [ "$count" != "0" ] && [ "$count" != "1" ]; then
                    warn "  Node-$receiver has $count copies of message from Node-$sender after sync"
                    dedup_working=false
                fi
            fi
        done
    done
    
    if $dedup_working; then
        log "✅ Deduplication working - all messages processed only once!"
    else
        warn "⚠️  Some messages were processed multiple times"
    fi
    
    log "=========================================="
    log "SCENARIO COMPLETE: Duplicate Message Prevention"
    log "=========================================="
}

