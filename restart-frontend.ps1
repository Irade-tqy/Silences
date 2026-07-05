# 重启前端开发服务器（非阻塞）
# 先启动新进程，再杀旧进程，避免自调用时自我终结

$WebDir = "E:\programs\Silences\web"
$Port = 1204

# 第一步：先启动新窗口的 dev server
Start-Process powershell -WorkingDirectory $WebDir -ArgumentList @(
    "-NoExit",
    "-Command",
    "npm run dev"
)

Write-Host "[restart] 新窗口已启动，等待就绪..."

# 第二步：等新进程绑定端口后，再杀旧进程
Start-Sleep -Seconds 3

$connections = netstat -ano | Select-String ":$Port\s"
if ($connections) {
    $pids = $connections | ForEach-Object {
        ($_ -split '\s+')[-1]
    } | Select-Object -Unique

    foreach ($pid in $pids) {
        $proc = Get-Process -Id $pid -ErrorAction SilentlyContinue
        if ($proc -and $proc.ProcessName -ne 'System' -and $proc.ProcessName -ne 'Idle') {
            Write-Host "[restart] 杀掉旧进程: $pid ($($proc.ProcessName))"
            Stop-Process -Id $pid -Force
        }
    }
}

Write-Host "[restart] 前端服务器已重启完毕 (端口 $Port)"
