//! find — 按文件名正则搜索

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use super::{expand_pattern, ToolDef, ToolOutcome};
use silences_core::ToolLimits;

static FIND_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn tool(console_dir: Option<PathBuf>, limits: ToolLimits) -> ToolDef {
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
        handler: Box::new(move |args| {
            let cd = console_dir.clone();
            Box::pin(execute(args, cd, limits))
        }),
    }
}

async fn execute(args: Value, console_dir: Option<PathBuf>, limits: ToolLimits) -> Result<ToolOutcome> {
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
        return Ok(ToolOutcome {
            summary: format!("find: 在 {} 中无匹配 \"{}\"", path, raw_pattern),
            inverse: None,
        
        rollback: false,
        
        approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        });
    }

    let total = results.len();
    if total <= limits.find_max_shown_items {
        let header = format!("find \"{}\" in {}:\n", raw_pattern, path);
        let body = results.join("\n");
        let footer = format!("\n── 共 {} 个匹配", total);
        Ok(ToolOutcome {
            summary: format!("{header}{body}{footer}"),
            inverse: None,
        
        rollback: false,
        
        approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        })
    } else {
        let mut shown = Vec::new();
        for r in &results[..limits.find_max_shown_items] {
            shown.push(r.as_str());
        }
        let over = total - limits.find_max_shown_items;
        let file_path = save_find_file(&console_dir, path, raw_pattern, &results);

        let header = format!("find \"{}\" in {}:\n", raw_pattern, path);
        let body = shown.join("\n");
        Ok(ToolOutcome {
            summary: format!(
                "{}{}\n...以及 {} 个匹配（共 {} 个），完整输出: {}\n",
                header, body, over, total, file_path
            ),
            inverse: None,
        
        rollback: false,
        
        approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        })
    }
}

/// 保存完整搜索结果到 console 文件
fn save_find_file(console_dir: &Option<PathBuf>, path: &str, pattern: &str, results: &[String]) -> String {
    let dir = match console_dir {
        Some(d) => d.clone(),
        None => return "（未保存，无会话目录）".into(),
    };
    let _ = std::fs::create_dir_all(&dir);
    let seq = FIND_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_path = dir.join(format!("find_{seq}.out"));
    let header = format!("find \"{}\" in {}:\n", pattern, path);
    let body = results.join("\n");
    let footer = format!("\n── 共 {} 个匹配", results.len());
    let _ = std::fs::write(&file_path, format!("{}{}{}", header, body, footer));
    file_path.to_string_lossy().to_string()
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
