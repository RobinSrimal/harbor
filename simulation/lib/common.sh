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

