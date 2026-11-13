@echo off
chcp 65001 >nul
title Taobao RU
cd /d "%~dp0"
"%~dp0app\target\release\taobao-tr.exe" --no-local
if errorlevel 1 pause
