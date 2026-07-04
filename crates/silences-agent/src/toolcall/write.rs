//! write — 创建新文件或覆写已有文件
//!
//! 与 Claude Code 的 Write 一致：
//! - 文件不存在时直接创建
//! - 文件存在时直接覆写（有 regret 逆操作可恢复）

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{InverseOp, ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "write",
        description:
            "创建新文件或覆写已有文件。\nwhy: 需要生成新代码或修改已有文件时使用。\nhow: 文件存在时直接覆写，可通过 regret 恢复原文。[可撤销]",
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
        handler: Box::new(|args| Box::pin(execute(args))),
    }
}

async fn execute(args: Value) -> Result<ToolOutcome> {
    let path = args["path"].as_str().context("缺少 path 参数")?;
    let content = args["content"].as_str().context("缺少 content 参数")?;

    // 记录覆写前的原文（用于逆操作恢复）
    let original = if Path::new(path).exists() {
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
    inject_messages: vec![],
    defer_rollback: false,
    })
}
