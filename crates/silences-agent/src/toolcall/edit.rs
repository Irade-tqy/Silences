//! edit — 按正则替换文件中第一个匹配（全文匹配，支持 PCRE 锚点）
//! 自动标准化换行符和缩进。

use std::fs;

use anyhow::{Context, Result};
use fancy_regex::Regex;
use serde_json::Value;

use super::{
    expand_pattern, is_tabsensitive, normalize, read_file_robust, InverseOp, TABSENSITIVE_WARNING,
    ToolDef, ToolOutcome,
};

pub fn tool() -> ToolDef {
    ToolDef {
        name: "edit",
        description:
            "将文件中正则匹配的第一个结果替换为指定字符串。\nwhy: 对单个位置进行精准修改时使用。\nhow: 全文匹配（不按行拆分），正则引擎为 fancy-regex，`(`, `)`, `*`, `+`, `.`, `[`, `]`, `{`, `}`, `^`, `$`, `\\` 均为元字符，需 `\\` 转义。\n注意: 会自动将 \\r\\n 转为 \\n，行首连续 tab 转为 4 空格。需要保持原始格式请用 raw_edit。\n提示: 反引号内为纯文本，可与正则混写。如 `fn main()`*\n 匹配 \"fn main()\" 后跟正则 *\\n。",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "文件绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "正则表达式；反引号内纯文本，可混写。如 `fn()`abc"
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
    let raw_pattern = args["pattern"].as_str().context("缺少 pattern 参数")?;
    let replacement = args["replacement"].as_str().context("缺少 replacement 参数")?;
    let target_line = args.get("line").and_then(Value::as_u64);

    let re_pattern = expand_pattern(raw_pattern);
    let re = Regex::new(&re_pattern).context("正则表达式无效")?;
    let original = read_file_robust(file)?;

    let is_mk = is_tabsensitive(file);
    let content = if is_mk { original.clone() } else { normalize(&original) };
    let warning = if is_mk { format!("\n{}", TABSENSITIVE_WARNING) } else { String::new() };

    // 全文匹配（不按行拆分，支持跨行正则）
    let mut matches_positions: Vec<(usize, usize)> = Vec::new(); // (byte_start, byte_end)
    for m in re.find_iter(&content) {
        let m = m.context("正则匹配错误")?;
        matches_positions.push((m.start(), m.end()));
    }

    if matches_positions.is_empty() {
        anyhow::bail!("未找到匹配 \"{}\"", raw_pattern);
    }

    // 构建行起始字节偏移表 → O(log n) 字节定位行号
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(content.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let pos_to_line = |pos: usize| -> usize {
        match line_starts.binary_search(&pos) {
            Ok(i) => i + 1,   // 恰好是行首
            Err(i) => i,       // 落在 i 号行内（1-based，因为 Err 返回插入位置）
        }
    };

    // 选择匹配
    let (abs_start, abs_end) = match target_line {
        Some(tl) => {
            let tl = tl as usize;
            matches_positions.sort_by_key(|&(start, _)| {
                (pos_to_line(start) as isize - tl as isize).abs()
            });
            matches_positions[0]
        }
        None => {
            if matches_positions.len() > 1 {
                anyhow::bail!(
                    "匹配不唯一（{} 处），请指定 line 参数选择目标行",
                    matches_positions.len()
                );
            }
            matches_positions[0]
        }
    };

    let match_line = pos_to_line(abs_start);
    let new_content = format!("{}{}{}", &content[..abs_start], replacement, &content[abs_end..]);

    fs::write(file, &new_content).context("写入文件失败")?;

    let file_owned = file.to_string();
    Ok(ToolOutcome {
        summary: format!("已编辑 {}:{}{}", file, match_line, warning),
        inverse: Some(InverseOp::new(
            format!("edit on {}", file),
            move || {
                std::fs::write(&file_owned, &original)?;
                Ok(format!("已恢复 {}", file_owned))
            },
        )),
        rollback: false,
    
        approval_pending: None,
    inject_messages: vec![],
    defer_rollback: false,
    })
}
