//! replace — 在目录下所有文件中搜索并替换所有匹配（扩展名白名单保护）
//!
//! AI 必须指定 extensions 参数声明要搜索的文件扩展名，replace 不会猜测。
//! regex=true：全文正则匹配；regex=false：纯文本字面量匹配。
//! 安全兜底：始终跳过隐藏目录、node_modules、target、tokenizer、api_debug.json。
//! 自动标准化换行符和缩进。

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use fancy_regex::Regex;
use serde_json::Value;

use silences_core::ToolLimits;

use super::{normalize, read_file_robust, truncate_head_tok, InverseOp, ToolDef, ToolOutcome};

static REPLACE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn tool(console_dir: Option<PathBuf>, limits: ToolLimits) -> ToolDef {
    ToolDef {
        name: "replace",
        description:
            "在路径下所有文件中搜索并替换全部匹配。跳过隐藏目录、node_modules、target、tokenizer、api_debug.json。\nwhy: 批量重命名或重构[可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标文件或目录的绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "要替换的模式"
                },
                "replacement": {
                    "type": "string",
                    "description": "替换内容"
                },
                "extensions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "要搜索的文件扩展名，不含点号。如 [\"rs\",\"ts\",\"tsx\"]"
                },
                "regex": {
                    "type": "boolean",
                    "description": "true=启用正则（默认 false）"
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "true=仅预览匹配结果，不实际替换（默认 false）"
                }
            },
            "required": ["path", "pattern", "replacement", "extensions"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let cd = console_dir.clone();
            Box::pin(async move { execute(args, cd, limits).await })
        }),
    }
}

async fn execute(args: Value, console_dir: Option<PathBuf>, limits: ToolLimits) -> Result<ToolOutcome> {
    let path = args["path"].as_str().context("缺少 path 参数")?;
    let raw_pattern = args["pattern"].as_str().context("缺少 pattern 参数")?;
    let replacement = args["replacement"].as_str().context("缺少 replacement 参数")?;
    let use_regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
    let dry_run = args.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let extensions: HashSet<String> = args["extensions"]
        .as_array()
        .context("extensions 必须是数组")?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
        .collect();
    if extensions.is_empty() {
        anyhow::bail!("extensions 不能为空，请指定要搜索的文件扩展名，如 [\"rs\",\"ts\"]");
    }

    let meta = fs::metadata(path).context("路径不存在")?;

    let mut changed_files: Vec<(String, String)> = Vec::new();

    if meta.is_dir() {
        replace_in_dir(path, raw_pattern, replacement, use_regex, &extensions, &mut changed_files, dry_run)?;
    } else if use_regex {
        // 单文件：正则
        replace_in_file_regex(path, raw_pattern, replacement, &mut changed_files, dry_run)?;
    } else {
        // 单文件：字面量
        replace_in_file_literal(path, raw_pattern, replacement, &mut changed_files, dry_run)?;
    }

    if changed_files.is_empty() {
        // 匹配失败：提供反馈
        let feedback = if !meta.is_dir() {
            // 单文件：同 edit 的逻辑
            let content = read_file_robust(path).unwrap_or_default();
            let norm = normalize(&content);
            build_failure_feedback(
                path, &norm, raw_pattern, use_regex,
                limits.edit_context_lines, &console_dir,
            )
        } else {
            format!(
                "replace: 在 {} 中无匹配 (仅扩展名: {:?})。{}",
                path,
                extensions,
                if use_regex {
                    "当前为正则模式，如需纯文本匹配请设置 regex=false。"
                } else {
                    "请检查 pattern 是否正确。"
                }
            )
        };
        return Ok(ToolOutcome::new(feedback));
    }

    let file_list: Vec<String> = changed_files
        .iter()
        .map(|(p, _)| p.clone())
        .collect();

    if dry_run {
        let mut summary = format!(
            "[DRY RUN] 将在 {} 个文件中替换（共扫描 {} 个文件）:\n{}",
            changed_files.len(),
            changed_files.len(),
            file_list.join("\n"),
        );
        // 截断过长输出
        const DRY_RUN_TRUNCATE_TOK: usize = 800;
        let count = changed_files.len();
        let (truncated, was_truncated) = truncate_head_tok(&summary, DRY_RUN_TRUNCATE_TOK);
        if was_truncated {
            if let Some(ref cd) = console_dir {
                let _ = fs::create_dir_all(cd);
                let seq = REPLACE_COUNTER.fetch_add(1, Ordering::Relaxed);
                let out_path = cd.join(format!("replace_dry_run_{seq}.out"));
                let full = format!(
                    "replace dry-run (path: {})\npattern: {}\nreplacement: {}\n{} 个文件将被替换:\n{}\n",
                    path, raw_pattern, replacement, count, file_list.join("\n"),
                );
                let _ = fs::write(&out_path, &full);
                summary = format!(
                    "{}\n还有 {} 个文件未显示（共 {} 个），完整列表: {}",
                    truncated, count - file_list.len().min(1), count, out_path.display(),
                );
            }
        }
        return Ok(ToolOutcome::new(summary));
    }

    let files_owned = changed_files.clone();
    Ok(ToolOutcome {
        summary: format!(
            "批量替换完成 ({} 个文件):\n{}",
            changed_files.len(),
            file_list.join("\n")
        ),
        inverse: Some(InverseOp::new(
            format!("replace {} files", changed_files.len()),
            move || {
                for (path, original_content) in &files_owned {
                    std::fs::write(path, original_content)?;
                }
                Ok(format!("已恢复 {} 个文件", files_owned.len()))
            },
        )),
        rollback: false,
        approval_pending: None,
        inject_messages: vec![],
        defer_rollback: false,
    })
}

/// 单文件正则替换
fn replace_in_file_regex(
    path: &str,
    raw_pattern: &str,
    replacement: &str,
    changed: &mut Vec<(String, String)>,
    dry_run: bool,
) -> Result<()> {
    let re = Regex::new(raw_pattern).context("正则表达式无效")?;
    let original = match read_file_robust(path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    let content = normalize(&original);
    let new = replace_all_regex(&re, &content, replacement)?;
    if new != content {
        if !dry_run {
            fs::write(path, &new).context("写入文件失败")?;
        }
        changed.push((path.to_string(), original));
    }
    Ok(())
}

/// 单文件字面量替换（全部匹配）
fn replace_in_file_literal(
    path: &str,
    pattern: &str,
    replacement: &str,
    changed: &mut Vec<(String, String)>,
    dry_run: bool,
) -> Result<()> {
    let original = match read_file_robust(path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    let content = normalize(&original);
    let new = content.replace(pattern, replacement);
    if new != content {
        if !dry_run {
            fs::write(path, &new).context("写入文件失败")?;
        }
        changed.push((path.to_string(), original));
    }
    Ok(())
}

/// 目录递归替换
fn replace_in_dir(
    dir: &str,
    pattern: &str,
    replacement: &str,
    use_regex: bool,
    exts: &HashSet<String>,
    changed: &mut Vec<(String, String)>,
    dry_run: bool,
) -> Result<()> {
    for entry in fs::read_dir(dir).context("读取目录失败")? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            // 安全兜底：跳过隐藏目录、node_modules、target、tokenizer
            if !name.starts_with('.') && name != "node_modules" && name != "target" && name != "tokenizer" {
                replace_in_dir(&path.to_string_lossy(), pattern, replacement, use_regex, exts, changed, dry_run)?;
            }
        } else {
            // 安全兜底：跳过调试日志文件，避免自引用循环
            if name == "api_debug.json" {
                continue;
            }
            // 白名单检查：只替换指定扩展名
            let ext_ok = path.extension()
                .and_then(|e| e.to_str())
                .map(|e| exts.contains(&e.to_lowercase()))
                .unwrap_or(false);
            if !ext_ok {
                continue;
            }
            let ps = path.to_string_lossy().to_string();
            if use_regex {
                replace_in_file_regex(&ps, pattern, replacement, changed, dry_run)?;
            } else {
                replace_in_file_literal(&ps, pattern, replacement, changed, dry_run)?;
            }
        }
    }
    Ok(())
}

/// 正则全文替换所有匹配
fn replace_all_regex(re: &Regex, content: &str, replacement: &str) -> Result<String> {
    let mut result = String::new();
    let mut last_end = 0;
    for m in re.find_iter(content) {
        let m = m.context("正则匹配错误")?;
        result.push_str(&content[last_end..m.start()]);
        result.push_str(replacement);
        last_end = m.end();
    }
    result.push_str(&content[last_end..]);
    Ok(result)
}

/// 匹配失败时生成上下文反馈（单文件，与 edit 一致）
fn build_failure_feedback(
    path: &str,
    content: &str,
    pattern: &str,
    use_regex: bool,
    context_lines: usize,
    console_dir: &Option<PathBuf>,
) -> String {
    let total_lines = content.lines().count();
    let mut body = String::from("未找到匹配。");

    if !use_regex {
        let candidates = fuzzy_find_best(content, pattern, 3);
        if !candidates.is_empty() {
            body.push_str(&format!("\n最接近的候选位置：\n"));
            for (idx, (line_num, _, dist)) in candidates.iter().enumerate() {
                body.push_str(&format!(
                    "  {}. 第 {} 行附近（相似度 {:.0}%）\n",
                    idx + 1,
                    line_num + 1,
                    (1.0 - dist) * 100.0,
                ));
                let start = 1.max(line_num.saturating_sub(context_lines / 2));
                let end = total_lines.min(line_num + context_lines / 2);
                let ctx: Vec<String> = content.lines()
                    .skip(start - 1)
                    .take(end - start + 1)
                    .enumerate()
                    .map(|(i, l)| {
                        let lineno = start + i;
                        let arrow = if lineno == line_num + 1 { "→" } else { " " };
                        format!("{} {:>6}\t{}", arrow, lineno, l)
                    })
                    .collect();
                body.push_str(&format!("{}\n", ctx.join("\n")));
            }
        }
        body.push_str("\n提示：如需查看整行，可使用 read 工具");
    } else {
        body.push_str("\n当前为正则模式，如需纯文本匹配请设置 regex=false。");
    }

    const FEEDBACK_TRUNCATE_TOK: usize = 600;
    if let Some((truncated, _)) = truncate_opt(&body, FEEDBACK_TRUNCATE_TOK) {
        if let Some(cd) = console_dir {
            let _ = fs::create_dir_all(cd);
            let seq = REPLACE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let out_path = cd.join(format!("replace_fail_{seq}.out"));
            let full = format!(
                "replace 匹配失败 (file: {})\n{}\n{}\n",
                path,
                if use_regex { "模式: 正则" } else { "模式: 纯文本" },
                body
            );
            let _ = fs::write(&out_path, &full);
            body = format!(
                "{}\n[完整反馈已保存至 {}]",
                truncated,
                out_path.display(),
            );
        }
    }

    body
}

fn truncate_opt(text: &str, max_tok: usize) -> Option<(String, bool)> {
    let (truncated, was_truncated) = truncate_head_tok(text, max_tok);
    if was_truncated { Some((truncated, true)) } else { None }
}

fn fuzzy_find_best<'c>(
    content: &'c str,
    pattern: &str,
    top_n: usize,
) -> Vec<(usize, String, f64)> {
    let mut scored: Vec<(usize, String, f64)> = content
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            let max_len = l.len().max(pattern.len());
            let dist = if max_len == 0 {
                0.0
            } else {
                levenshtein_ratio(l, pattern)
            };
            (i, l.to_string(), dist)
        })
        .collect();

    scored.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
    scored.truncate(top_n);
    scored
}

fn levenshtein_ratio(a: &str, b: &str) -> f64 {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();
    if n == 0 { return if m == 0 { 0.0 } else { 1.0 }; }
    if m == 0 { return 1.0; }

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0; m + 1];

    for i in 0..n {
        curr[0] = i + 1;
        for j in 0..m {
            let cost = if a_chars[i] == b_chars[j] { 0 } else { 1 };
            curr[j + 1] = (prev[j] + cost)
                .min(curr[j] + 1)
                .min(prev[j + 1] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    let max_len = n.max(m);
    if max_len == 0 { 0.0 } else { prev[m] as f64 / max_len as f64 }
}

