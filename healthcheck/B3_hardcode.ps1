# B3: 检查项目中是否存在硬编码字符串和有意义的常量
# Silences.md 规范：禁止硬编码字符串（如提示词、文档）和有意义的常量（如超时时间、重试次数）
# 如果没有合适的位置，至少创建一个统一 env.json 配置文件

$root = "E:\programs\Silences"
$skip = @('.git', 'target', 'node_modules', '.trash', 'silences.db')

Write-Host "=== B3: 硬编码字符串/常量检查 ===" -ForegroundColor Cyan

# 记录发现
$violations = @()

# 检查是否已存在 env.json
$envJson = Join-Path $root "env.json"
if (Test-Path $envJson) {
    Write-Host "[INFO] 存在 env.json 配置文件" -ForegroundColor Green
} else {
    Write-Host "[WARN] 不存在统一的 env.json 配置文件" -ForegroundColor Yellow
}

# --- 1. 检查 tool 描述（文档/提示词）是否硬编码 ---
Write-Host "`n--- 1. Tool 描述文档（应抽离到配置文件）---" -ForegroundColor Yellow
$toolFiles = @(
    "crates\silences-agent\src\toolcall\edit.rs",
    "crates\silences-agent\src\toolcall\grep.rs",
    "crates\silences-agent\src\toolcall\read.rs",
    "crates\silences-agent\src\toolcall\raw_read.rs",
    "crates\silences-agent\src\toolcall\write.rs",
    "crates\silences-agent\src\toolcall\replace.rs",
    "crates\silences-agent\src\toolcall\raw_edit.rs",
    "crates\silences-agent\src\toolcall\find.rs",
    "crates\silences-agent\src\toolcall\glance.rs",
    "crates\silences-agent\src\toolcall\command.rs",
    "crates\silences-agent\src\toolcall\trash.rs",
    "crates\silences-agent\src\toolcall\regret.rs",
    "crates\silences-agent\src\toolcall\add_task.rs",
    "crates\silences-agent\src\toolcall\start_task.rs",
    "crates\silences-agent\src\toolcall\end_task.rs"
)

foreach ($f in $toolFiles) {
    $path = Join-Path $root $f
    if (Test-Path $path) {
        $content = Get-Content $path -Raw
        # 提取 description: 后的字符串（多行字符串）
        if ($content -match 'description:\s*"[^"]{100,}') {
            $match = $matches[0]
            $len = $match.Length - 14  # 去掉 "description: "
            $violations += "硬编码 tool 描述: $f ($len 字符)"
        }
    }
}

# --- 2. 检查有意义的常量 ---
Write-Host "`n--- 2. 有意义的常量（应抽离到配置）---" -ForegroundColor Yellow

# 2a. main.rs 中的 fallback 常量
$mainRs = Join-Path $root "crates\silences-server\src\main.rs"
$mainContent = Get-Content $mainRs -Raw

# check fallback model
if ($mainContent -match 'unwrap_or_else\(\|_\| "(deepseek-v4-flash)"\)') {
    $violations += "硬编码 fallback 模型名: 'deepseek-v4-flash' (main.rs)"
}

# check fallback bind
if ($mainContent -match 'unwrap_or_else\(\|_\| "(127\.0\.0\.1:0412)"\)') {
    $violations += "硬编码 fallback 绑定地址: '127.0.0.1:0412' (main.rs)"
}

# check fallback max_context
if ($mainContent -match 'unwrap_or\(50\)') {
    $violations += "硬编码 fallback max_context: 50 (main.rs)"
}

# check fallback db_path
if ($mainContent -match 'unwrap_or_else\(\|_\| "silences\.db"\)') {
    $violations += "硬编码 fallback 数据库路径: 'silences.db' (main.rs)"
}

# check fallback tokenizer path
if ($mainContent -match 'unwrap_or_else\(\|_\| "tokenizer/tokenizer\.json"\)') {
    $violations += "硬编码 fallback tokenizer 路径: 'tokenizer/tokenizer.json' (main.rs)"
}

# 2b. server lib.rs 中的 warmup 常量
$libRs = Join-Path $root "crates\silences-server\src\lib.rs"
$libContent = Get-Content $libRs -Raw

if ($libContent -match 'Duration::from_secs\(2\)') {
    $violations += "硬编码 warmup 等待时间: 2 秒 (lib.rs)"
}

# 2c. read.rs / raw_read.rs 中的 truncation 常量
$readRs = Join-Path $root "crates\silences-agent\src\toolcall\read.rs"
$readContent = Get-Content $readRs -Raw
if ($readContent -match 'auto_truncate\(&content,\s*2000,\s*1500,\s*500\)') {
    $violations += "硬编码 truncation 常量: 2000, 1500, 500 (read.rs)"
}

$rawReadRs = Join-Path $root "crates\silences-agent\src\toolcall\raw_read.rs"
$rawReadContent = Get-Content $rawReadRs -Raw
if ($rawReadContent -match 'auto_truncate\(&content,\s*2000,\s*1500,\s*500\)') {
    $violations += "硬编码 truncation 常量: 2000, 1500, 500 (raw_read.rs)"
}

# 2d. command.rs 中的 stderr truncation
$commandRs = Join-Path $root "crates\silences-agent\src\toolcall\command.rs"
$cmdContent = Get-Content $commandRs -Raw
if ($cmdContent -match 'truncate\(&stderr,\s*2000\)') {
    $violations += "硬编码 stderr truncation: 2000 (command.rs)"
}

# --- 3. 检查 .silences/prompt.md 是否存在（项目级提示词配置）---
Write-Host "`n--- 3. 项目级提示词配置文件 ---" -ForegroundColor Yellow
$promptMd = Join-Path $root ".silences\prompt.md"
if (Test-Path $promptMd) {
    Write-Host "[OK] 存在 .silences/prompt.md（项目级提示词）" -ForegroundColor Green
} else {
    $violations += "缺少 .silences/prompt.md（项目级提示词配置文件）"
}

# --- 输出结果 ---
Write-Host "`n=== 检查结果 ===" -ForegroundColor Cyan
if ($violations.Count -eq 0) {
    Write-Host "✅ 通过：未发现硬编码字符串/常量违规" -ForegroundColor Green
} else {
    Write-Host "⚠️  发现 $($violations.Count) 处违规:" -ForegroundColor Red
    $i = 1
    foreach ($v in $violations) {
        Write-Host "  $i. $v" -ForegroundColor Yellow
        $i++
    }
}

# --- 输出总结（简洁格式，供模型消费）---
Write-Host "`n=== 机器摘要 ===" -ForegroundColor Magenta
$summary = @{
    "test" = "B3-hardcode"
    "passed" = ($violations.Count -eq 0)
    "violations" = $violations.Count
    "details" = $violations
}
$summary | ConvertTo-Json -Compress
