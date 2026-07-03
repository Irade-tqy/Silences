//! raw_edit — 按正则替换文件中第一个匹配（按行号最近）
//! 不做任何换行符或缩进标准化，保持文件原始格式。

use std::fs;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use super::{InverseOp, ToolDef, ToolOutcome};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "raw_edit",
        description:
            "将文件中正则匹配的第一个结果替换为指定字符串。不执行换行符和缩进标准化。\nwhy: 需要保持文件原始格式（CRLF、Tab 等）时使用。\nhow: 不指定 line 且匹配不唯一时报错。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "文件绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "正则表达式"
                },
                "replacement": {
                    "type": "string",
                    "description": "要替换为的字符串"
                },
                "line": {
                    "type": "integer",
                    "description": "目标行号。不指定 line 且匹配唯一时自动选择；不指定 line 且匹配不唯一时报错。"
                }
            },
            "required": ["file", "pattern", "replacement"],
            "additionalProperties": false
        }),
        handler: Box::new(|args| Box::pin(execute(args))),
    }
}

async fn execute(args: Value) -> Result<ToolOutcome> {
    let file = args["file"].as_str().context("缺少 file 参数")?;
    let pattern_str = args["pattern"].as_str().context("缺少 pattern 参数")?;
    let replacement = args["replacement"].as_str().context("缺少 replacement 参数")?;
    let target_line = args.get("line").and_then(Value::as_u64);

    let re = Regex::new(pattern_str).context("正则表达式无效")?;
    let original = fs::read_to_string(file).context("读取文件失败")?;

    // 找到所有匹配及其行号
    let lines: Vec<&str> = original.lines().collect();
    let mut matches: Vec<(usize, usize, usize)> = Vec::new(); // (line_no, byte_start, byte_end)

    for (i, line) in lines.iter().enumerate() {
        for m in re.find_iter(line) {
            matches.push((i + 1, m.start(), m.end()));
        }
    }

    if matches.is_empty() {
        anyhow::bail!("未找到匹配 \"{}\"", pattern_str);
    }

    // 选择匹配
    let (match_line, byte_start_in_line, byte_end_in_line) = match target_line {
        Some(tl) => {
            // 按行号最近排序
            matches.sort_by_key(|(line, _, _)| (*line as isize - tl as isize).abs());
            matches[0]
        }
        None => {
            if matches.len() > 1 {
                anyhow::bail!(
                    "匹配不唯一（{} 处），请指定 line 参数选择目标行",
                    matches.len()
                );
            }
            matches[0]
        }
    };

    // 计算在全文中的绝对字节偏移
    let abs_start: usize = lines[..match_line - 1]
        .iter()
        .map(|l| l.len() + 1) // +1 for newline
        .sum::<usize>()
        + byte_start_in_line;
    let abs_end = abs_start + (byte_end_in_line - byte_start_in_line);

    let new_content = format!("{}{}{}", &original[..abs_start], replacement, &original[abs_end..]);

    fs::write(file, &new_content).context("写入文件失败")?;

    let preview: String = replacement.chars().take(60).collect();
    let file_owned = file.to_string();
    Ok(ToolOutcome {
        summary: format!("已编辑 {}:{} (替换为 \"{}\")", file, match_line, preview),
        inverse: Some(InverseOp::new(
            format!("raw_edit on {}", file),
            move || {
                std::fs::write(&file_owned, &original)?;
                Ok(format!("已恢复 {}", file_owned))
            },
        )),
    })
}
