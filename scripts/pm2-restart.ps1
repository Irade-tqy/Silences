# pm2-restart.ps1 — 用 PM2 重启 Silences 前后端
# 等价于 pm2-restart.sh

$ProjectRoot = Split-Path $PSScriptRoot -Parent
Set-Location $ProjectRoot

pm2 restart ecosystem.config.js
