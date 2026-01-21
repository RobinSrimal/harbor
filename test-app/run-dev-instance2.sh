#!/bin/bash

# Run test-app instance 2 in dev mode with custom database

cd "$(dirname "$0")"

export HARBOR_DB_PATH="./test-data/instance2/harbor.db"
export VITE_PORT=1422

mkdir -p ./test-data/instance2

echo "=========================================="
echo "Harbor Test App - Instance 2"
echo "=========================================="
echo "Database: $HARBOR_DB_PATH"
echo "Dev server port: $VITE_PORT"
echo ""

npx tauri dev
