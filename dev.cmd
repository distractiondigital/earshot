@echo off
rem Starts Earshot in development mode (hot reload, separate "Earshot Dev" data, no updater).
cd /d "%~dp0"
if not exist node_modules call npm ci
call npm run tauri:dev
if errorlevel 1 pause
