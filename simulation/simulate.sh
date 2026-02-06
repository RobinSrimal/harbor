#!/bin/bash
#
# Harbor Simulation Test
#
# Tests DHT behavior, Harbor node message storage, and membership flows:
# 1. Starting node-1 as bootstrap (no external bootstrap needed)
# 2. Other nodes bootstrap from node-1
# 3. Creating topics and having nodes join
# 4. Sending messages
# 5. Taking nodes offline
# 6. Verifying message delivery via Harbor
#
# Usage:
#   ./simulate.sh                              # Run full simulation
#   ./simulate.sh --nodes 5                    # Use 5 nodes
#   ./simulate.sh --scenario offline           # Run specific scenario
#
# Available scenarios (run with ./simulate.sh <scenario>):
#
#   Basic:
#     basic            - Basic topic creation and messaging
#     offline          - Offline node message retrieval via Harbor
#     dht-churn        - DHT churn test (nodes joining/leaving)
#
#   Membership:
#     member-join      - Member join flow verification
#     member-leave     - Member leave flow verification
#     new-to-offline   - New member to offline member delivery
#     offline-sync     - Offline sync with membership changes
#     deduplication    - Duplicate message prevention test
#
#   Advanced:
#     multi-topic      - Multi-topic isolation test
#     concurrent       - Concurrent senders stress test
#     large-message    - Large message delivery test (1KB-100KB)
#     scale-members    - Scale test with max 10 members
#     harbor-sync      - Harbor delivery for offline members
#     harbor-churn     - Harbor node churn resilience test
#     race-join        - Message during join race condition
#     flap             - Rapid join/leave (flapping) test
#     dht-pool         - DHT connection pool verification
#
#   File Sharing:
#     share-basic      - Basic file sharing test
#     share-peer-offline - Peer offline during file share
#     share-few-peers  - Adaptive splitting with few peers
#     share-large-file - Large file (10MB) transfer test
#     share-retry-source    - Retry when source goes offline
#     share-retry-recipient - Retry when recipient goes offline
#
#   CRDT Sync:
#     sync-basic       - Basic collaborative text sync
#     sync-concurrent  - Concurrent edits from multiple nodes
#     sync-offline     - Offline node receives sync updates
#     sync-initial     - New member gets full document state
#
#   Control:
#     control-connect       - Control connection flow with invite token
#     control-topic-invite  - Topic invite/accept flow via Control ALPN
#     control-suggest       - Peer introduction/suggestion flow
#     control-offline       - Offline delivery via Harbor for Control messages
#     control-remove-member - Member removal with epoch key rotation
#
#   Connection Gating:
#     gate-dm-allowed       - DM-connected peers can use gated protocols
#     gate-topic-allowed    - Topic peers can use gated protocols
#     gate-blocked          - Blocked peers are rejected from gated protocols
#     gate-stranger         - Strangers are rejected from gated protocols
#     gate-open-protocols   - DHT/Harbor/Control remain open to everyone
#
#   Suites:
#     membership       - Run all membership scenarios
#     advanced         - Run all advanced scenarios
#     share            - Run all file sharing scenarios
#     sync             - Run all CRDT sync scenarios
#     control          - Run all control scenarios
#     gate             - Run all connection gating scenarios
#     full             - Run all scenarios (default)

set -e

# Get the script directory
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ============================================================================
# SOURCE LIBRARIES AND SCENARIOS
# ============================================================================

source "$SCRIPT_DIR/lib/common.sh"
source "$SCRIPT_DIR/lib/node_mgmt.sh"

# Source all scenario files
source "$SCRIPT_DIR/scenarios/basic.sh"
source "$SCRIPT_DIR/scenarios/offline.sh"
source "$SCRIPT_DIR/scenarios/dht_churn.sh"
source "$SCRIPT_DIR/scenarios/member_join.sh"
source "$SCRIPT_DIR/scenarios/member_leave.sh"
source "$SCRIPT_DIR/scenarios/new_to_offline.sh"
source "$SCRIPT_DIR/scenarios/offline_sync.sh"
source "$SCRIPT_DIR/scenarios/deduplication.sh"
source "$SCRIPT_DIR/scenarios/multi_topic.sh"
source "$SCRIPT_DIR/scenarios/concurrent.sh"
source "$SCRIPT_DIR/scenarios/large_message.sh"
source "$SCRIPT_DIR/scenarios/scale_members.sh"
source "$SCRIPT_DIR/scenarios/harbor_sync.sh"
# forward_secrecy.sh archived - requires MLS Tier 3 (not Tier 1)
# source "$SCRIPT_DIR/scenarios/forward_secrecy.sh"
# wildcard_catchup.sh archived - catch-up mode removed
# source "$SCRIPT_DIR/scenarios/wildcard_catchup.sh"
source "$SCRIPT_DIR/scenarios/harbor_churn.sh"
source "$SCRIPT_DIR/scenarios/race_join.sh"
source "$SCRIPT_DIR/scenarios/flap.sh"
source "$SCRIPT_DIR/scenarios/dht_pool.sh"
source "$SCRIPT_DIR/scenarios/share_basic.sh"
source "$SCRIPT_DIR/scenarios/share_peer_offline.sh"
source "$SCRIPT_DIR/scenarios/share_few_peers.sh"
source "$SCRIPT_DIR/scenarios/share_large_file.sh"
source "$SCRIPT_DIR/scenarios/share_retry_source.sh"
source "$SCRIPT_DIR/scenarios/share_retry_recipient.sh"
source "$SCRIPT_DIR/scenarios/sync_basic.sh"
source "$SCRIPT_DIR/scenarios/sync_concurrent.sh"
source "$SCRIPT_DIR/scenarios/sync_offline.sh"
source "$SCRIPT_DIR/scenarios/sync_initial.sh"
source "$SCRIPT_DIR/scenarios/stream_basic.sh"
source "$SCRIPT_DIR/scenarios/stream_reject.sh"
source "$SCRIPT_DIR/scenarios/stream_end.sh"
source "$SCRIPT_DIR/scenarios/dm_basic.sh"
source "$SCRIPT_DIR/scenarios/dm_offline.sh"
source "$SCRIPT_DIR/scenarios/dm_sync_basic.sh"
source "$SCRIPT_DIR/scenarios/dm_sync_offline.sh"
source "$SCRIPT_DIR/scenarios/dm_share_basic.sh"
source "$SCRIPT_DIR/scenarios/dm_share_offline.sh"
source "$SCRIPT_DIR/scenarios/dm_stream_basic.sh"
source "$SCRIPT_DIR/scenarios/dm_stream_offline.sh"
source "$SCRIPT_DIR/scenarios/control_connect.sh"
source "$SCRIPT_DIR/scenarios/control_topic_invite.sh"
source "$SCRIPT_DIR/scenarios/control_suggest.sh"
source "$SCRIPT_DIR/scenarios/control_offline.sh"
source "$SCRIPT_DIR/scenarios/control_remove_member.sh"
source "$SCRIPT_DIR/scenarios/gate_dm_allowed.sh"
source "$SCRIPT_DIR/scenarios/gate_topic_allowed.sh"
source "$SCRIPT_DIR/scenarios/gate_blocked.sh"
source "$SCRIPT_DIR/scenarios/gate_stranger.sh"
source "$SCRIPT_DIR/scenarios/gate_open_protocols.sh"
source "$SCRIPT_DIR/scenarios/suites.sh"

# ============================================================================
# INITIALIZATION
# ============================================================================

# Find the harbor binary
find_harbor_binary

# Set up cleanup trap
# trap cleanup EXIT  # disabled for debugging

# Default scenario
SCENARIO="full"

# Parse args
while [[ $# -gt 0 ]]; do
    case $1 in
        --nodes) NODE_COUNT="$2"; shift 2 ;;
        --scenario) SCENARIO="$2"; shift 2 ;;
        -*) shift ;;  # Skip unknown flags
        *) SCENARIO="$1"; shift ;;  # Positional arg = scenario
    esac
done

# ============================================================================
# MAIN
# ============================================================================

echo ""
echo "╔════════════════════════════════════════╗"
echo "║     Harbor Network Simulation          ║"
echo "╚════════════════════════════════════════╝"
echo ""

clear_data

case $SCENARIO in
    basic)
        scenario_basic
        ;;
    offline)
        scenario_offline
        ;;
    dht-churn)
        scenario_dht_churn
        ;;
    # Membership scenarios
    member-join)
        scenario_member_join
        ;;
    member-leave)
        scenario_member_leave
        ;;
    new-to-offline)
        scenario_new_to_offline
        ;;
    offline-sync)
        scenario_offline_sync
        ;;
    deduplication)
        scenario_deduplication
        ;;
    membership)
        run_membership_suite
        ;;
    # Advanced scenarios
    multi-topic)
        scenario_multi_topic
        ;;
    concurrent)
        scenario_concurrent
        ;;
    large-message)
        scenario_large_message
        ;;
    scale-members)
        scenario_scale_members
        ;;
    harbor-sync)
        scenario_harbor_sync
        ;;
    forward-secrecy|fs)
        echo "forward-secrecy scenario archived - requires Tier 3 MLS"
        exit 1
        ;;
    wildcard-catchup|wildcard|catchup)
        echo "wildcard-catchup scenario archived - catch-up mode removed"
        exit 1
        ;;
    harbor-churn)
        scenario_harbor_churn
        ;;
    race-join)
        scenario_race_join
        ;;
    flap)
        scenario_flap
        ;;
    dht-pool)
        scenario_dht_pool
        ;;
    # File sharing scenarios
    share-basic)
        scenario_share_basic
        ;;
    share-peer-offline)
        scenario_share_peer_offline
        ;;
    share-few-peers)
        scenario_share_few_peers
        ;;
    share-large-file)
        scenario_share_large_file
        ;;
    share-retry-source)
        scenario_share_retry_source
        ;;
    share-retry-recipient)
        scenario_share_retry_recipient
        ;;
    share)
        run_share_suite
        ;;
    # CRDT Sync scenarios
    sync-basic)
        scenario_sync_basic
        ;;
    sync-concurrent)
        scenario_sync_concurrent
        ;;
    sync-offline)
        scenario_sync_offline
        ;;
    sync-initial)
        scenario_sync_initial
        ;;
    sync)
        run_sync_suite
        ;;
    stream-basic)
        scenario_stream_basic
        ;;
    stream-reject)
        scenario_stream_reject
        ;;
    stream-end)
        scenario_stream_end
        ;;
    dm-basic)
        scenario_dm_basic
        ;;
    dm-offline)
        scenario_dm_offline
        ;;
    dm-sync-basic)
        scenario_dm_sync_basic
        ;;
    dm-sync-offline)
        scenario_dm_sync_offline
        ;;
    dm-share-basic)
        scenario_dm_share_basic
        ;;
    dm-share-offline)
        scenario_dm_share_offline
        ;;
    dm-stream-basic)
        scenario_dm_stream_basic
        ;;
    dm-stream-offline)
        scenario_dm_stream_offline
        ;;
    # Control scenarios
    control-connect)
        scenario_control_connect
        ;;
    control-topic-invite)
        scenario_control_topic_invite
        ;;
    control-suggest)
        scenario_control_suggest
        ;;
    control-offline)
        scenario_control_offline
        ;;
    control-remove-member)
        scenario_control_remove_member
        ;;
    control)
        run_control_suite
        ;;
    # Connection gating scenarios
    gate-dm-allowed)
        scenario_gate_dm_allowed
        ;;
    gate-topic-allowed)
        scenario_gate_topic_allowed
        ;;
    gate-blocked)
        scenario_gate_blocked
        ;;
    gate-stranger)
        scenario_gate_stranger
        ;;
    gate-open-protocols)
        scenario_gate_open_protocols
        ;;
    gate)
        run_gate_suite
        ;;
    advanced)
        run_advanced_suite
        ;;
    full)
        run_full_suite
        ;;
    *)
        echo ""
        echo "Unknown scenario: '$SCENARIO'"
        echo ""
        echo "Available scenarios:"
        echo "  Basic:      basic, offline, dht-churn"
        echo "  Membership: member-join, member-leave, new-to-offline, offline-sync, deduplication"
        echo "  Advanced:   multi-topic, concurrent, large-message, scale-members,"
        echo "              harbor-sync, harbor-churn, race-join, flap, dht-pool"
        echo "  Share:      share-basic, share-peer-offline, share-few-peers, share-large-file,"
        echo "              share-retry-source, share-retry-recipient"
        echo "  Sync:       sync-basic, sync-concurrent, sync-offline, sync-initial"
        echo "  Stream:     stream-basic, stream-reject, stream-end"
        echo "  DM:         dm-basic, dm-offline"
        echo "  DM Stream:  dm-stream-basic, dm-stream-offline"
        echo "  Control:    control-connect, control-topic-invite, control-suggest, control-offline, control-remove-member"
        echo "  Gate:       gate-dm-allowed, gate-topic-allowed, gate-blocked, gate-stranger, gate-open-protocols"
        echo "  Suites:     membership, advanced, share, sync, control, gate, full"
        echo ""
        exit 1
        ;;
esac

echo ""
log "All scenarios complete!"
