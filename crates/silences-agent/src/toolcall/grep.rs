//! grep — 正则搜索（自动标准化换行和缩进）

use std::fs;

use anyhow::{Context, Result};
use fancy_regex::Regex;
use serde_json::Value;

use super::{expand_pattern, normalize, read_file_robust, ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "grep",
        description:
            "在指定路径下搜索正则表达式匹配。每个匹配返回上下文三行。将会跳过隐藏目录、node_modules、target。\nwhy: 需要精确定位代码中某个模式出现的位置时使用。\n注意: 会自动将 \\r\\n 转为 \\n，行首连续 tab 转为 4 空格后搜索。\n提示: 反引号内的内容自动转义为纯文本，可与正则混写。如 `fn main()`*\n 匹配 \"fn main()\" 后跟正则 *\\n。\\` 在反引号内表示字面反引号。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标文件或目录的绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "正则表达式；反引号内为纯文本，可混写。如 `fn()`abc 匹配 \"fn()\" + 正则 abc"
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

    let mut results = Vec::new();
    if meta.is_dir() {
        search_dir(path, &re, &mut results)?;
    } else {
        search_file(path, &re, &mut results)?;
    }

    if results.is_empty() {
        Ok(ToolOutcome {
            summary: format!("grep: 无匹配 \"{}\"", raw_pattern),
            inverse: None,
        
        rollback: false,
        
        approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        })
    } else {
        let summary = format!(
            "grep \"{}\" 匹配 {} 处:\n{}",
            raw_pattern,
            results.len(),
            results.join("\n---\n")
        );
        Ok(ToolOutcome {
            summary,
            inverse: None,
        
        rollback: false,
        
        approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        })
    }
}

fn search_dir(dir: &str, re: &Regex, results: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).context("读取目录失败")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // 跳过隐藏目录和 node_modules / target
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') && name != "node_modules" && name != "target" {
                search_dir(&path.to_string_lossy(), re, results)?;
            }
        } else if is_text_file(&path) {
            search_file(&path.to_string_lossy(), re, results)?;
        }
    }
    Ok(())
}

fn search_file(path: &str, re: &Regex, results: &mut Vec<String>) -> Result<()> {
    let content = match read_file_robust(path) {
        Ok(c) => normalize(&c),
        Err(_) => return Ok(()), // 二进制文件跳过
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut file_parts = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if re.is_match(line).unwrap_or(false) {
            let start = i.saturating_sub(3);
            let end = (i + 4).min(lines.len());
            let mut ctx = Vec::new();
            ctx.push(format!("{}  {}", i + 1, line));
            for j in start..i {
                ctx.push(format!("  {}  {}", j + 1, lines[j]));
            }
            for j in (i + 1)..end {
                ctx.push(format!("  {}  {}", j + 1, lines[j]));
            }
            file_parts.push(ctx.join("\n"));
        }
    }
    if !file_parts.is_empty() {
        results.push(format!("[{}]\n{}", path, file_parts.join("\n---\n")));
    }
    Ok(())
}

fn is_text_file(path: &std::path::Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => matches!(
            ext,
            "rs"
                | "py"
                | "js"
                | "ts"
                | "jsx"
                | "tsx"
                | "go"
                | "java"
                | "c"
                | "h"
                | "cpp"
                | "hpp"
                | "css"
                | "html"
                | "json"
                | "toml"
                | "yaml"
                | "yml"
                | "md"
                | "txt"
                | "sh"
                | "bat"
                | "ps1"
                | "sql"
                | "xml"
                | "lock"
        ),
        None => false,
    }
}
