# B2: 验证 Silences.md 规范 "禁止 2>&1，pwsh 不支持"
Write-Host "=== B2: 扫描 2>&1 违规 ==="
Write-Host ""

$violations = Get-ChildItem "E:\programs\Silences" -Recurse -File |
    Where-Object {
        $_.FullName -notmatch '\\target\\' -and
        $_.FullName -notmatch '\\.git\\' -and
        $_.FullName -notmatch '\\node_modules\\' -and
        $_.FullName -notmatch '\\.trash\\' -and
        $_.Extension -notin '.db', '.lock', '.tsbuildinfo'
    } |
    Select-String -Pattern "2>&1" -SimpleMatch

if ($violations.Count -eq 0) {
    Write-Host "通过：未发现 2>&1 使用" -ForegroundColor Green
} else {
    Write-Host "违规位置：" -ForegroundColor Yellow
    $violations | ForEach-Object {
        Write-Host "  $($_.Path):$($_.LineNumber)  ->  $($_.Line.Trim())" -ForegroundColor Red
    }
    Write-Host ""
    Write-Host "说明：pwsh 不支持 cmd 的 2>&1 重定向语法。" -ForegroundColor Cyan
    Write-Host "      pwsh 中应使用 `$null 2>&1 或 *>` 替代。" -ForegroundColor Cyan
}

Write-Host ""
Write-Host "=== B2 完成 ==="
