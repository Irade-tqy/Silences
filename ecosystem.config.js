// PM2 进程管理配置 — Silences 前后端
// 启动： pm2 start ecosystem.config.js
// 重启： pm2 restart ecosystem.config.js
// 状态： pm2 status
// 日志： pm2 logs

module.exports = {
  apps: [
    {
      name: 'silences-backend',
      script: 'scripts/start-backend.js',  // 指向新的 JS 脚本
      interpreter: 'node',                  // 使用 node 执行
      cwd: __dirname,
      autorestart: true,
      max_restarts: 10,
      kill_timeout: 10_000,
      log_date_format: 'YYYY-MM-DD HH:mm:ss',
      min_uptime: 3000
    },
    {
      name: 'silences-frontend',
      script: 'node',
      args: ['scripts/start-frontend.js'],
      cwd: __dirname,
      autorestart: true,
      max_restarts: 10,
      kill_timeout: 10_000,
      log_date_format: 'YYYY-MM-DD HH:mm:ss',
      min_uptime: 3000,
    },
  ],
}