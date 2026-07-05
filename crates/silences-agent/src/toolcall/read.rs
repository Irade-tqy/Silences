//! read — 读取文件内容
//! raw=false（默认）：自动标准化换行和缩进
//! raw=true：保持原始格式

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::Mutex;

use super::{
    auto_truncate, is_tabsensitive, normalize, read_file_robust, ReadTracker, TABSENSITIVE_WARNING,
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
        return Ok(ToolOutcome {
            summary: format!("[空文件] {}{}", path, warning),
            inverse: None,

        rollback: false,

        approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        });
    }

    // 自动截断：仅当 all=false 且未指定显式行范围时才生效
    let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
    let has_explicit_range = args.get("start_line").is_some() || args.get("end_line").is_some();
    let display_content = if !all && !has_explicit_range {
        let (truncated, _) = auto_truncate(&content, 2000, 1500, 500);
        truncated
    } else {
        content.clone()
    };

    let total = content.lines().count();
    let lines: Vec<&str> = display_content.lines().collect();
    let display_total = lines.len();

    let start = args["start_line"].as_u64().unwrap_or(1) as usize;
    let end = args["end_line"].as_u64().map(|e| e as usize).unwrap_or(display_total);

    let start = start.max(1).min(display_total);
    let end = end.max(start).min(display_total);

    let selected: Vec<String> = lines[start - 1..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", start + i, line))
        .collect();

    let summary = format!(
        "{} {} (共 {} 行, 显示 {}–{})\n{}{}",
        prefix,
        path,
        total,
        start,
        end,
        selected.join("\n"),
        warning,
    );

    // 注册已读文件（写前检查用）
    let mut tracker = read_tracker.lock().await;
    if let Ok(abs) = std::path::absolute(path) {
        tracker.insert(abs.to_string_lossy().replace('\\', "/"));
    } else {
        tracker.insert(path.to_string());
    }
    drop(tracker);

    Ok(ToolOutcome {
        summary,
        inverse: None,

        rollback: false,

        approval_pending: None,
        inject_messages: vec![],
            defer_rollback: false,
        })
}
