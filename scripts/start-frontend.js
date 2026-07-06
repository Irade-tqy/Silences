// PM2 前端入口 — 先构建再启动，构建失败则退出让 PM2 重新尝试
const { execSync } = require('child_process')
const path = require('path')

const webDir = path.resolve(__dirname, '..', 'web')
process.chdir(webDir)

try {
  console.log('[frontend] building & starting...')
  execSync('npm run build && npm run start', { stdio: 'inherit', windowsHide: true })
} catch (err) {
  console.error('[frontend] 启动失败:', err.message)
  process.exit(1)
}
