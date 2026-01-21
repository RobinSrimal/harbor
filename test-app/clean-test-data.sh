#!/bin/bash
# Clean test data directories to reset instances with fresh identities

cd "$(dirname "$0")"

echo "Cleaning test data directories..."
echo "This will delete all database files and logs, giving each instance a fresh identity."
echo ""

rm -rf ./src-tauri/test-data/instance1/*
rm -rf ./src-tauri/test-data/instance2/*

mkdir -p ./src-tauri/test-data/instance1
mkdir -p ./src-tauri/test-data/instance2

echo "✓ Test data cleaned"
echo ""
echo "Now you can run both instances and they will have different endpoint IDs:"
echo "  ./run-dev-instance1.sh"
echo "  ./run-dev-instance2.sh"
