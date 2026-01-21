@echo off
REM Run test-app instance 1 in dev mode with custom database

cd /d "%~dp0"

set HARBOR_DB_PATH=.\test-data\instance1\harbor.db
set VITE_PORT=1420

if not exist ".\test-data\instance1" mkdir ".\test-data\instance1"

echo ==========================================
echo Harbor Test App - Instance 1
echo ==========================================
echo Database: %HARBOR_DB_PATH%
echo Dev server port: %VITE_PORT%
echo.

npx tauri dev
