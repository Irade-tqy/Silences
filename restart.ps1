# restart.ps1 — 重启 Silences 前后端（零停机）
# 先构建 → 启动新进程 → 再杀旧进程，避免自调用时自我终结
# 使用场景：Silences 的 command tool 调用此脚本完成自我更新重启

$ProjectRoot = "E:\programs\Silences"
$BackendPort = 1030
$FrontendPort = 1204
$BackendBin = "$ProjectRoot\target\release\silences-server.exe"

Write-Host "`n[restart] ======== 1/4 构建后端 ========"
cargo build --release -p silences-server
if ($LASTEXITCODE -ne 0) {
    Write-Host "[restart] 后端构建失败，退出"
    exit 1
}

Write-Host "`n[restart] ======== 2/4 构建前端 ========"
Push-Location "$ProjectRoot\web"
npm run build
if ($LASTEXITCODE -ne 0) {
    Write-Host "[restart] 前端构建失败，退出"
    Pop-Location
    exit 1
}
Pop-Location

Write-Host "`n[restart] ======== 3/4 启动新进程 ========"

# 启动后端（新窗口）
Start-Process powershell -WorkingDirectory $ProjectRoot -WindowStyle Normal -ArgumentList @(
    "-NoExit", "-Command", ".\target\release\silences-server.exe"
)
Write-Host "[restart] 后端新进程已启动"

# 启动前端（新窗口）
Start-Process powershell -WorkingDirectory "$ProjectRoot\web" -WindowStyle Normal -ArgumentList @(
    "-NoExit", "-Command", "npm run start"
)
Write-Host "[restart] 前端新进程已启动"

Write-Host "[restart] 等待新进程绑定端口..."
Start-Sleep -Seconds 12

Write-Host "`n[restart] ======== 4/4 清理旧进程 ========"

function Kill-ProcessesOnPort($Port, $Label) {
    $conns = netstat -ano | Select-String ":$Port\s"
    if (-not $conns) {
        Write-Host "[restart] $Label 端口 $Port 无旧进程"
        return
    }
    $seen = @{}
    $pids = $conns | ForEach-Object { ($_ -split '\s+')[-1] } | Select-Object -Unique
    foreach ($pid in $pids) {
        if ($seen.ContainsKey($pid)) { continue }
        $seen[$pid] = $true
        $proc = Get-Process -Id $pid -ErrorAction SilentlyContinue
        if ($proc -and $proc.ProcessName -ne 'System') {
            Write-Host "[restart] 杀掉 $Label 旧进程: $pid ($($proc.ProcessName))"
            Stop-Process -Id $pid -Force
        }
    }
}

Kill-ProcessesOnPort $BackendPort "后端"
Kill-ProcessesOnPort $FrontendPort "前端"

Write-Host "`n[restart] ======== 完成 ========"
Write-Host "[restart] 后端 -> http://localhost:$BackendPort"
Write-Host "[restart] 前端 -> http://localhost:$FrontendPort"
