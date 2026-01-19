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

