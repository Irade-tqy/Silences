# pm2-stop.ps1 — 用 PM2 停止 Silences 前后端
# 等价于 pm2-stop.sh

$ProjectRoot = Split-Path $PSScriptRoot -Parent
Set-Location $ProjectRoot

pm2 stop ecosystem.config.js
Write-Host "已停止。要重启执行: pm2 start ecosystem.config.js"
