const { spawn } = require('child_process');
const path = require('path');

// 切换到项目根目录
process.chdir(path.resolve(__dirname, '..'));

// 使用 spawn 调用 cargo，确保 argv[0] 是 'cargo'
const cargo = spawn('cargo', ['run', '--bin', 'silences-server'], {
    stdio: 'inherit',
    shell: true,  // 在 Windows 上可能需要
    windowsHide: true,
});

cargo.on('close', (code) => {
    process.exit(code);
});