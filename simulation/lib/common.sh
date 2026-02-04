#!/bin/bash
#
# Harbor Simulation - Common Utilities
#
# This file contains shared functions for:
# - Colors and logging
# - Configuration variables
# - API helper functions
#

# ============================================================================
# CONFIGURATION
# ============================================================================

SCRIPT_DIR="${SCRIPT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
NODES_DIR="$SCRIPT_DIR/nodes"
BASE_API_PORT=9001

# Default config
NODE_COUNT=${NODE_COUNT:-20}

# Bootstrap info file
BOOTSTRAP_FILE="$SCRIPT_DIR/bootstrap.txt"

# ============================================================================
# COLORS AND LOGGING
# ============================================================================

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${GREEN}[SIM]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
fail() { echo -e "${RED}[FAIL]${NC} $1"; exit 1; }
info() { echo -e "${BLUE}[INFO]${NC} $1"; }

# ============================================================================
# API HELPERS
# ============================================================================

api_port() {
    echo $((BASE_API_PORT + $1 - 1))
}

# Log API timeout/connection failures (to stderr so it's not swallowed by > /dev/null)
# curl exit codes: 7=connection refused, 28=timeout
log_api_error() {
    local exit_code=$1
    local node=$2
    local endpoint=$3
    if [ $exit_code -eq 28 ]; then
        echo -e "${YELLOW}[WARN]${NC} API TIMEOUT: Node-$node $endpoint (exit code 28)" >&2
    elif [ $exit_code -eq 7 ]; then
        echo -e "${YELLOW}[WARN]${NC} API CONNECTION REFUSED: Node-$node $endpoint (exit code 7)" >&2
    elif [ $exit_code -ne 0 ]; then
        echo -e "${YELLOW}[WARN]${NC} API ERROR: Node-$node $endpoint (exit code $exit_code)" >&2
    fi
}

api_create_topic() {
    local n=$1
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/topic")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/topic"
    echo "$result"
}

api_join_topic() {
    local n=$1
    local invite=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/join" -d "$invite")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/join"
    echo "$result"
}

api_send() {
    local n=$1
    local topic=$2
    local message=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/send" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\",\"message\":\"$message\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/send"
    echo "$result"
}

api_send_dm() {
    local n=$1
    local recipient=$2
    local message=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/dm" \
        -H "Content-Type: application/json" \
        -d "{\"recipient\":\"$recipient\",\"message\":\"$message\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/dm"
    echo "$result"
}

api_stats() {
    local n=$1
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/stats")
    local exit_code=$?
    log_api_error $exit_code $n "GET /api/stats"
    echo "$result"
}

api_topics() {
    local n=$1
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/topics")
    local exit_code=$?
    log_api_error $exit_code $n "GET /api/topics"
    echo "$result"
}

api_get_invite() {
    local n=$1
    local topic=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/invite/$topic")
    local exit_code=$?
    log_api_error $exit_code $n "GET /api/invite"
    echo "$result"
}

wait_for_api() {
    local n=$1
    local port=$(api_port $n)
    local attempts=0
    while ! curl -s --connect-timeout 2 --max-time 2 "http://127.0.0.1:$port/api/health" > /dev/null 2>&1; do
        sleep 0.5
        attempts=$((attempts + 1))
        if [ $attempts -gt 20 ]; then
            fail "Node-$n API not responding after 10s"
        fi
    done
}

# ============================================================================
# SHARE API HELPERS
# ============================================================================

# Share a file with a topic
# Usage: api_share_file <node> <topic_hex> <file_path>
# Returns: JSON with hash
api_share_file() {
    local n=$1
    local topic=$2
    local file_path=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 120 -X POST "http://127.0.0.1:$port/api/share" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\",\"file_path\":\"$file_path\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/share"
    echo "$result"
}

# Get share status for a blob
# Usage: api_share_status <node> <hash_hex>
# Returns: JSON with status
api_share_status() {
    local n=$1
    local hash=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/share/status/$hash")
    local exit_code=$?
    log_api_error $exit_code $n "GET /api/share/status"
    echo "$result"
}

# List blobs for a topic
# Usage: api_list_blobs <node> <topic_hex>
# Returns: JSON with blobs array
api_list_blobs() {
    local n=$1
    local topic=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/share/blobs/$topic")
    local exit_code=$?
    log_api_error $exit_code $n "GET /api/share/blobs"
    echo "$result"
}

# Request to pull a blob
# Usage: api_share_pull <node> <hash_hex>
api_share_pull() {
    local n=$1
    local hash=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/share/pull" \
        -H "Content-Type: application/json" \
        -d "{\"hash\":\"$hash\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/share/pull"
    echo "$result"
}

# Export a blob to a file
# Usage: api_share_export <node> <hash_hex> <dest_path>
api_share_export() {
    local n=$1
    local hash=$2
    local dest_path=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 120 -X POST "http://127.0.0.1:$port/api/share/export" \
        -H "Content-Type: application/json" \
        -d "{\"hash\":\"$hash\",\"dest_path\":\"$dest_path\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/share/export"
    echo "$result"
}

# Wait for a blob to be complete on a node
# Usage: wait_for_blob_complete <node> <hash_hex> [timeout_seconds]
wait_for_blob_complete() {
    local n=$1
    local hash=$2
    local timeout=${3:-300}  # Default 5 minutes
    local attempts=0
    local interval=2
    local max_attempts=$((timeout / interval))
    
    while [ $attempts -lt $max_attempts ]; do
        local status=$(api_share_status $n $hash)
        local state=$(echo "$status" | grep -o '"state":"[^"]*"' | cut -d'"' -f4)
        
        if [ "$state" = "complete" ]; then
            return 0
        fi
        
        sleep $interval
        attempts=$((attempts + 1))
    done
    
    return 1  # Timeout
}

# Create a test file of specified size
# Usage: create_test_file <path> <size_mb>
create_test_file() {
    local path=$1
    local size_mb=$2
    dd if=/dev/urandom of="$path" bs=1M count=$size_mb 2>/dev/null
}

# Verify file hashes match
# Usage: verify_file_hash <file1> <file2>
verify_file_hash() {
    local file1=$1
    local file2=$2
    
    if command -v sha256sum &> /dev/null; then
        local hash1=$(sha256sum "$file1" | cut -d' ' -f1)
        local hash2=$(sha256sum "$file2" | cut -d' ' -f1)
    elif command -v shasum &> /dev/null; then
        local hash1=$(shasum -a 256 "$file1" | cut -d' ' -f1)
        local hash2=$(shasum -a 256 "$file2" | cut -d' ' -f1)
    else
        warn "No hash tool available, skipping verification"
        return 0
    fi
    
    if [ "$hash1" = "$hash2" ]; then
        return 0
    else
        return 1
    fi
}

# ============================================================================
# SYNC API HELPERS (Transport layer - raw CRDT bytes)
# ============================================================================

# Send a sync update (CRDT delta) to all topic members
# Usage: api_sync_update <node> <topic_hex> <data_hex>
api_sync_update() {
    local n=$1
    local topic=$2
    local data_hex=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/sync/update" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\",\"data\":\"$data_hex\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/sync/update"
    echo "$result"
}

# Request sync state from topic members
# Usage: api_sync_request <node> <topic_hex>
api_sync_request() {
    local n=$1
    local topic=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/sync/request" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/sync/request"
    echo "$result"
}

# Respond to a sync request with full state (direct SYNC_ALPN)
# Usage: api_sync_respond <node> <topic_hex> <requester_hex> <data_hex>
api_sync_respond() {
    local n=$1
    local topic=$2
    local requester=$3
    local data_hex=$4
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/sync/respond" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\",\"requester\":\"$requester\",\"data\":\"$data_hex\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/sync/respond"
    echo "$result"
}

# Generate mock CRDT update bytes (hex-encoded)
# Usage: mock_crdt_update <text>
# Returns: Hex string representing mock CRDT update
mock_crdt_update() {
    local text="$1"
    # Simple mock: just hex-encode the text with a version prefix
    echo -n "01" # Version byte
    echo -n "$text" | xxd -p | tr -d '\n'
}

# Generate mock CRDT snapshot bytes (hex-encoded)
# Usage: mock_crdt_snapshot <text>
# Returns: Hex string representing mock CRDT full state
mock_crdt_snapshot() {
    local text="$1"
    # Simple mock: hex-encode the text with a snapshot prefix
    echo -n "02" # Snapshot byte
    echo -n "$text" | xxd -p | tr -d '\n'
}
extract_sync_text() {
    local json=$1
    echo "$json" | grep -o '"text":"[^"]*"' | cut -d'"' -f4
}

# ============================================================================
# DM SYNC API HELPERS
# ============================================================================

# Send a DM sync update (CRDT delta) to a single peer
# Usage: api_dm_sync_update <node> <recipient_hex> <data_hex>
api_dm_sync_update() {
    local n=$1
    local recipient=$2
    local data_hex=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/dm/sync/update" \
        -H "Content-Type: application/json" \
        -d "{\"recipient\":\"$recipient\",\"data\":\"$data_hex\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/dm/sync/update"
    echo "$result"
}

# Request DM sync state from a peer
# Usage: api_dm_sync_request <node> <recipient_hex>
api_dm_sync_request() {
    local n=$1
    local recipient=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/dm/sync/request" \
        -H "Content-Type: application/json" \
        -d "{\"recipient\":\"$recipient\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/dm/sync/request"
    echo "$result"
}

# Respond to a DM sync request with full state (direct SYNC_ALPN)
# Usage: api_dm_sync_respond <node> <recipient_hex> <data_hex>
api_dm_sync_respond() {
    local n=$1
    local recipient=$2
    local data_hex=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/dm/sync/respond" \
        -H "Content-Type: application/json" \
        -d "{\"recipient\":\"$recipient\",\"data\":\"$data_hex\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/dm/sync/respond"
    echo "$result"
}

# Share a file via DM (peer-to-peer)
# Usage: api_dm_share <node> <recipient_hex> <file_path>
# Returns: JSON with hash
api_dm_share() {
    local n=$1
    local recipient=$2
    local file_path=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/dm/share" \
        -H "Content-Type: application/json" \
        -d "{\"recipient\":\"$recipient\",\"file_path\":\"$file_path\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/dm/share"
    echo "$result"
}

# Get a node's endpoint ID
# Usage: api_get_endpoint_id <node>
# Returns: 64-char hex endpoint ID
api_get_endpoint_id() {
    local n=$1
    local stats=$(api_stats $n)
    echo "$stats" | grep -o '"endpoint_id":"[^"]*"' | cut -d'"' -f4
}

# ============================================================================
# STREAM API HELPERS
# ============================================================================

# Request a stream to a peer
# Usage: api_stream_request <node> <topic_hex> <peer_hex> [name]
# Returns: JSON with request_id
api_stream_request() {
    local n=$1
    local topic=$2
    local peer=$3
    local name=${4:-"test"}
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/stream/request" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\",\"peer\":\"$peer\",\"name\":\"$name\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/stream/request"
    echo "$result"
}

# Request a DM stream to a peer (peer-to-peer, no topic)
# Usage: api_dm_stream_request <node> <peer_hex> [name]
# Returns: JSON with request_id
api_dm_stream_request() {
    local n=$1
    local peer=$2
    local name=${3:-"test"}
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/dm/stream/request" \
        -H "Content-Type: application/json" \
        -d "{\"peer\":\"$peer\",\"name\":\"$name\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/dm/stream/request"
    echo "$result"
}

# Accept a stream request
# Usage: api_stream_accept <node> <request_id_hex>
api_stream_accept() {
    local n=$1
    local request_id=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/stream/accept" \
        -H "Content-Type: application/json" \
        -d "{\"request_id\":\"$request_id\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/stream/accept"
    echo "$result"
}

# Reject a stream request
# Usage: api_stream_reject <node> <request_id_hex> [reason]
api_stream_reject() {
    local n=$1
    local request_id=$2
    local reason=${3:-""}
    local port=$(api_port $n)
    local body="{\"request_id\":\"$request_id\""
    if [ -n "$reason" ]; then
        body="$body,\"reason\":\"$reason\""
    fi
    body="$body}"
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/stream/reject" \
        -H "Content-Type: application/json" \
        -d "$body")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/stream/reject"
    echo "$result"
}

# Publish data to an active stream
# Usage: api_stream_publish <node> <request_id_hex> <data_hex>
api_stream_publish() {
    local n=$1
    local request_id=$2
    local data_hex=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/stream/publish" \
        -H "Content-Type: application/json" \
        -d "{\"request_id\":\"$request_id\",\"data\":\"$data_hex\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/stream/publish"
    echo "$result"
}

# End an active stream
# Usage: api_stream_end <node> <request_id_hex>
api_stream_end() {
    local n=$1
    local request_id=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/stream/end" \
        -H "Content-Type: application/json" \
        -d "{\"request_id\":\"$request_id\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/stream/end"
    echo "$result"
}

# Extract request_id from stream request response JSON
# Usage: extract_request_id <json>
extract_request_id() {
    local json=$1
    echo "$json" | grep -o '"request_id":"[^"]*"' | cut -d'"' -f4
}

# ============================================================================
# CONTROL API HELPERS
# ============================================================================

# Request a connection to a peer
# Usage: api_control_connect <node> <peer_hex> [relay_url] [display_name] [token_hex]
api_control_connect() {
    local n=$1
    local peer=$2
    local relay_url=${3:-}
    local display_name=${4:-}
    local token=${5:-}
    local port=$(api_port $n)

    local body="{\"peer\":\"$peer\""
    [ -n "$relay_url" ] && body="$body,\"relay_url\":\"$relay_url\""
    [ -n "$display_name" ] && body="$body,\"display_name\":\"$display_name\""
    [ -n "$token" ] && body="$body,\"token\":\"$token\""
    body="$body}"

    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/connect" \
        -H "Content-Type: application/json" \
        -d "$body")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/connect"
    echo "$result"
}

# Accept a pending connection request
# Usage: api_control_accept <node> <request_id_hex>
api_control_accept() {
    local n=$1
    local request_id=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/accept" \
        -H "Content-Type: application/json" \
        -d "{\"request_id\":\"$request_id\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/accept"
    echo "$result"
}

# Decline a pending connection request
# Usage: api_control_decline <node> <request_id_hex> [reason]
api_control_decline() {
    local n=$1
    local request_id=$2
    local reason=${3:-}
    local port=$(api_port $n)
    local body="{\"request_id\":\"$request_id\""
    [ -n "$reason" ] && body="$body,\"reason\":\"$reason\""
    body="$body}"
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/decline" \
        -H "Content-Type: application/json" \
        -d "$body")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/decline"
    echo "$result"
}

# Generate a connect invite (for QR code / invite string)
# Usage: api_control_generate_invite <node>
# Returns: JSON with endpoint_id, token, relay_url, invite
api_control_generate_invite() {
    local n=$1
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/invite")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/invite"
    echo "$result"
}

# Connect using an invite string
# Usage: api_control_connect_with_invite <node> <invite_string>
api_control_connect_with_invite() {
    local n=$1
    local invite=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/connect-with-invite" \
        -H "Content-Type: application/json" \
        -d "{\"invite\":\"$invite\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/connect-with-invite"
    echo "$result"
}

# Block a peer
# Usage: api_control_block <node> <peer_hex>
api_control_block() {
    local n=$1
    local peer=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/block" \
        -H "Content-Type: application/json" \
        -d "{\"peer\":\"$peer\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/block"
    echo "$result"
}

# Unblock a peer
# Usage: api_control_unblock <node> <peer_hex>
api_control_unblock() {
    local n=$1
    local peer=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/unblock" \
        -H "Content-Type: application/json" \
        -d "{\"peer\":\"$peer\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/unblock"
    echo "$result"
}

# List all connections
# Usage: api_control_list_connections <node>
api_control_list_connections() {
    local n=$1
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/control/connections")
    local exit_code=$?
    log_api_error $exit_code $n "GET /api/control/connections"
    echo "$result"
}

# Invite a peer to a topic
# Usage: api_control_topic_invite <node> <peer_hex> <topic_hex>
api_control_topic_invite() {
    local n=$1
    local peer=$2
    local topic=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/topic-invite" \
        -H "Content-Type: application/json" \
        -d "{\"peer\":\"$peer\",\"topic\":\"$topic\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/topic-invite"
    echo "$result"
}

# Accept a topic invitation
# Usage: api_control_topic_accept <node> <message_id_hex>
api_control_topic_accept() {
    local n=$1
    local message_id=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/topic-accept" \
        -H "Content-Type: application/json" \
        -d "{\"message_id\":\"$message_id\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/topic-accept"
    echo "$result"
}

# Decline a topic invitation
# Usage: api_control_topic_decline <node> <message_id_hex>
api_control_topic_decline() {
    local n=$1
    local message_id=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/topic-decline" \
        -H "Content-Type: application/json" \
        -d "{\"message_id\":\"$message_id\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/topic-decline"
    echo "$result"
}

# Leave a topic via Control protocol
# Usage: api_control_leave <node> <topic_hex>
api_control_leave() {
    local n=$1
    local topic=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/leave" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/leave"
    echo "$result"
}

# Remove a member from a topic (admin only)
# Usage: api_control_remove_member <node> <topic_hex> <member_hex>
api_control_remove_member() {
    local n=$1
    local topic=$2
    local member=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/remove-member" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\",\"member\":\"$member\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/remove-member"
    echo "$result"
}

# Suggest a peer introduction
# Usage: api_control_suggest <node> <to_peer_hex> <suggested_peer_hex> [note]
api_control_suggest() {
    local n=$1
    local to_peer=$2
    local suggested_peer=$3
    local note=${4:-}
    local port=$(api_port $n)
    local body="{\"to_peer\":\"$to_peer\",\"suggested_peer\":\"$suggested_peer\""
    [ -n "$note" ] && body="$body,\"note\":\"$note\""
    body="$body}"
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/control/suggest" \
        -H "Content-Type: application/json" \
        -d "$body")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/control/suggest"
    echo "$result"
}

# List pending topic invites
# Usage: api_control_pending_invites <node>
api_control_pending_invites() {
    local n=$1
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/control/pending-invites")
    local exit_code=$?
    log_api_error $exit_code $n "GET /api/control/pending-invites"
    echo "$result"
}

# Extract invite string from generate_invite response
# Usage: extract_invite_string <json>
extract_invite_string() {
    local json=$1
    echo "$json" | grep -o '"invite":"[^"]*"' | cut -d'"' -f4
}

# Extract message_id from topic invite response
# Usage: extract_message_id <json>
extract_message_id() {
    local json=$1
    echo "$json" | grep -o '"message_id":"[^"]*"' | cut -d'"' -f4
}

