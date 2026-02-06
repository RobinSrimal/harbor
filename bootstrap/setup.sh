#!/bin/bash
#
# Bootstrap Node Production Setup
#
# Sets up N bootstrap nodes as systemd services with persistent data.
# The nodes spun up here ARE your production bootstrap nodes.
#
# Usage: sudo ./bootstrap_setup.sh [num_nodes] [start_port]
#
# Prerequisites:
#   - Harbor binary built and copied to /usr/local/bin/harbor
#   - Running as root (for systemd setup)
#   - jq installed: apt install jq
#

set -e

# Configuration
NUM_NODES=${1:-20}
START_PORT=${2:-3001}
HARBOR_BINARY="/usr/local/bin/harbor"
DATA_BASE_DIR="/var/lib/harbor"
SERVICE_USER="harbor"
OUTPUT_DIR="./bootstrap_output"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Check prerequisites
check_prerequisites() {
    if [ "$EUID" -ne 0 ]; then
        log_error "Please run as root (sudo ./bootstrap_setup.sh)"
        exit 1
    fi

    if [ ! -f "$HARBOR_BINARY" ]; then
        log_error "Harbor binary not found at $HARBOR_BINARY"
        log_info "Build and copy it first:"
        log_info "  cd core && cargo build --release"
        log_info "  sudo cp target/release/harbor /usr/local/bin/"
        exit 1
    fi

    if ! command -v jq &> /dev/null; then
        log_error "jq is required but not installed"
        log_info "Install it: apt install jq"
        exit 1
    fi

    if ! command -v curl &> /dev/null; then
        log_error "curl is required but not installed"
        exit 1
    fi
}

# Create harbor user if it doesn't exist
setup_user() {
    if ! id "$SERVICE_USER" &>/dev/null; then
        log_info "Creating user: $SERVICE_USER"
        useradd --system --no-create-home --shell /bin/false "$SERVICE_USER"
    fi
}

# Create systemd service file for a node
create_service_file() {
    local node_num=$1
    local port=$2
    local data_dir="$DATA_BASE_DIR/bootstrap-$node_num"
    local service_name="harbor-bootstrap-$node_num"
    local service_file="/etc/systemd/system/${service_name}.service"

    # First node starts without bootstrap, others bootstrap from first
    local bootstrap_flag=""
    if [ "$node_num" -eq 1 ]; then
        bootstrap_flag="--no-default-bootstrap"
    else
        # Will be updated after first node is running
        bootstrap_flag="--no-default-bootstrap"
    fi

    cat > "$service_file" << EOF
[Unit]
Description=Harbor Bootstrap Node $node_num
After=network.target
Wants=network-online.target

[Service]
Type=simple
User=$SERVICE_USER
ExecStart=$HARBOR_BINARY --port $port --data-dir $data_dir $bootstrap_flag
Restart=always
RestartSec=5

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=$data_dir
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF

    log_info "Created service: $service_name"
}

# Update service file with bootstrap arg (for nodes 2+)
update_service_with_bootstrap() {
    local node_num=$1
    local bootstrap_arg=$2
    local service_name="harbor-bootstrap-$node_num"
    local service_file="/etc/systemd/system/${service_name}.service"
    local port=$((START_PORT + node_num - 1))
    local data_dir="$DATA_BASE_DIR/bootstrap-$node_num"

    cat > "$service_file" << EOF
[Unit]
Description=Harbor Bootstrap Node $node_num
After=network.target
Wants=network-online.target

[Service]
Type=simple
User=$SERVICE_USER
ExecStart=$HARBOR_BINARY --port $port --data-dir $data_dir --bootstrap $bootstrap_arg
Restart=always
RestartSec=5

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=$data_dir
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF
}

# Wait for node to be ready and get bootstrap info
get_bootstrap_info() {
    local port=$1
    local max_attempts=30
    local attempt=0

    while [ $attempt -lt $max_attempts ]; do
        if curl -s "http://localhost:$port/api/bootstrap" > /dev/null 2>&1; then
            curl -s "http://localhost:$port/api/bootstrap"
            return 0
        fi
        sleep 1
        attempt=$((attempt + 1))
    done

    log_error "Node on port $port did not become ready"
    return 1
}

# Main execution
main() {
    log_info "Harbor Bootstrap Node Production Setup"
    log_info "======================================="
    log_info "Nodes to create: $NUM_NODES"
    log_info "Port range: $START_PORT - $((START_PORT + NUM_NODES - 1))"
    log_info "Data directory: $DATA_BASE_DIR"
    log_info "Binary: $HARBOR_BINARY"
    log_info ""

    check_prerequisites
    setup_user

    # Prompt for operator name
    read -p "Enter operator/organization name (e.g., 'harbor', 'alice'): " OPERATOR_NAME
    OPERATOR_NAME=${OPERATOR_NAME:-"operator"}
    log_info "Operator: $OPERATOR_NAME"
    log_info ""

    # Create base data directory
    mkdir -p "$DATA_BASE_DIR"
    chown "$SERVICE_USER:$SERVICE_USER" "$DATA_BASE_DIR"

    # Create output directory for generated files
    mkdir -p "$OUTPUT_DIR"

    # Arrays to store results
    declare -a BOOTSTRAP_ARGS

    # Step 1: Create data directories and service files
    log_info "Step 1: Creating data directories and service files..."
    for i in $(seq 1 $NUM_NODES); do
        port=$((START_PORT + i - 1))
        data_dir="$DATA_BASE_DIR/bootstrap-$i"

        mkdir -p "$data_dir"
        chown "$SERVICE_USER:$SERVICE_USER" "$data_dir"

        create_service_file $i $port
    done

    systemctl daemon-reload

    # Step 2: Start first node and get its bootstrap info
    log_info ""
    log_info "Step 2: Starting node 1 (bootstrap seed)..."
    systemctl start harbor-bootstrap-1
    systemctl enable harbor-bootstrap-1

    log_info "Waiting for node 1 to be ready..."
    FIRST_NODE_INFO=$(get_bootstrap_info $START_PORT)
    FIRST_BOOTSTRAP_ARG=$(echo "$FIRST_NODE_INFO" | jq -r '.bootstrap_arg')
    BOOTSTRAP_ARGS+=("$FIRST_BOOTSTRAP_ARG")
    log_info "Node 1 ready: $FIRST_BOOTSTRAP_ARG"

    # Step 3: Update remaining service files with bootstrap arg and start them
    log_info ""
    log_info "Step 3: Configuring and starting nodes 2-$NUM_NODES..."
    for i in $(seq 2 $NUM_NODES); do
        update_service_with_bootstrap $i "$FIRST_BOOTSTRAP_ARG"
    done

    systemctl daemon-reload

    for i in $(seq 2 $NUM_NODES); do
        systemctl start "harbor-bootstrap-$i"
        systemctl enable "harbor-bootstrap-$i"
        log_info "Started harbor-bootstrap-$i"
    done

    # Step 4: Wait for all nodes and collect bootstrap info
    log_info ""
    log_info "Step 4: Waiting for all nodes to initialize..."
    sleep 5

    log_info "Collecting bootstrap info from all nodes..."
    for i in $(seq 2 $NUM_NODES); do
        port=$((START_PORT + i - 1))
        info=$(get_bootstrap_info $port)
        if [ $? -eq 0 ]; then
            bootstrap_arg=$(echo "$info" | jq -r '.bootstrap_arg')
            BOOTSTRAP_ARGS+=("$bootstrap_arg")
            log_info "Node $i: collected"
        else
            log_error "Failed to get info from node $i"
        fi
    done

    # Step 5: Generate output files
    log_info ""
    log_info "Step 5: Generating output files..."

    # Save raw list
    OUTPUT_FILE="$OUTPUT_DIR/bootstrap_list.txt"
    printf '%s\n' "${BOOTSTRAP_ARGS[@]}" > "$OUTPUT_FILE"
    log_info "Raw list saved to: $OUTPUT_FILE"

    # Generate Rust code entries
    RUST_FILE="$OUTPUT_DIR/bootstrap_entries.rs"

    cat > "$RUST_FILE" << EOF
// Bootstrap nodes contributed by: $OPERATOR_NAME
// Generated: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
// Add these entries to the vec![] in default_bootstrap_nodes()

EOF

    node_num=1
    for arg in "${BOOTSTRAP_ARGS[@]}"; do
        endpoint_id=$(echo "$arg" | cut -d':' -f1)
        relay_url=$(echo "$arg" | cut -d':' -f2-)

        cat >> "$RUST_FILE" << EOF
        BootstrapNode::from_hex_with_relay(
            "$endpoint_id",
            Some("${OPERATOR_NAME}-bootstrap-$node_num"),
            "$relay_url",
        ),
EOF
        node_num=$((node_num + 1))
    done

    log_info "Rust entries saved to: $RUST_FILE"

    # Summary
    log_info ""
    log_info "======================================="
    log_info "SETUP COMPLETE"
    log_info "======================================="
    log_info ""
    log_info "Running services:"
    for i in $(seq 1 $NUM_NODES); do
        systemctl is-active --quiet "harbor-bootstrap-$i" && \
            log_info "  harbor-bootstrap-$i: active" || \
            log_warn "  harbor-bootstrap-$i: inactive"
    done
    log_info ""
    log_info "Data directories: $DATA_BASE_DIR/bootstrap-{1..$NUM_NODES}"
    log_info ""
    log_info "Useful commands:"
    log_info "  systemctl status harbor-bootstrap-1    # Check status"
    log_info "  journalctl -u harbor-bootstrap-1 -f    # View logs"
    log_info "  systemctl restart harbor-bootstrap-1   # Restart"
    log_info ""
    log_info "Next steps:"
    log_info "1. Copy entries from $RUST_FILE"
    log_info "2. Add to core/src/network/dht/internal/bootstrap.rs"
    log_info "3. Submit PR or commit changes"
    log_info ""
}

main "$@"
