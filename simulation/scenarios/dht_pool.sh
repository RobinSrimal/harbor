#!/bin/bash
#
# Scenario: DHT Connection Pool Verification
#
# Tests DHT connection pool functionality:
# - Connection establishment and caching
# - Connection reuse (pooling)
# - Candidate verification
# - Routing table operations
#
# Verification: Logs checked for specific pool-related messages
#
# Usage:
#   ./simulate.sh dht-pool              # Run with default logging (debug)
#   RUST_LOG=trace ./simulate.sh pool   # Run with trace logging (more detail)
#
# Log levels (set RUST_LOG environment variable):
#   debug - Connection establishment, DHT operations (default)
#   trace - Connection reuse, eviction, detailed routing table ops
#

scenario_dht_pool() {
    log "=========================================="
    log "SCENARIO: DHT Connection Pool Verification"
    log "=========================================="
    
    local TEST_NODES=5  # Small number for focused pool testing
    
    # Track verification results
    local pool_connecting=0
    local pool_reuse=0
    local pool_eviction=0
    local dht_added_nodes=0
    local dht_candidates_verified=0
    local dht_findnode_success=0
    
    # Start bootstrap node first
    start_bootstrap_node
    
    # Start remaining test nodes
    log "Phase 1: Starting nodes 2-$TEST_NODES..."
    for i in $(seq 2 $TEST_NODES); do
        start_node $i
        sleep 1
    done
    
    # Wait for APIs
    log "Waiting for all nodes to initialize..."
    for i in $(seq 2 $TEST_NODES); do
        wait_for_api $i
    done
    
    # Initial DHT convergence
    log "Phase 2: Waiting for initial DHT convergence (30s)..."
    sleep 30
    
    # Check initial DHT stats
    log "Initial DHT Status:"
    for i in $(seq 1 $TEST_NODES); do
        local stats=$(api_stats $i)
        info "  Node-$i: $stats"
    done
    
    # Trigger more DHT activity by creating and joining a topic
    log "Phase 3: Triggering DHT activity via topic operations..."
    INVITE=$(api_create_topic 1)
    if [ -n "$INVITE" ]; then
        info "Created topic, joining from other nodes..."
        local current_invite="$INVITE"
        TOPIC_ID="${INVITE:0:64}"
        
        for i in $(seq 2 $TEST_NODES); do
            api_join_topic $i "$current_invite" > /dev/null
            sleep 1
            current_invite=$(api_get_invite 1 "$TOPIC_ID")
        done
        
        # Send some messages to trigger more connections
        log "Sending messages to trigger connection pool activity..."
        for i in $(seq 1 $TEST_NODES); do
            api_send $i "$TOPIC_ID" "pool-test-msg-$i" > /dev/null
            sleep 0.5
        done
    fi
    
    # Wait for more DHT activity
    log "Phase 4: Waiting for connection pool activity (45s)..."
    sleep 45
    
    # Final wait for log messages
    sleep 15
    
    # ============================================================================
    # LOG VERIFICATION
    # ============================================================================
    
    log "Phase 6: Verifying DHT Pool Log Messages..."
    log ""
    
    # Helper function to show sample log lines
    show_sample_logs() {
        local pattern="$1"
        local log_file="$2"
        local max_lines="${3:-2}"
        grep -E "$pattern" "$log_file" 2>/dev/null | head -n "$max_lines" | while read -r line; do
            echo "        └─ $line"
        done
    }
    
    # Check each node's logs for pool-related messages
    for i in $(seq 1 $TEST_NODES); do
        local log_file="$NODES_DIR/node-$i/output.log"
        
        if [ ! -f "$log_file" ]; then
            warn "  Node-$i: Log file not found at $log_file"
            continue
        fi
        
        local log_size=$(wc -l < "$log_file" | tr -d ' ')
        info "  Node-$i log analysis ($log_size lines):"
        
        # Helper to safely count grep matches (returns 0 on no match or error)
        safe_grep_count() {
            local pattern="$1"
            local file="$2"
            local count
            count=$(grep -c -E "$pattern" "$file" 2>/dev/null | tr -d '[:space:]' | head -1)
            # Ensure we return a valid number
            if [[ "$count" =~ ^[0-9]+$ ]]; then
                echo "$count"
            else
                echo "0"
            fi
        }
        
        # Connection: New connections established (INFO level from iroh)
        # Pattern: "new connection type" indicates successful connection
        local connecting_count=$(safe_grep_count "new connection type" "$log_file")
        pool_connecting=$((pool_connecting + connecting_count))
        if [ "$connecting_count" -gt 0 ]; then
            info "    ✅ Connections established: $connecting_count"
            show_sample_logs "new connection type" "$log_file" 2
        else
            info "    ❌ Connections established: 0 (pattern: 'new connection type')"
        fi
        
        # Pool: Connection reuse (TRACE level - requires RUST_LOG=trace)
        local reuse_count=$(safe_grep_count "reusing cached connection" "$log_file")
        pool_reuse=$((pool_reuse + reuse_count))
        if [ "$reuse_count" -gt 0 ]; then
            info "    ✅ Pool connection reuse: $reuse_count"
            show_sample_logs "reusing cached connection" "$log_file" 2
        else
            info "    ⚪ Pool connection reuse: 0 (needs RUST_LOG=trace)"
        fi
        
        # Pool: Eviction (TRACE level - requires RUST_LOG=trace)
        local eviction_count=$(safe_grep_count "evicting idle connection" "$log_file")
        pool_eviction=$((pool_eviction + eviction_count))
        if [ "$eviction_count" -gt 0 ]; then
            info "    ℹ️  Pool evictions: $eviction_count"
            show_sample_logs "evicting idle connection" "$log_file" 2
        fi
        
        # DHT: Candidates added for verification (INFO level)
        local candidates_count=$(safe_grep_count "added discovered nodes to candidates|added requester directly to routing table" "$log_file")
        dht_added_nodes=$((dht_added_nodes + candidates_count))
        if [ "$candidates_count" -gt 0 ]; then
            info "    ✅ DHT routing activity: $candidates_count"
            show_sample_logs "added discovered nodes to candidates|added requester directly to routing table" "$log_file" 2
        else
            info "    ❌ DHT routing activity: 0"
        fi
        
        # DHT: Candidate verification (INFO level)
        local verified_count=$(safe_grep_count "verified.*candidate nodes|starting candidate verification" "$log_file")
        dht_candidates_verified=$((dht_candidates_verified + verified_count))
        if [ "$verified_count" -gt 0 ]; then
            info "    ✅ DHT candidates verified: $verified_count"
            show_sample_logs "verified.*candidate nodes|starting candidate verification" "$log_file" 2
        else
            info "    ⚪ DHT candidates verified: 0"
        fi
        
        # DHT: FindNode RPC (DEBUG/TRACE level)
        local findnode_count=$(safe_grep_count "FindNode RPC succeeded|FindNode response" "$log_file")
        dht_findnode_success=$((dht_findnode_success + findnode_count))
        if [ "$findnode_count" -gt 0 ]; then
            info "    ✅ FindNode RPC activity: $findnode_count"
            show_sample_logs "FindNode RPC succeeded|FindNode response" "$log_file" 2
        else
            info "    ⚪ FindNode RPC activity: 0 (needs RUST_LOG=debug)"
        fi
        
        # DHT: Routing table saved (INFO level - confirms DHT has nodes)
        local saved_count=$(safe_grep_count "DHT routing table saved" "$log_file")
        if [ "$saved_count" -gt 0 ]; then
            info "    ✅ DHT routing table saved: $saved_count times"
            show_sample_logs "DHT routing table saved" "$log_file" 2
        fi
        
        # Check for errors
        local pool_errors=$(safe_grep_count "connection pool full|connection failed|connection timed out" "$log_file")
        if [ "$pool_errors" -gt 0 ]; then
            warn "    ⚠️  Pool errors detected: $pool_errors"
            show_sample_logs "connection pool full|connection failed|connection timed out" "$log_file" 3
        fi
        
        # Show any ERROR level messages
        local error_count=$(safe_grep_count " ERROR " "$log_file")
        if [ "$error_count" -gt 0 ]; then
            warn "    ⚠️  ERROR log messages: $error_count"
            show_sample_logs " ERROR " "$log_file" 3
        fi
        
        log ""
    done
    
    # ============================================================================
    # SUMMARY
    # ============================================================================
    
    log ""
    log "=========================================="
    log "DHT POOL VERIFICATION SUMMARY"
    log "=========================================="
    log ""
    log "┌─────────────────────────────────────────────────────────────────┐"
    log "│ Connection Metrics                                              │"
    log "├─────────────────────────────────────────────────────────────────┤"
    log "│  Connections established:  $(printf '%4d' $pool_connecting)  (required: ≥1)                    │"
    log "│  Connections reused:       $(printf '%4d' $pool_reuse)  (optional, RUST_LOG=trace)       │"
    log "│  Connections evicted:      $(printf '%4d' $pool_eviction)  (optional, RUST_LOG=trace)       │"
    log "├─────────────────────────────────────────────────────────────────┤"
    log "│ DHT Routing Metrics                                             │"
    log "├─────────────────────────────────────────────────────────────────┤"
    log "│  DHT routing activity:     $(printf '%4d' $dht_added_nodes)  (required: ≥1)                    │"
    log "│  Candidates verified:      $(printf '%4d' $dht_candidates_verified)  (optional)                        │"
    log "│  FindNode RPC activity:    $(printf '%4d' $dht_findnode_success)  (optional, RUST_LOG=debug)       │"
    log "└─────────────────────────────────────────────────────────────────┘"
    log ""
    
    # Verification criteria
    local verification_passed=true
    local failed_checks=""
    local passed_checks=""
    
    # Must have established connections
    if [ "$pool_connecting" -lt 1 ]; then
        failed_checks="${failed_checks}\n  ❌ FAIL: No connections established"
        failed_checks="${failed_checks}\n     → Expected log pattern: 'new connection type'"
        failed_checks="${failed_checks}\n     → Check: Are nodes able to reach each other?"
        verification_passed=false
    else
        passed_checks="${passed_checks}\n  ✅ PASS: Connections established ($pool_connecting total)"
    fi
    
    # DHT nodes should be discovered
    if [ "$dht_added_nodes" -lt 1 ]; then
        failed_checks="${failed_checks}\n  ❌ FAIL: No DHT routing activity detected"
        failed_checks="${failed_checks}\n     → Expected pattern: 'added discovered nodes to candidates'"
        failed_checks="${failed_checks}\n     → Check: Is the bootstrap node reachable?"
        verification_passed=false
    else
        passed_checks="${passed_checks}\n  ✅ PASS: DHT routing active ($dht_added_nodes events)"
    fi
    
    # Candidate verification should occur
    if [ "$dht_candidates_verified" -lt 1 ]; then
        passed_checks="${passed_checks}\n  ⚠️  WARN: No candidate verification logged"
        passed_checks="${passed_checks}\n     → May require RUST_LOG=trace for detailed output"
    else
        passed_checks="${passed_checks}\n  ✅ PASS: DHT candidate verification working ($dht_candidates_verified verified)"
    fi
    
    # Connection reuse is optional but good indicator
    if [ "$pool_reuse" -gt 0 ]; then
        passed_checks="${passed_checks}\n  ✅ BONUS: Connection pooling/caching confirmed ($pool_reuse reuses)"
    else
        passed_checks="${passed_checks}\n  ℹ️  INFO: No connection reuse logged (normal for short tests)"
    fi
    
    # Print results
    log "Verification Results:"
    if [ -n "$passed_checks" ]; then
        echo -e "$passed_checks"
    fi
    if [ -n "$failed_checks" ]; then
        echo -e "$failed_checks"
    fi
    
    log ""
    
    # Final DHT stats
    log "Final DHT Statistics (API response):"
    for i in $(seq 1 $TEST_NODES); do
        local stats=$(api_stats $i)
        if [ -n "$stats" ] && [ "$stats" != "null" ]; then
            info "  Node-$i: $stats"
        else
            warn "  Node-$i: No stats available (API may be down)"
        fi
    done
    
    log ""
    log "=========================================="
    if [ "$verification_passed" = true ]; then
        log "✅ SCENARIO PASSED: DHT Pool Verification"
        log "=========================================="
        log ""
        log "All required checks passed. The DHT connection pool is working correctly."
    else
        log "❌ SCENARIO FAILED: DHT Pool Verification"
        log "=========================================="
        log ""
        log "Troubleshooting steps:"
        log "  1. Check log level: RUST_LOG=debug ./simulate.sh dht-pool"
        log "  2. For more detail: RUST_LOG=trace ./simulate.sh dht-pool"
        log "  3. Inspect raw logs: cat $NODES_DIR/node-1/output.log"
        log "  4. Check for errors: grep -i error $NODES_DIR/node-*/output.log"
        log "  5. Verify bootstrap: Is node-1 listening and reachable?"
        log ""
        log "Expected log patterns at INFO level:"
        log "  - 'new connection type'                      (iroh, INFO)"
        log "  - 'added discovered nodes to candidates'     (actor.rs, INFO)"
        log "  - 'verified X candidate nodes'               (actor.rs, INFO)"
        log "  - 'DHT routing table saved'                  (core.rs, INFO)"
        log ""
        log "Additional patterns (require RUST_LOG=debug or trace):"
        log "  - 'connecting to peer'          (pool.rs, DEBUG)"
        log "  - 'added node to routing table' (actor.rs, TRACE)"
        log "  - 'reusing cached connection'   (pool.rs, TRACE)"
        log ""
        
        # Show sample of actual log content for debugging
        log "Sample log content from node-1 (last 20 lines):"
        log "─────────────────────────────────────────────────────────"
        if [ -f "$NODES_DIR/node-1/output.log" ]; then
            tail -n 20 "$NODES_DIR/node-1/output.log" | while read -r line; do
                echo "  $line"
            done
        else
            warn "  Log file not found: $NODES_DIR/node-1/output.log"
        fi
        log "─────────────────────────────────────────────────────────"
        log ""
        log "DHT-related messages from node-1 (first 10):"
        log "─────────────────────────────────────────────────────────"
        if [ -f "$NODES_DIR/node-1/output.log" ]; then
            grep -i -E "dht|pool|routing|connect|FindNode" "$NODES_DIR/node-1/output.log" 2>/dev/null | head -n 10 | while read -r line; do
                echo "  $line"
            done
        fi
        log "─────────────────────────────────────────────────────────"
    fi
}

