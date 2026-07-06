//! glance — 概览目录或文件信息

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde_json::Value;

use super::{read_file_robust, ToolDef, ToolOutcome};
use silences_core::ToolLimits;

static GLANCE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn tool(console_dir: Option<PathBuf>, limits: ToolLimits) -> ToolDef {
    ToolDef {
        name: "glance",
        description:
            "获取目录一级子项概览或单个文件信息。\nwhy: 探索项目结构[不可撤销]",
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
        handler: Box::new(move |args| {
            let cd = console_dir.clone();
            Box::pin(execute(args, cd, limits))
        }),
    }
}

async fn execute(args: Value, console_dir: Option<PathBuf>, limits: ToolLimits) -> Result<ToolOutcome> {
    let path = args["path"].as_str().context("缺少 path 参数")?;
    let meta = fs::metadata(path).context("路径不存在")?;

    let summary = if meta.is_dir() {
        let (full_output, entry_count) = glance_dir(path)?;
        if entry_count > limits.glance_max_shown_items {
            let lines: Vec<&str> = full_output.lines().collect();
            let header = lines[0];
            let shown = lines[1..=limits.glance_max_shown_items.min(lines.len() - 1)].join("\n");
            let over = entry_count - limits.glance_max_shown_items;
            let file_path = save_glance_file(&console_dir, &full_output);
            format!(
                "{}\n{}\n...以及 {} 个未显示项（共 {} 项），完整输出: {}\n",
                header, shown, over, entry_count, file_path
            )
        } else {
            full_output
        }
    } else {
        glance_file(path, limits)?
    };

    Ok(ToolOutcome::new(summary))
}

fn glance_dir(path: &str) -> Result<(String, usize)> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .context("读取目录失败")?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let total = entries.len();
    let mut lines = Vec::new();
    lines.push(format!("[DIR] {} ({} 项):", path, total));

    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);

        if is_dir {
            lines.push(format!("  [DIR] {}/", name));
        } else {
            let size = meta.map(|m| m.len()).unwrap_or(0);
            let comment_hint = read_leading_comments(&entry.path());
            if let Some(hint) = comment_hint {
                lines.push(format!("  [FILE] {} ({} bytes) 开头: {}", name, size, hint));
            } else {
                lines.push(format!("  [FILE] {} ({} bytes)", name, size));
            }
        }
    }

    Ok((lines.join("\n"), total))
}

fn glance_file(path: &str, limits: ToolLimits) -> Result<String> {
    let meta = fs::metadata(path).context("读取文件信息失败")?;
    let size = meta.len();
    let content = read_file_robust(path).ok();
    let line_count = content.as_ref().map(|c| c.lines().count()).unwrap_or(0);

    let max_lines = limits.glance_max_comment_lines;
    let preview = if let Some(ref c) = content {
        let hint = leading_comment_lines(c, max_lines);
        match hint {
            Some(comments) if comments.lines().count() >= 3 => {
                Some(format!("开头注释:\n{}", comments))
            }
            Some(comments) => {
                // 注释不足 3 行：回退显示前 3 行非空内容
                let preview: Vec<&str> = c.lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(3)
                    .collect();
                let mut fallback = format!("开头注释:\n{}", comments);
                if !preview.is_empty() {
                    fallback.push_str(&format!("\n[文件前 {} 行内容]:\n{}", preview.len(), preview.join("\n")));
                }
                Some(fallback)
            }
            None => {
                // 无注释行：回退显示前 3 行非空内容
                let preview: Vec<&str> = c.lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(3)
                    .collect();
                if !preview.is_empty() {
                    Some(format!("[文件前 {} 行内容]:\n{}", preview.len(), preview.join("\n")))
                } else {
                    None
                }
            }
        }
    } else {
        None
    };

    let mut info = format!("[FILE] {} ({} bytes, {} 行)", path, size, line_count);
    if let Some(ref p) = preview {
        info.push_str(&format!("\n{}", p));
    }

    // 文件行数多时提示用 read 读全文
    if line_count > 500 {
        info.push_str(&format!("\n[提示] 文件共 {} 行，仅显示文件开头。如需完整内容请使用 read 工具。", line_count));
    }

    Ok(info)
}

/// 保存完整目录清单到 console 文件
fn save_glance_file(console_dir: &Option<PathBuf>, content: &str) -> String {
    let dir = match console_dir {
        Some(d) => d.clone(),
        None => return "（未保存，无会话目录）".into(),
    };
    let _ = std::fs::create_dir_all(&dir);
    let seq = GLANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_path = dir.join(format!("glance_{seq}.out"));
    let _ = std::fs::write(&file_path, content);
    file_path.to_string_lossy().to_string()
}

/// 读取文件的开头连续注释行
fn read_leading_comments(path: &Path) -> Option<String> {
    let content = read_file_robust(&path.to_string_lossy()).ok()?;
    leading_comment_lines(&content, 20)
}

fn leading_comment_lines(content: &str, max_lines: usize) -> Option<String> {
    let comment_prefixes = ["//", "#", ";", "--", "(*"];
    let mut comments = Vec::new();
    let mut line_count = 0;

    for line in content.lines().take(max_lines) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if comment_prefixes.iter().any(|p| trimmed.starts_with(p)) {
            comments.push(trimmed);
            line_count += 1;
        } else {
            break;
        }
    }

    if comments.is_empty() {
        None
    } else {
        let mut result = comments.join("\n");
        if line_count >= max_lines {
            result.push_str(&format!(
                "\n[提示: 仅显示前 {max_lines} 行注释，文件可能还有更多注释内容]"
            ));
        }
        Some(result)
    }
}
