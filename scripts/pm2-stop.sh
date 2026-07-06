cd "$(dirname "$0")/.."
pm2 stop ecosystem.config.js
echo "已停止。要重启执行: pm2 start ecosystem.config.js"
