//! replace — 在目录下所有文件中搜索并替换正则表达式的所有匹配
//! 自动标准化换行符和缩进。

use std::fs;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use super::{normalize, InverseOp, ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "replace",
        description:
            "在指定路径下所有文本文件中搜索并替换正则表达式的所有匹配。\nwhy: 需要批量重命名或重构时使用。\nhow: 谨慎使用，影响范围大。\n注意: 会自动将 \\r\\n 转为 \\n，行首连续 tab 转为 4 空格。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目标文件或目录的绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "正则表达式"
                },
                "replacement": {
                    "type": "string",
                    "description": "要替换为的字符串"
                }
            },
            "required": ["path", "pattern", "replacement"],
            "additionalProperties": false
        }),
        handler: Box::new(|args| Box::pin(execute(args))),
    }
}

async fn execute(args: Value) -> Result<ToolOutcome> {
    let path = args["path"].as_str().context("缺少 path 参数")?;
    let pattern_str = args["pattern"].as_str().context("缺少 pattern 参数")?;
    let replacement = args["replacement"].as_str().context("缺少 replacement 参数")?;

    let re = Regex::new(pattern_str).context("正则表达式无效")?;
    let meta = fs::metadata(path).context("路径不存在")?;

    let mut changed_files: Vec<(String, String)> = Vec::new();

    if meta.is_dir() {
        replace_in_dir(path, &re, replacement, &mut changed_files)?;
    } else {
        replace_in_file(path, &re, replacement, &mut changed_files)?;
    }

    if changed_files.is_empty() {
        return Ok(ToolOutcome {
            summary: format!("replace: 无匹配 \"{}\"", pattern_str),
            inverse: None,
        });
    }

    let file_list: Vec<String> = changed_files
        .iter()
        .map(|(p, _)| p.clone())
        .collect();

    let files_owned = changed_files.clone();
    Ok(ToolOutcome {
        summary: format!(
            "[REPLACED] \"{}\" -> \"{}\" 在 {} 个文件中:\n{}",
            pattern_str,
            replacement,
            changed_files.len(),
            file_list.join("\n")
        ),
        inverse: Some(InverseOp::new(
            format!("replace {} files", changed_files.len()),
            move || {
                for (path, original_content) in &files_owned {
                    std::fs::write(path, original_content)?;
                }
                Ok(format!("已恢复 {} 个文件", files_owned.len()))
            },
        )),
    })
}

fn replace_in_dir(
    dir: &str,
    re: &Regex,
    replacement: &str,
    changed: &mut Vec<(String, String)>,
) -> Result<()> {
    for entry in fs::read_dir(dir).context("读取目录失败")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') && name != "node_modules" && name != "target" {
                replace_in_dir(&path.to_string_lossy(), re, replacement, changed)?;
            }
        } else if is_text_file(&path) {
            replace_in_file(&path.to_string_lossy(), re, replacement, changed)?;
        }
    }
    Ok(())
}

fn replace_in_file(
    path: &str,
    re: &Regex,
    replacement: &str,
    changed: &mut Vec<(String, String)>,
) -> Result<()> {
    let original = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    let content = normalize(&original);
    let new = re.replace_all(&content, replacement).to_string();
    if new != content {
        fs::write(path, &new).context("写入文件失败")?;
        changed.push((path.to_string(), original));
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
