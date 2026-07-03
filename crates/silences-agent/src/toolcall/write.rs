//! write — 创建新文件或覆写已读过的文件
//!
//! 与 Claude Code 的 Write 一致：
//! - 文件不存在时直接创建
//! - 文件存在时要求先 read 后才允许覆写

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::Mutex;

use super::{InverseOp, ReadTracker, ToolDef, ToolOutcome};

pub fn tool(read_tracker: ReadTracker) -> ToolDef {
    let tracker = read_tracker.clone();
    ToolDef {
        name: "write",
        description:
            "创建新文件或覆写已读过的文件。\nwhy: 需要生成新代码或修改已有文件时使用。\nhow: 覆写已存在文件前必须先 read 该文件。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "文件绝对路径"
                },
                "content": {
                    "type": "string",
                    "description": "文件内容"
                }
            },
            "required": ["path", "content"],
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
    let content = args["content"].as_str().context("缺少 content 参数")?;

    // 记录覆写前的原文（用于逆操作恢复）
    let original = if Path::new(path).exists() {
        // — 覆写已存在文件：必须先在 read 注册表中 —
        let mut tracker = read_tracker.lock().await;
        let canonical = std::path::absolute(path)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/").to_string())
            .unwrap_or_else(|| path.to_string());

        if !tracker.remove(&canonical) {
            anyhow::bail!(
                "文件已存在但未读取过: {}\n请先使用 read 工具读取该文件，确认内容后再进行覆写。",
                path
            );
        }
        drop(tracker);
        Some(fs::read_to_string(path).unwrap_or_default())
    } else {
        None
    };

    // 确保父目录存在
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).context("创建父目录失败")?;
    }

    fs::write(path, content).context("写入文件失败")?;

    let line_count = content.lines().count();
    let path_owned = path.to_string();
    let original_for_inverse = original.clone();
    Ok(ToolOutcome {
        summary: format!("已写入 {} ({} 行)", path, line_count),
        inverse: Some(InverseOp::new(
            format!("write {}", path),
            move || {
                if let Some(ref orig) = original_for_inverse {
                    // 撤销覆写：恢复原文
                    std::fs::write(&path_owned, orig)?;
                } else {
                    // 撤销新建：删除文件
                    if std::path::Path::new(&path_owned).exists() {
                        std::fs::remove_file(&path_owned)?;
                    }
                }
                Ok(format!("已恢复 {}", path_owned))
            },
        )),
        rollback: false,

        approval_pending: None,
    })
}
