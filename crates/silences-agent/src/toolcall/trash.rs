//! trash — 将文件安全移动到 .trash 文件夹

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{InverseOp, ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "trash",
        description:
            "将文件移动到项目的 .trash 文件夹。\nwhy: 安全删除文件，保留恢复可能。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要移入回收站的文件或目录的绝对路径"
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
    let src = Path::new(path);

    if !src.exists() {
        anyhow::bail!("路径不存在: {}", path);
    }

    let abs_path = fs::canonicalize(path).context("获取绝对路径失败")?;

    // 找到项目根（向上找 .trash 或最近的仓库根）
    let project_root = find_project_root(&abs_path).unwrap_or_else(|| {
        abs_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| abs_path.clone())
    });

    let trash_dir = project_root.join(".trash");
    fs::create_dir_all(&trash_dir).context("创建 .trash 目录失败")?;

    // 生成唯一目标名（避免重名覆盖）
    let file_name = abs_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let dest_name = format!("{}_{}", timestamp, file_name);
    let dest = trash_dir.join(&dest_name);

    fs::rename(&abs_path, &dest).context("移动到 .trash 失败")?;

    let dest_s = dest.to_string_lossy().to_string();
    let orig_s = abs_path.to_string_lossy().to_string();
    Ok(ToolOutcome {
        summary: format!("[TRASHED] 已移入回收站: {} -> .trash/{}", path, dest_name),
        inverse: Some(InverseOp::new(
            format!("trash {}", path),
            move || {
                if let Some(parent) = std::path::Path::new(&orig_s).parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                std::fs::rename(&dest_s, &orig_s)?;
                Ok(format!("已恢复到 {}", orig_s))
            },
        )),
    })
}

/// 向上找到项目根（包含 .trash 或 .git 的目录）
fn find_project_root(path: &Path) -> Option<std::path::PathBuf> {
    for ancestor in path.ancestors() {
        if ancestor.join(".trash").exists() || ancestor.join(".git").exists() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}
