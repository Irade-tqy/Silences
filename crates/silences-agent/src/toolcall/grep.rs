//! grep — 文本/正则搜索（白名单扩展名 + 自动标准化）
//!
//! AI 必须指定 extensions 参数声明要搜索的文件扩展名，grep 不会猜测。
//! 安全兜底：始终跳过隐藏目录、node_modules、target、tokenizer、api_debug.json。
//! 结果 >20 条匹配时摘要仅显示前 20 条，完整输出写入 console 目录供模型读取。

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use fancy_regex::Regex;
use serde_json::Value;

use silences_core::ToolLimits;

use super::{normalize, read_file_robust, truncate_head_tok, ToolDef, ToolOutcome};

static GREP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn tool(console_dir: Option<PathBuf>, limits: ToolLimits) -> ToolDef {
    ToolDef {
        name: "grep",
        description:
            "在路径下搜索文本或正则匹配\nwhy: 精确定位[不可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标文件或目录的绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "要匹配的模式"
                },
                "extensions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "要搜索的文件扩展名，不含点号。如 [\"rs\",\"ts\",\"tsx\"]"
                },
                "context_lines": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 20,
                    "description": "每个匹配行上下各显示多少行（默认 2）"
                },
                "regex": {
                    "type": "boolean",
                    "description": "true=启用正则（默认 false）"
                }
            },
            "required": ["path", "pattern", "extensions"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let cd = console_dir.clone();
            Box::pin(execute(args, cd, limits))
        }),
    }
}

async fn execute(args: Value, console_dir: Option<PathBuf>, limits: ToolLimits) -> Result<ToolOutcome> {
    let path = args["path"].as_str().context("缺少 path 参数")?;
    let raw_pattern = args["pattern"].as_str().context("缺少 pattern 参数")?;
    let use_regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
    let extensions: HashSet<String> = args["extensions"]
        .as_array()
        .context("extensions 必须是数组")?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
        .collect();
    if extensions.is_empty() {
        anyhow::bail!("extensions 不能为空，请指定要搜索的文件扩展名，如 [\"rs\",\"ts\"]");
    }

    let re_pattern = if use_regex {
        raw_pattern.to_string()
    } else {
        regex::escape(raw_pattern)
    };
    let re = Regex::new(&re_pattern).context("搜索模式无效")?;

    let ctx_lines = args.get("context_lines")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(limits.grep_context_lines);

    let meta = fs::metadata(path).context("路径不存在")?;

    // (格式化输出, 该文件内的匹配条数)
    let mut results: Vec<(String, usize)> = Vec::new();
    if meta.is_dir() {
        search_dir(path, &re, &extensions, &mut results, ctx_lines)?;
    } else {
        search_file(path, &re, &mut results, ctx_lines)?;
    }

    if results.is_empty() {
        let summary = if !meta.is_dir() && !use_regex {
            // 单文件 + 纯文本模式：模糊匹配找最近似行
            let content = read_file_robust(path).unwrap_or_default();
            let content = normalize(&content);
            grep_failure_feedback(&content, raw_pattern, limits.edit_context_lines, &console_dir)
        } else {
            format!("grep: 无匹配 \"{}\" (仅扩展名: {:?})", raw_pattern, extensions)
        };
        return Ok(ToolOutcome {
            summary,
            inverse: None,
            rollback: false,
            approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        });
    }

    let total_matches: usize = results.iter().map(|(_, count)| count).sum();
    let summary_text = results
        .iter()
        .map(|(text, _)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n---\n");

    if total_matches <= limits.grep_max_shown_matches {
        // 没超限，正常返回全部结果
        return Ok(ToolOutcome {
            summary: format!("grep \"{}\" 匹配 {} 处:\n{}", raw_pattern, total_matches, summary_text),
            inverse: None,
            rollback: false,
            approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        });
    }

    // 超过上限：摘要只显示前 N 条
    let mut truncated = Vec::new();
    let mut shown = 0;
    for (text, count) in &results {
        if shown >= limits.grep_max_shown_matches {
            break;
        }
        truncated.push(text.as_str());
        shown += count;
    }
    let over_count = total_matches - shown;

    // 完整输出写入 console 目录
    let file_path = if let Some(ref cd) = console_dir {
        let _ = fs::create_dir_all(cd);
        let seq = GREP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let out_path = cd.join(format!("grep_{seq}.out"));
        let full = format!("grep \"{}\" 匹配 {} 处:\n{}", raw_pattern, total_matches, summary_text);
        let _ = fs::write(&out_path, &full);
        Some(out_path)
    } else {
        None
    };

    let suffix = match file_path {
        Some(ref p) => format!("\n...以及 {over_count} 条匹配（共 {total_matches} 条），完整输出: {}\n", p.display()),
        None => format!("\n...以及 {over_count} 条匹配（共 {total_matches} 条），未设置 console 目录，请缩小搜索范围。\n"),
    };

    Ok(ToolOutcome {
        summary: format!(
            "grep \"{}\" 匹配 {} 处（显示前 {} 条）:\n{}{}",
            raw_pattern, total_matches, limits.grep_max_shown_matches,
            truncated.join("\n---\n"),
            suffix,
        ),
        inverse: None,
        rollback: false,
        approval_pending: None,
        inject_messages: vec![],
        defer_rollback: false,
    })
}

fn search_dir(
    dir: &str,
    re: &Regex,
    exts: &HashSet<String>,
    results: &mut Vec<(String, usize)>,
    context_lines: usize,
) -> Result<()> {
    for entry in fs::read_dir(dir).context("读取目录失败")? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            // 安全兜底：跳过隐藏目录、node_modules、target、tokenizer
            if !name.starts_with('.') && name != "node_modules" && name != "target" && name != "tokenizer" {
                search_dir(&path.to_string_lossy(), re, exts, results, context_lines)?;
            }
        } else {
            // 安全兜底：跳过调试日志文件，避免自引用循环
            if name == "api_debug.json" {
                continue;
            }
            // 白名单检查：只搜指定扩展名
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if exts.contains(&ext.to_lowercase()) {
                    search_file(&path.to_string_lossy(), re, results, context_lines)?;
                }
            }
        }
    }
    Ok(())
}

/// 搜索单个文件，如果匹配则 push (formatted_text, match_count) 到 results
fn search_file(path: &str, re: &Regex, results: &mut Vec<(String, usize)>, context_lines: usize) -> Result<()> {
    let content = match read_file_robust(path) {
        Ok(c) => normalize(&c),
        Err(_) => return Ok(()), // 二进制文件跳过
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut file_parts = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if re.is_match(line).unwrap_or(false) {
            let start = i.saturating_sub(context_lines);
            let end = (i + context_lines + 1).min(lines.len());
            let mut ctx = Vec::new();
            ctx.push(format!("{}  {}", i + 1, line));
            for j in start..i {
                ctx.push(format!("  {}  {}", j + 1, lines[j]));
            }
            for j in (i + 1)..end {
                ctx.push(format!("  {}  {}", j + 1, lines[j]));
            }
            file_parts.push(ctx.join("\n"));
        }
    }
    if !file_parts.is_empty() {
        let count = file_parts.len();
        results.push((format!("[{}]\n{}", path, file_parts.join("\n---\n")), count));
    }
    Ok(())
}

/// grep 单文件匹配失败时模糊反馈
fn grep_failure_feedback(
    content: &str,
    pattern: &str,
    context_lines: usize,
    console_dir: &Option<PathBuf>,
) -> String {
    let total_lines = content.lines().count();
    let mut body = String::from("grep: 无匹配。");
    let candidates = fuzzy_find_best(content, pattern, 3);

    if !candidates.is_empty() {
        body.push_str(&format!("\n最接近的候选行：\n"));
        for (idx, (line_num, _, dist)) in candidates.iter().enumerate() {
            body.push_str(&format!(
                "  {}. 第 {} 行（相似度 {:.0}%）\n",
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

    // 截断入文件
    const FEEDBACK_TRUNCATE_TOK: usize = 600;
    let (body_cropped, was_truncated) = truncate_head_tok(&body, FEEDBACK_TRUNCATE_TOK);
    if was_truncated {
        if let Some(cd) = console_dir {
            let _ = fs::create_dir_all(cd);
            let seq = GREP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let out_path = cd.join(format!("grep_fail_{seq}.out"));
            let _ = fs::write(&out_path, &body);
            body = format!("{}\n[完整反馈已保存至 {}]", body_cropped, out_path.display());
        }
    }

    body
}

/// 模糊匹配：对每一行计算 Levenshtein 距离，返回最接近的 N 行
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
            let dist = levenshtein_ratio(l, pattern);
            (i, l.to_string(), dist)
        })
        .collect();

    scored.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
    scored.truncate(top_n);
    scored
}

/// 两字符串间的 Levenshtein 距离归一化 [0,1]，0=完全相同
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
