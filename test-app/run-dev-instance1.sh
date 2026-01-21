#!/bin/bash

# Run test-app instance 1 in dev mode with custom database

cd "$(dirname "$0")"

export HARBOR_DB_PATH="./test-data/instance1/harbor.db"
export VITE_PORT=1420

mkdir -p ./test-data/instance1

echo "=========================================="
echo "Harbor Test App - Instance 1"
echo "=========================================="
echo "Database: $HARBOR_DB_PATH"
echo "Dev server port: $VITE_PORT"
echo ""

npx tauri dev
