//! read — 读取文件内容
//! raw=false（默认）：自动标准化换行和缩进
//! raw=true：保持原始格式

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::Mutex;

use super::{
    get_tokenizer, is_tabsensitive, normalize, read_file_robust, ReadTracker, TABSENSITIVE_WARNING,
    ToolDef, ToolOutcome,
};

pub fn tool(read_tracker: ReadTracker) -> ToolDef {
    let tracker = read_tracker.clone();
    ToolDef {
        name: "read",
        description:
            "读取文件[不可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "文件绝对路径"
                },
                "all": {
                    "type": "boolean",
                    "description": "是否关闭大文件自动截断（默认 false）"
                },
                "start_line": {
                    "type": "integer",
                    "description": "起始行号（1-based，默认 1）"
                },
                "end_line": {
                    "type": "integer",
                    "description": "结束行号（1-based，默认末尾）"
                },
                "raw": {
                    "type": "boolean",
                    "description": "true=不执行 CRLF/Tab 标准化（默认 false）"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let rt = tracker.clone();
            Box::pin(async move { execute(args, rt).await })
        }),
    }
}

async fn execute(args: Value, read_tracker: Arc<Mutex<HashSet<String>>>) -> Result<ToolOutcome> {
    let path = args["path"].as_str().context("缺少 path 参数")?;

    if !std::path::Path::new(path).exists() {
        anyhow::bail!("文件不存在: {}", path);
    }

    let original = read_file_robust(path)?;
    let use_raw = args.get("raw").and_then(Value::as_bool).unwrap_or(false);
    let (content, is_raw_mode) = if use_raw {
        (original, true)
    } else if is_tabsensitive(path) {
        (original, false)
    } else {
        (normalize(&original), false)
    };
    let prefix = if is_raw_mode { "[RAW FILE]" } else { "[FILE]" };
    let warning = if !use_raw && is_tabsensitive(path) {
        format!("\n{}", TABSENSITIVE_WARNING)
    } else {
        String::new()
    };

    if content.is_empty() {
        return Ok(ToolOutcome::new(format!("[空文件] {}{}", path, warning)));
    }

    let total = content.lines().count();
    let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
    let has_explicit_range = args.get("start_line").is_some() || args.get("end_line").is_some();

    // 当用户指定了 all=true 或显式行范围时，不自动截断
    let should_truncate = !all && !has_explicit_range;

    // 生成带行号的输出行
    // 返回值：(行号文本列表, 是否截断)
    let (numbered_lines, warning_lines, was_truncated) = if should_truncate {
        format_lines_with_truncation(&content, total)
    } else {
        let lines: Vec<String> = content.lines()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", i + 1, line))
            .collect();
        (lines, Vec::new(), false)
    };

    // 应用 start_line / end_line 过滤
    let user_start = args["start_line"].as_u64().unwrap_or(1) as usize;
    let user_end = args["end_line"].as_u64().map(|e| e as usize).unwrap_or(numbered_lines.len());

    let user_start = user_start.max(1).min(numbered_lines.len());
    let user_end = user_end.max(user_start).min(numbered_lines.len());

    let mut output = format!("{} {} (共 {} 行", prefix, path, total);
    if should_truncate && was_truncated {
        output.push_str(", 已截断");
    }
    if has_explicit_range || (should_truncate && was_truncated) {
        output.push_str(&format!(", 显示 {}–{}", user_start, user_end));
    }
    output.push(')');

    // 截断公告（如果有）
    for w in &warning_lines {
        output.push_str(&format!("\n{}", w));
    }

    // 行内容
    for line in &numbered_lines[user_start - 1..user_end] {
        output.push('\n');
        output.push_str(line);
    }

    output.push_str(&warning);

    // 注册已读文件（写前检查用）
    let mut tracker = read_tracker.lock().await;
    if let Ok(abs) = std::path::absolute(path) {
        tracker.insert(abs.to_string_lossy().replace('\\', "/"));
    } else {
        tracker.insert(path.to_string());
    }
    drop(tracker);

    Ok(ToolOutcome::new(output))
}

/// 截断并生成带行号的输出行。
///
/// pub(super) 对外暴露供测试。
///
/// 返回 (numbered_lines, warning_lines, was_truncated)：
/// - numbered_lines：每行已编号的文本（或公告行，不带编号）
/// - warning_lines：截断公告行（不带编号，放在顶部）
/// - was_truncated：是否实际截断
pub(super) fn format_lines_with_truncation(content: &str, total_lines: usize) -> (Vec<String>, Vec<String>, bool) {
    const THRESHOLD_TOK: usize = 2000;
    const HEAD_TOK: usize = 1500;
    const TAIL_TOK: usize = 500;

    let all_lines: Vec<&str> = content.lines().collect();

    // 用 tokenizer 估算 token 数
    let total_tok = if let Some(tok) = get_tokenizer() {
        if let Ok(enc) = tok.encode(content, true) {
            enc.len()
        } else {
            // 回退字节估算
            content.len() / 4
        }
    } else {
        content.len() / 4
    };

    if total_tok <= THRESHOLD_TOK {
        // 不超限：全文编号，无截断
        let lines: Vec<String> = all_lines.iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", i + 1, line))
            .collect();
        return (lines, Vec::new(), false);
    }

    // 需要截断：估算头尾各保留多少行
    // 按比例从 token 换算到行，确保头尾不重叠
    let total_tok_float = total_tok as f64;
    let head_tok = HEAD_TOK.min(total_tok - TAIL_TOK);
    let tail_tok = TAIL_TOK.min(total_tok - head_tok);

    let head_ratio = head_tok as f64 / total_tok_float;
    let tail_ratio = tail_tok as f64 / total_tok_float;

    let head_lines = (total_lines as f64 * head_ratio).ceil() as usize;
    let tail_lines = (total_lines as f64 * tail_ratio).ceil() as usize;

    // 确保不重叠且有间隔
    let head_lines = head_lines.min(total_lines.saturating_sub(tail_lines + 1));
    let tail_start = total_lines.saturating_sub(tail_lines).max(head_lines + 1);
    let head_lines = head_lines.min(tail_start.saturating_sub(1));

    let mut numbered = Vec::new();
    let mut warnings = Vec::new();

    warnings.push(format!(
        "[截断：文件较大 (~{} tok，共 {} 行)，仅显示 1-{} 行 + {}-{} 行。使用 all=true 或 start_line/end_line 读取完整内容。]",
        total_tok, total_lines, head_lines, tail_start + 1, total_lines,
    ));

    // 头部行（编号）
    for i in 0..head_lines {
        numbered.push(format!("{:>6}\t{}", i + 1, all_lines[i]));
    }

    // 截断分隔符（不编号）
    numbered.push("[...截断...]".to_string());

    // 尾部行（编号）
    for i in tail_start..total_lines {
        numbered.push(format!("{:>6}\t{}", i + 1, all_lines[i]));
    }

    (numbered, warnings, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_lines_under_threshold() {
        let content = "line1\nline2\nline3";
        let (numbered, warnings, truncated) = format_lines_with_truncation(content, 3);
        assert!(!truncated);
        assert!(warnings.is_empty());
        assert_eq!(numbered.len(), 3);
        assert_eq!(numbered[0], "     1\tline1");
        assert_eq!(numbered[2], "     3\tline3");
    }

    #[test]
    fn test_format_lines_empty() {
        let (numbered, warnings, truncated) = format_lines_with_truncation("", 0);
        assert!(!truncated);
        assert!(warnings.is_empty());
        assert!(numbered.is_empty());
    }

    #[test]
    fn test_truncation_notice_not_numbered() {
        // 1500 lines to exceed the 2000 tok threshold
        let content: String = (0..1500)
            .map(|i| format!("this is line number {} of the test content yeah\n", i + 1))
            .collect();
        let total = content.lines().count();
        let (numbered, warnings, truncated) = format_lines_with_truncation(&content, total);
        assert!(truncated, "should be truncated with 1500 lines");
        assert_eq!(warnings.len(), 1, "should have one warning");
        assert!(warnings[0].starts_with("[截断"), "warning should start with [截断");

        // Verify no line-numbered line contains the word "截断"
        for line in &numbered {
            if line.contains("截断") {
                // The only line containing 截断 should be the separator "[...截断...]" (not numbered)
                assert_eq!(line.trim(), "[...截断...]", "截断 should only appear in the separator, not in numbered lines");
            }
        }
    }

    #[test]
    fn test_truncation_line_numbers_correct() {
        let content: String = (0..1500)
            .map(|i| format!("line_{}\n", i + 1))
            .collect();
        let total = content.lines().count();
        let (numbered, _warnings, truncated) = format_lines_with_truncation(&content, total);
        assert!(truncated, "should be truncated with 1500 lines");

        // First numbered line should be line 1
        assert!(numbered[0].ends_with("\tline_1"), "first line should be line_1: {:?}", numbered[0]);

        // The separator shouldn't be numbered
        let sep = numbered.iter().position(|l| l.trim() == "[...截断...]");
        assert!(sep.is_some(), "should have [...] separator");

        // Last numbered line should be the last line with correct number
        let last = numbered.last().unwrap();
        assert!(last.ends_with("\tline_1500"), "last line should be line_1500: {:?}", last);
        assert!(last.starts_with("  1500\t"), "last line number should be 1500: {:?}", last);
    }
}
