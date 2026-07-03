//! glance — 概览目录或文件信息

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "glance",
        description:
            "获取指定目录的一级子项概览或单个文件的信息。返回名称、文本文件行数和开头连续注释行的内容。\nwhy: 用于探索项目结构，了解代码库布局。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标文件或目录的绝对路径"
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
    let meta = fs::metadata(path).context("路径不存在")?;

    let summary = if meta.is_dir() {
        glance_dir(path)?
    } else {
        glance_file(path)?
    };

    Ok(ToolOutcome {
        summary,
        inverse: None,
    })
}

fn glance_dir(path: &str) -> Result<String> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .context("读取目录失败")?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut lines = Vec::new();
    lines.push(format!("[DIR] {} ({} 项):", path, entries.len()));

    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);

        if is_dir {
            lines.push(format!("  [DIR] {}/", name));
        } else {
            let size = meta.map(|m| m.len()).unwrap_or(0);
            // 尝试读文件头部注释
            let comment_hint = read_leading_comments(&entry.path());
            if let Some(hint) = comment_hint {
                lines.push(format!("  [FILE] {} ({} bytes) 开头: {}", name, size, hint));
            } else {
                lines.push(format!("  [FILE] {} ({} bytes)", name, size));
            }
        }
    }

    Ok(lines.join("\n"))
}

fn glance_file(path: &str) -> Result<String> {
    let meta = fs::metadata(path).context("读取文件信息失败")?;
    let size = meta.len();
    let content = fs::read_to_string(path).ok();
    let line_count = content.as_ref().map(|c| c.lines().count()).unwrap_or(0);

    let comment_hint = if let Some(ref c) = content {
        leading_comment_lines(c)
    } else {
        None
    };

    let mut info = format!("[FILE] {} ({} bytes, {} 行)", path, size, line_count);
    if let Some(hint) = comment_hint {
        info.push_str(&format!("\n开头注释:\n{}", hint));
    }

    Ok(info)
}

/// 读取文件的开头连续注释行
fn read_leading_comments(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    leading_comment_lines(&content)
}

fn leading_comment_lines(content: &str) -> Option<String> {
    let comment_prefixes = ["//", "#", ";", "--", "(*"];
    let mut comments = Vec::new();

    for line in content.lines().take(20) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if comment_prefixes.iter().any(|p| trimmed.starts_with(p)) {
            comments.push(trimmed);
        } else {
            break;
        }
    }

    if comments.is_empty() {
        None
    } else {
        Some(comments.join("\n"))
    }
}
