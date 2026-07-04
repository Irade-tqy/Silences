//! find — 按文件名正则搜索

use std::fs;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use super::{expand_pattern, ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "find",
        description:
            "按文件名正则表达式搜索指定目录下的文件。返回匹配文件的相对路径，按目录层级组织。将会跳过隐藏目录、node_modules、target。\nwhy: 需要快速定位文件名满足某种模式的文件时使用。\n提示: 反引号内为纯文本，可混写。如 `main.rs` 匹配字面 \"main.rs\"。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标目录的绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "文件名正则；反引号内纯文本，可混写。如 `main`\\.rs"
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
    let raw_pattern = args["pattern"].as_str().context("缺少 pattern 参数")?;
    let re_pattern = expand_pattern(raw_pattern);
    let re = Regex::new(&re_pattern).context("正则表达式无效")?;

    let meta = fs::metadata(path).context("路径不存在")?;
    if !meta.is_dir() {
        anyhow::bail!("路径不是目录: {path}");
    }

    let mut results = Vec::new();
    search_dir(path, path, &re, &mut results);

    if results.is_empty() {
        Ok(ToolOutcome {
            summary: format!("find: 在 {} 中无匹配 \"{}\"", path, raw_pattern),
            inverse: None,
        
        rollback: false,
        
        approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        })
    } else {
        let header = format!("find \"{}\" in {}:\n", raw_pattern, path);
        let body = results.join("\n");
        let footer = format!("\n── 共 {} 个匹配", results.len());
        Ok(ToolOutcome {
            summary: format!("{header}{body}{footer}"),
            inverse: None,
        
        rollback: false,
        
        approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        })
    }
}

/// 递归搜索目录，匹配文件名。跳过无法读取的目录。
fn search_dir(root: &str, dir: &str, re: &Regex, results: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // 跳过权限不足的目录
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if !name.starts_with('.') && name != "node_modules" && name != "target" {
                search_dir(root, &path.to_string_lossy(), re, results);
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
}
