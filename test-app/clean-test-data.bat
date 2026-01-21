@echo off
REM Clean test data directories to reset instances with fresh identities

cd /d "%~dp0"

echo Cleaning test data directories...
echo This will delete all database files and logs, giving each instance a fresh identity.
echo.

if exist ".\src-tauri\test-data\instance1" rd /s /q ".\src-tauri\test-data\instance1"
if exist ".\src-tauri\test-data\instance2" rd /s /q ".\src-tauri\test-data\instance2"

mkdir ".\src-tauri\test-data\instance1"
mkdir ".\src-tauri\test-data\instance2"

echo.
echo ✓ Test data cleaned
echo.
echo Now you can run both instances and they will have different endpoint IDs:
echo   run-dev-instance1.bat
echo   run-dev-instance2.bat
