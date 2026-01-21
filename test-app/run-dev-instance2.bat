@echo off
REM Run test-app instance 2 in dev mode with custom database

cd /d "%~dp0"

set HARBOR_DB_PATH=.\test-data\instance2\harbor.db
set VITE_PORT=1422

if not exist ".\test-data\instance2" mkdir ".\test-data\instance2"

echo ==========================================
echo Harbor Test App - Instance 2
echo ==========================================
echo Database: %HARBOR_DB_PATH%
echo Dev server port: %VITE_PORT%
echo.

npx tauri dev
