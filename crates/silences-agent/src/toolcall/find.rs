//! find — 按文件名正则搜索

use std::fs;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use super::{ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "find",
        description:
            "按文件名正则表达式搜索指定目录下的文件。返回匹配文件的相对路径，按目录层级组织。将会跳过隐藏目录、node_modules、target。\nwhy: 需要快速定位文件名满足某种模式的文件时使用。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标目录的绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "文件名正则表达式（如 .*\\.rs$ 匹配所有 Rust 文件）"
                }
            },
            "required": ["path", "pattern"],
            "additionalProperties": false
        }),
        handler: Box::new(|args| Box::pin(execute(args))),
    }
}

async fn execute(args: Value) -> Result<ToolOutcome> {
    let path = args["path"].as_str().context("缺少 path 参数")?;
    let pattern_str = args["pattern"].as_str().context("缺少 pattern 参数")?;
    let re = Regex::new(pattern_str).context("正则表达式无效")?;

    let meta = fs::metadata(path).context("路径不存在")?;
    if !meta.is_dir() {
        anyhow::bail!("路径不是目录: {path}");
    }

    let mut results = Vec::new();
    search_dir(path, path, &re, &mut results)?;

    if results.is_empty() {
        Ok(ToolOutcome {
            summary: format!("find: 在 {} 中无匹配 \"{}\"", path, pattern_str),
            inverse: None,
        
        rollback: false,
        
        approval_pending: None,
        })
    } else {
        let header = format!("find \"{}\" in {}:\n", pattern_str, path);
        let body = results.join("\n");
        let footer = format!("\n── 共 {} 个匹配", results.len());
        Ok(ToolOutcome {
            summary: format!("{header}{body}{footer}"),
            inverse: None,
        
        rollback: false,
        
        approval_pending: None,
        })
    }
}

/// 递归搜索目录，匹配文件名
fn search_dir(root: &str, dir: &str, re: &Regex, results: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).context("读取目录失败")? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if !name.starts_with('.') && name != "node_modules" && name != "target" {
                search_dir(root, &path.to_string_lossy(), re, results)?;
            }
        } else if re.is_match(&name) {
            let rel = path.to_string_lossy().to_string();
            let display = rel
                .strip_prefix(root)
                .and_then(|s| s.strip_prefix(std::path::MAIN_SEPARATOR))
                .unwrap_or(&rel);
            results.push(format!("  {}", display));
        }
    }
    Ok(())
}
