# 重启前端开发服务器（非阻塞）
# 启动后立即返回，新服务在独立窗口中运行

$WebDir = "E:\programs\Silences\web"
$Port = 1204

# 杀掉占用端口的旧进程
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

# 新窗口启动 dev server（非阻塞）
Start-Process powershell -WorkingDirectory $WebDir -ArgumentList @(
    "-NoExit",
    "-Command",
    "npm run dev"
)

Write-Host "[restart] 前端服务器已在新窗口启动 (端口 $Port)"
