@echo off
chcp 65001 >nul
title Taobao RU translator
cd /d "%~dp0"
"%~dp0app\target\release\taobao-tr.exe"
if errorlevel 1 pause
