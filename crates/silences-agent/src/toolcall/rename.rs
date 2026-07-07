//! rename — 移动/重命名文件或目录
//!
//! 可跨目录移动（自动创建目标父目录），可撤销。
//! 跨文件系统时 fs::rename 可能失败（EXDEV），此时会返回错误提示。

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{InverseOp, ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "rename",
        description:
            "重命名或移动文件/目录。可跨目录移动，自动创建目标父目录。\n注意：跨文件系统移动可能失败。\nwhy: 整理文件结构[可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "源文件或目录的绝对路径"
                },
                "destination": {
                    "type": "string",
                    "description": "目标路径的绝对路径"
                }
            },
            "required": ["source", "destination"],
            "additionalProperties": false
        }),
        handler: Box::new(|args| Box::pin(execute(args))),
    }
}

async fn execute(args: Value) -> Result<ToolOutcome> {
    let source = args["source"].as_str().context("缺少 source 参数")?;
    let destination = args["destination"].as_str().context("缺少 destination 参数")?;

    let src_path = Path::new(source);
    let dst_path = Path::new(destination);

    if !src_path.exists() {
        anyhow::bail!("重命名失败：源路径不存在: {}", source);
    }
    if dst_path.exists() {
        anyhow::bail!("重命名失败：目标路径已存在: {}", destination);
    }

    // 自动创建目标父目录
    if let Some(parent) = dst_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).context("创建目标父目录失败")?;
        }
    }

    fs::rename(source, destination).with_context(|| {
        format!(
            "重命名/移动失败。可能原因：跨文件系统移动（请使用 command 工具手动 cp + trash），或权限不足。\n源: {}\n目标: {}",
            source, destination
        )
    })?;

    let source_owned = source.to_string();
    let dest_owned = destination.to_string();
    Ok(ToolOutcome {
        summary: format!("[MOVED] {} → {}", source, destination),
        inverse: Some(InverseOp::new(
            format!("rename {} → {}", source, destination),
            move || {
                fs::rename(&dest_owned, &source_owned)
                    .with_context(|| format!("撤销重命名失败：无法从 {} 移回 {}", dest_owned, source_owned))?;
                Ok(format!("已恢复: {} ← {}", source_owned, dest_owned))
            },
        )),
        rollback: false,
        approval_pending: None,
        inject_messages: vec![],
        defer_rollback: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rename_file() {
        let dir = std::env::temp_dir().join("rename_test_file");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let src = dir.join("old.txt");
        let dst = dir.join("new.txt");
        fs::write(&src, "content").unwrap();

        let args = serde_json::json!({
            "source": src.to_string_lossy(),
            "destination": dst.to_string_lossy(),
        });
        let result = execute(args).await.unwrap();
        assert!(result.summary.contains("[MOVED]"));
        assert!(!src.exists());
        assert!(dst.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "content");

        // undo
        let inv = result.inverse.unwrap();
        let undo_summary = inv.apply().unwrap();
        assert!(undo_summary.contains("已恢复"));
        assert!(src.exists());
        assert!(!dst.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_rename_dir() {
        let dir = std::env::temp_dir().join("rename_test_dir");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src_sub")).unwrap();
        fs::write(dir.join("src_sub/file.txt"), "data").unwrap();

        let src = dir.join("src_sub");
        let dst = dir.join("dst_sub");
        let src_s = src.to_string_lossy().to_string();
        let dst_s = dst.to_string_lossy().to_string();

        let args = serde_json::json!({
            "source": src_s,
            "destination": dst_s,
        });
        let result = execute(args).await.unwrap();
        assert!(result.summary.contains("[MOVED]"));
        assert!(!src.exists());
        assert!(dst.exists());
        assert!(dst.join("file.txt").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_rename_source_not_found() {
        let args = serde_json::json!({
            "source": "/nonexistent/path/file.txt",
            "destination": "/some/dest.txt",
        });
        let err = execute(args).await.unwrap_err();
        assert!(err.to_string().contains("源路径不存在"));
    }

    #[tokio::test]
    async fn test_rename_destination_exists() {
        let dir = std::env::temp_dir().join("rename_test_exists");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let src = dir.join("src.txt");
        let dst = dir.join("dst.txt");
        fs::write(&src, "src content").unwrap();
        fs::write(&dst, "dst content").unwrap();

        let args = serde_json::json!({
            "source": src.to_string_lossy(),
            "destination": dst.to_string_lossy(),
        });
        let err = execute(args).await.unwrap_err();
        assert!(err.to_string().contains("目标路径已存在"));

        let _ = fs::remove_dir_all(&dir);
    }
}
