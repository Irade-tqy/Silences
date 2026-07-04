# B1: Verify 'py' is used instead of 'python' or 'python3'
# Rule: 执行用 py，禁止 python 和 python3

Write-Host "=== B1 Test: py vs python/python3 ==="

# Check py available
try {
    $ver = py --version 2>&1 | Out-String
    Write-Host "[PASS] py available: $($ver.Trim())"
} catch {
    Write-Host "[FAIL] py not available"
    exit 1
}

# Check config files for python/python3 usage
Write-Host "`n--- Checking project for python/python3 usage ---"
$root = "E:\programs\Silences"

$foundCmd = Select-String -Path "$root\Cargo.toml", "$root\package.json", "$root\web\package.json" -Pattern "python|python3"
if ($foundCmd) {
    Write-Host "[FAIL] Build config uses python/python3:"
    $foundCmd | ForEach-Object { Write-Host "       $($_.Path):$($_.LineNumber) $($_.Line.Trim())" }
} else {
    Write-Host "[PASS] Build config clean"
}

# Check scripts
$scripts = Get-ChildItem -Path $root -Recurse -Include "*.ps1", "*.sh", "*.bat", "*.cmd" -File
$violations = @()
foreach ($s in $scripts) {
    $content = Get-Content $s.FullName -Raw -ErrorAction SilentlyContinue
    if ($content -match "(?<!#!)(?<!#)(?<!#/usr/bin/env)python\d?\b") {
        $violations += $s.FullName
    }
}
if ($violations.Count -gt 0) {
    Write-Host "[FAIL] Scripts using python/python3 (non-shebang):"
    $violations | ForEach-Object { Write-Host "       $_" }
} else {
    Write-Host "[PASS] All scripts compliant"
}

Write-Host "`n=== B1 Complete ==="
exit 0
