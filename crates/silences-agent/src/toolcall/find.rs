//! find — 按文件名搜索（白名单扩展名保护）
//!
//! AI 必须指定 extensions 参数声明要搜索的文件扩展名，find 不会猜测。
//! 安全兜底：始终跳过隐藏目录、node_modules、target、tokenizer、api_debug.json。

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use super::{ToolDef, ToolOutcome};
use silences_core::ToolLimits;

static FIND_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn tool(console_dir: Option<PathBuf>, limits: ToolLimits) -> ToolDef {
    ToolDef {
        name: "find",
        description:
            "按文件名搜索目录，返回相对路径。跳过隐藏目录、node_modules、target、tokenizer、api_debug.json。\nwhy: 定位文件名匹配的文件[不可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标目录的绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "搜索模式"
                },
                "extensions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "要搜索的文件扩展名，不含点号。如 [\"rs\",\"ts\",\"tsx\"]"
                },
                "regex": {
                    "type": "boolean",
                    "description": "true=启用正则（默认 false）"
                }
            },
            "required": ["path", "pattern", "extensions"],
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
    let use_regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
    let extensions: HashSet<String> = args["extensions"]
        .as_array()
        .context("extensions 必须是数组")?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
        .collect();
    if extensions.is_empty() {
        anyhow::bail!("extensions 不能为空，请指定要搜索的文件扩展名，如 [\"rs\",\"ts\"]");
    }

    let re_pattern = if use_regex {
        raw_pattern.to_string()
    } else {
        regex::escape(raw_pattern)
    };
    let re = Regex::new(&re_pattern).context("搜索模式无效")?;

    let meta = fs::metadata(path).context("路径不存在")?;
    if !meta.is_dir() {
        anyhow::bail!("路径不是目录: {path}");
    }

    let mut results = Vec::new();
    search_dir(path, path, &re, &extensions, &mut results);

    if results.is_empty() {
        return Ok(ToolOutcome {
            summary: format!("find: 在 {} 中无匹配 \"{}\" (仅扩展名: {:?})", path, raw_pattern, extensions),
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
fn search_dir(root: &str, dir: &str, re: &Regex, exts: &HashSet<String>, results: &mut Vec<String>) {
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
            // 安全兜底：跳过隐藏目录、node_modules、target、tokenizer
            if !name.starts_with('.') && name != "node_modules" && name != "target" && name != "tokenizer" {
                search_dir(root, &path.to_string_lossy(), re, exts, results);
            }
        } else {
            // 安全兜底：跳过调试日志文件，避免自引用循环
            if name == "api_debug.json" {
                continue;
            }
            // 白名单检查：只搜索指定扩展名
            let ext_ok = path.extension()
                .and_then(|e| e.to_str())
                .map(|e| exts.contains(&e.to_lowercase()))
                .unwrap_or(false);
            if !ext_ok {
                continue;
            }
            if re.is_match(&name) {
                let rel = path.to_string_lossy().to_string();
                let display = rel
                    .strip_prefix(root)
                    .and_then(|s| s.strip_prefix(std::path::MAIN_SEPARATOR))
                    .unwrap_or(&rel);
                results.push(format!("  {}", display));
            }
        }
    }
}
