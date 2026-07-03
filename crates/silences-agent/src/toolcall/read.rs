//! read — 读取文件内容（自动标准化换行和缩进）

use std::fs;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{is_tabsensitive, normalize, TABSENSITIVE_WARNING, ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "read",
        description:
            "读取文件内容。\nwhy: 需要查看代码或文件的内容时使用。\nhow: 若文件为空返回提示。\n注意: 会自动将 \\r\\n 转为 \\n，行首连续 tab 转为 4 空格展示。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "文件绝对路径"
                },
                "start_line": {
                    "type": "integer",
                    "description": "起始行号（1-based，可选，默认 1）"
                },
                "end_line": {
                    "type": "integer",
                    "description": "结束行号（1-based，可选，默认末尾）"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        handler: Box::new(|args| Box::pin(execute(args))),
    }
}

async fn execute(args: Value) -> Result<ToolOutcome> {
    let path = args["path"].as_str().context("缺少 path 参数")?;

    if !std::path::Path::new(path).exists() {
        anyhow::bail!("文件不存在: {}", path);
    }

    let original = fs::read_to_string(path).context("读取文件失败")?;
    let is_mk = is_tabsensitive(path);
    let content = if is_mk { original } else { normalize(&original) };
    let warning = if is_mk { format!("\n{}", TABSENSITIVE_WARNING) } else { String::new() };

    if content.is_empty() {
        return Ok(ToolOutcome {
            summary: format!("[空文件] {}{}", path, warning),
            inverse: None,
        });
    }

    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    let start = args["start_line"].as_u64().unwrap_or(1) as usize;
    let end = args["end_line"].as_u64().map(|e| e as usize).unwrap_or(total);

    let start = start.max(1).min(total);
    let end = end.max(start).min(total);

    let selected: Vec<String> = lines[start - 1..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", start + i, line))
        .collect();

    let summary = format!(
        "[FILE] {} (共 {} 行, 显示 {}–{})\n{}{}",
        path,
        total,
        start,
        end,
        selected.join("\n"),
        warning,
    );

    Ok(ToolOutcome {
        summary,
        inverse: None,
    })
}
