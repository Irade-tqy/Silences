//! raw_read — 读取文件原始内容（不做换行和缩进标准化）

use std::fs;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "raw_read",
        description:
            "读取文件原始内容，不做任何标准化。若文件为空返回提示。\nwhy: 需要查看文件的原始格式（原始换行符、Tab 等）时使用。",
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

    let content = fs::read_to_string(path).context("读取文件失败")?;

    if content.is_empty() {
        return Ok(ToolOutcome {
            summary: format!("[空文件] {}", path),
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
        "[RAW FILE] {} (共 {} 行, 显示 {}–{})\n{}",
        path,
        total,
        start,
        end,
        selected.join("\n")
    );

    Ok(ToolOutcome {
        summary,
        inverse: None,
    })
}
