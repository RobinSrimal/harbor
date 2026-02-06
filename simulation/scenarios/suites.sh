#!/bin/bash
#
# Suite Runners
#
# Functions to run groups of related scenarios.
#

# Run all membership scenarios
run_membership_suite() {
    log "=========================================="
    log "MEMBERSHIP TEST SUITE"
    log "=========================================="
    
    scenario_member_join
    echo ""
    sleep 5
    clear_data
    
    scenario_member_leave
    echo ""
    sleep 5
    clear_data
    
    scenario_stale_new_member
    echo ""
    sleep 5
    clear_data
    
    scenario_offline_sync
    echo ""
    sleep 5
    clear_data
    
    scenario_deduplication
    
    log "=========================================="
    log "MEMBERSHIP TEST SUITE COMPLETE"
    log "=========================================="
}

# Run all advanced scenarios
run_advanced_suite() {
    log "=========================================="
    log "ADVANCED TEST SUITE"
    log "=========================================="
    
    scenario_multi_topic
    echo ""
    sleep 5
    clear_data
    
    scenario_concurrent
    echo ""
    sleep 5
    clear_data
    
    scenario_large_message
    echo ""
    sleep 5
    clear_data
    
    scenario_harbor_sync
    echo ""
    sleep 5
    clear_data
    
    scenario_harbor_churn
    echo ""
    sleep 5
    clear_data
    
    scenario_race_join
    echo ""
    sleep 5
    clear_data
    
    scenario_flap
    echo ""
    sleep 5
    clear_data
    
    scenario_member_discovery
    
    log "=========================================="
    log "ADVANCED TEST SUITE COMPLETE"
    log "=========================================="
}

# Run file sharing suite
run_share_suite() {
    log "=========================================="
    log "FILE SHARING TEST SUITE"
    log "=========================================="
    
    scenario_share_basic
    echo ""
    sleep 5
    clear_data
    
    scenario_share_peer_offline
    echo ""
    sleep 5
    clear_data
    
    scenario_share_few_peers
    echo ""
    sleep 5
    clear_data
    
    scenario_share_large_file
    echo ""
    sleep 5
    clear_data
    
    scenario_share_retry_source
    echo ""
    sleep 5
    clear_data
    
    scenario_share_retry_recipient
    
    log "=========================================="
    log "FILE SHARING TEST SUITE COMPLETE"
    log "=========================================="
}

# Run CRDT sync suite
run_sync_suite() {
    log "=========================================="
    log "CRDT SYNC TEST SUITE"
    log "=========================================="
    
    scenario_sync_basic
    echo ""
    sleep 5
    clear_data
    
    scenario_sync_concurrent
    echo ""
    sleep 5
    clear_data
    
    scenario_sync_offline
    echo ""
    sleep 5
    clear_data
    
    scenario_sync_initial
    
    log "=========================================="
    log "CRDT SYNC TEST SUITE COMPLETE"
    log "=========================================="
}

# Run control protocol suite
run_control_suite() {
    log "=========================================="
    log "CONTROL PROTOCOL TEST SUITE"
    log "=========================================="

    scenario_control_connect
    echo ""
    sleep 5
    clear_data

    scenario_control_topic_invite
    echo ""
    sleep 5
    clear_data

    scenario_control_suggest
    echo ""
    sleep 5
    clear_data

    scenario_control_offline
    echo ""
    sleep 5
    clear_data

    scenario_control_remove_member

    log "=========================================="
    log "CONTROL PROTOCOL TEST SUITE COMPLETE"
    log "=========================================="
}

# Run connection gating suite
run_gate_suite() {
    log "=========================================="
    log "CONNECTION GATING TEST SUITE"
    log "=========================================="

    scenario_gate_dm_allowed
    echo ""
    sleep 5
    clear_data

    scenario_gate_topic_allowed
    echo ""
    sleep 5
    clear_data

    scenario_gate_blocked
    echo ""
    sleep 5
    clear_data

    scenario_gate_stranger
    echo ""
    sleep 5
    clear_data

    scenario_gate_open_protocols

    log "=========================================="
    log "CONNECTION GATING TEST SUITE COMPLETE"
    log "=========================================="
}

# Run full basic suite
run_full_suite() {
    scenario_basic
    echo ""
    sleep 5
    clear_data
    scenario_offline
    echo ""
    sleep 5
    clear_data
    scenario_dht_churn
}

