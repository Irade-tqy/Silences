//! create — 创建新文本文件

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{InverseOp, ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "create",
        description:
            "在指定路径创建新文本文件。若文件已存在则报错。\nwhy: 需要生成新代码文件或文档时使用。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要创建的文件的绝对路径"
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

    if Path::new(path).exists() {
        anyhow::bail!("文件已存在: {}", path);
    }

    // 确保父目录存在
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).context("创建父目录失败")?;
    }

    fs::write(path, content).context("写入文件失败")?;

    let line_count = content.lines().count();
    let preview: String = content.chars().take(100).collect();
    let preview = if content.len() > 100 {
        format!("{}...", preview)
    } else {
        preview
    };

    let path_owned = path.to_string();
    Ok(ToolOutcome {
        summary: format!("[OK] 已创建 {} ({} 行)\n{}", path, line_count, preview),
        inverse: Some(InverseOp::new(
            format!("create {}", path),
            move || {
                if std::path::Path::new(&path_owned).exists() {
                    std::fs::remove_file(&path_owned)?;
                }
                Ok(format!("已删除 {}", path_owned))
            },
        )),
    })
}
