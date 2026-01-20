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

api_refresh_members() {
    local n=$1
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/refresh_members")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/refresh_members"
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
# SYNC API HELPERS (CRDT collaboration)
# ============================================================================

# Enable sync for a topic
# Usage: api_sync_enable <node> <topic_hex>
api_sync_enable() {
    local n=$1
    local topic=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/sync/enable" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/sync/enable"
    echo "$result"
}

# Insert text into a sync document
# Usage: api_sync_text_insert <node> <topic_hex> <container> <index> <text>
api_sync_text_insert() {
    local n=$1
    local topic=$2
    local container=$3
    local index=$4
    local text=$5
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/sync/text/insert" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\",\"container\":\"$container\",\"index\":$index,\"text\":\"$text\"}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/sync/text/insert"
    echo "$result"
}

# Delete text from a sync document
# Usage: api_sync_text_delete <node> <topic_hex> <container> <index> <len>
api_sync_text_delete() {
    local n=$1
    local topic=$2
    local container=$3
    local index=$4
    local len=$5
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 30 -X POST "http://127.0.0.1:$port/api/sync/text/delete" \
        -H "Content-Type: application/json" \
        -d "{\"topic\":\"$topic\",\"container\":\"$container\",\"index\":$index,\"len\":$len}")
    local exit_code=$?
    log_api_error $exit_code $n "POST /api/sync/text/delete"
    echo "$result"
}

# Get text from a sync document
# Usage: api_sync_get_text <node> <topic_hex> <container>
# Returns: JSON with text field
api_sync_get_text() {
    local n=$1
    local topic=$2
    local container=$3
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/sync/text/$topic/$container")
    local exit_code=$?
    log_api_error $exit_code $n "GET /api/sync/text"
    echo "$result"
}

# Get sync status for a topic
# Usage: api_sync_status <node> <topic_hex>
# Returns: JSON with enabled, has_pending_changes, version
api_sync_status() {
    local n=$1
    local topic=$2
    local port=$(api_port $n)
    local result
    result=$(curl -s --connect-timeout 5 --max-time 10 "http://127.0.0.1:$port/api/sync/status/$topic")
    local exit_code=$?
    log_api_error $exit_code $n "GET /api/sync/status"
    echo "$result"
}

# Extract text value from sync API response
# Usage: extract_sync_text <json_response>
extract_sync_text() {
    local json=$1
    echo "$json" | grep -o '"text":"[^"]*"' | cut -d'"' -f4
}

