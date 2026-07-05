//! replace — 在目录下所有文件中搜索并替换所有匹配
//! regex=true（默认）：全文正则匹配
//! regex=false：纯文本字面量匹配
//! 自动标准化换行符和缩进。

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
            "在指定路径下所有文本文件中搜索并替换所有匹配。\nwhy: 需要批量重命名或重构时使用。\nhow: regex=true 全文正则匹配（默认）；regex=false 纯文本字面量匹配。\n注意: 会自动将 \\r\\n 转为 \\n，行首连续 tab 转为 4 空格。\n匹配失败时显示最近似的位置。[可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标文件或目录的绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "regex=true 为正则表达式；regex=false 为纯文本字面量"
                },
                "replacement": {
                    "type": "string",
                    "description": "要替换为的字符串"
                },
                "regex": {
                    "type": "boolean",
                    "description": "true=正则模式, false=纯文本字面量模式（默认）"
                }
            },
            "required": ["path", "pattern", "replacement"],
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

    let meta = fs::metadata(path).context("路径不存在")?;

    let mut changed_files: Vec<(String, String)> = Vec::new();

    if meta.is_dir() {
        replace_in_dir(path, raw_pattern, replacement, use_regex, &mut changed_files)?;
    } else if use_regex {
        // 单文件：正则
        replace_in_file_regex(path, raw_pattern, replacement, &mut changed_files)?;
    } else {
        // 单文件：字面量
        replace_in_file_literal(path, raw_pattern, replacement, &mut changed_files)?;
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
                "replace: 在 {} 中无匹配。{}",
                path,
                if use_regex {
                    "当前为正则模式，如需纯文本匹配请设置 regex=false。"
                } else {
                    "请检查 pattern 是否正确。"
                }
            )
        };
        return Ok(ToolOutcome {
            summary: feedback,
            inverse: None,
            rollback: false,
            approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        });
    }

    let file_list: Vec<String> = changed_files
        .iter()
        .map(|(p, _)| p.clone())
        .collect();

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
) -> Result<()> {
    let re = Regex::new(raw_pattern).context("正则表达式无效")?;
    let original = match read_file_robust(path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    let content = normalize(&original);
    let new = replace_all_regex(&re, &content, replacement)?;
    if new != content {
        fs::write(path, &new).context("写入文件失败")?;
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
) -> Result<()> {
    let original = match read_file_robust(path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    let content = normalize(&original);
    let new = content.replace(pattern, replacement);
    if new != content {
        fs::write(path, &new).context("写入文件失败")?;
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
    changed: &mut Vec<(String, String)>,
) -> Result<()> {
    for entry in fs::read_dir(dir).context("读取目录失败")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') && name != "node_modules" && name != "target" {
                replace_in_dir(&path.to_string_lossy(), pattern, replacement, use_regex, changed)?;
            }
        } else if is_text_file(&path) {
            let ps = path.to_string_lossy().to_string();
            if use_regex {
                replace_in_file_regex(&ps, pattern, replacement, changed)?;
            } else {
                replace_in_file_literal(&ps, pattern, replacement, changed)?;
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

fn is_text_file(path: &std::path::Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => matches!(
            ext,
            "rs"
                | "py"
                | "js"
                | "ts"
                | "jsx"
                | "tsx"
                | "go"
                | "java"
                | "c"
                | "h"
                | "cpp"
                | "hpp"
                | "css"
                | "html"
                | "json"
                | "toml"
                | "yaml"
                | "yml"
                | "md"
                | "txt"
                | "sh"
                | "bat"
                | "ps1"
                | "sql"
                | "xml"
                | "lock"
        ),
        None => false,
    }
}
