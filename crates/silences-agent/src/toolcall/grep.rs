//! grep — 正则搜索（白名单扩展名 + 自动标准化）
//!
//! AI 必须指定 extensions 参数声明要搜索的文件扩展名，grep 不会猜测。
//! 安全兜底：始终跳过隐藏目录、node_modules、target、tokenizer、api_debug.json。
//! 结果 >20 条匹配时摘要仅显示前 20 条，完整输出写入 console 目录供模型读取。

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use fancy_regex::Regex;
use serde_json::Value;

use silences_core::ToolLimits;

use super::{expand_pattern, normalize, read_file_robust, ToolDef, ToolOutcome};

static GREP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn tool(console_dir: Option<PathBuf>, limits: ToolLimits) -> ToolDef {
    ToolDef {
        name: "grep",
        description:
            "在指定路径下搜索正则表达式匹配。\n**你必须指定 extensions** 声明要搜的文件扩展名，grep 不会猜测扩展名。\n每个匹配返回上下各两行。结果超过 20 条时摘要截断，完整输出写入 console 目录。\n安全兜底：始终跳过隐藏目录、node_modules、target、tokenizer、api_debug.json。\nwhy: 需要精确定位代码中某个模式出现的位置时使用。\n注意: 会自动将 \\r\\n 转为 \\n，行首连续 tab 转为 4 空格后搜索。\n提示: 反引号内的内容自动转义为纯文本，可与正则混写。如 `fn main()`*\n 匹配 \"fn main()\" 后跟正则 *\\n。\\` 在反引号内表示字面反引号。\n反例: 不要不加 extensions，否则会报错。",
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
                },
                "extensions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "**必填**。要搜索的文件扩展名，不含点号。例如 [\"rs\",\"ts\",\"tsx\"]。只搜这些扩展名的文件。"
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
    let extensions: HashSet<String> = args["extensions"]
        .as_array()
        .context("extensions 必须是数组")?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
        .collect();
    if extensions.is_empty() {
        anyhow::bail!("extensions 不能为空，请指定要搜索的文件扩展名，如 [\"rs\",\"ts\"]");
    }

    let re_pattern = expand_pattern(raw_pattern);
    let re = Regex::new(&re_pattern).context("正则表达式无效")?;

    let meta = fs::metadata(path).context("路径不存在")?;

    // (格式化输出, 该文件内的匹配条数)
    let mut results: Vec<(String, usize)> = Vec::new();
    if meta.is_dir() {
        search_dir(path, &re, &extensions, &mut results, limits.grep_context_lines)?;
    } else {
        search_file(path, &re, &mut results, limits.grep_context_lines)?;
    }

    if results.is_empty() {
        return Ok(ToolOutcome {
            summary: format!("grep: 无匹配 \"{}\" (仅扩展名: {:?})", raw_pattern, extensions),
            inverse: None,
            rollback: false,
            approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        });
    }

    let total_matches: usize = results.iter().map(|(_, count)| count).sum();
    let summary_text = results
        .iter()
        .map(|(text, _)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n---\n");

    if total_matches <= limits.grep_max_shown_matches {
        // 没超限，正常返回全部结果
        return Ok(ToolOutcome {
            summary: format!("grep \"{}\" 匹配 {} 处:\n{}", raw_pattern, total_matches, summary_text),
            inverse: None,
            rollback: false,
            approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        });
    }

    // 超过上限：摘要只显示前 N 条
    let mut truncated = Vec::new();
    let mut shown = 0;
    for (text, count) in &results {
        if shown >= limits.grep_max_shown_matches {
            break;
        }
        truncated.push(text.as_str());
        shown += count;
    }
    let over_count = total_matches - shown;

    // 完整输出写入 console 目录
    let file_path = if let Some(ref cd) = console_dir {
        let _ = fs::create_dir_all(cd);
        let seq = GREP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let out_path = cd.join(format!("grep_{seq}.out"));
        let full = format!("grep \"{}\" 匹配 {} 处:\n{}", raw_pattern, total_matches, summary_text);
        let _ = fs::write(&out_path, &full);
        Some(out_path)
    } else {
        None
    };

    let suffix = match file_path {
        Some(ref p) => format!("\n...以及 {over_count} 条匹配（共 {total_matches} 条），完整输出: {}\n", p.display()),
        None => format!("\n...以及 {over_count} 条匹配（共 {total_matches} 条），未设置 console 目录，请缩小搜索范围。\n"),
    };

    Ok(ToolOutcome {
        summary: format!(
            "grep \"{}\" 匹配 {} 处（显示前 {} 条）:\n{}{}",
            raw_pattern, total_matches, limits.grep_max_shown_matches,
            truncated.join("\n---\n"),
            suffix,
        ),
        inverse: None,
        rollback: false,
        approval_pending: None,
        inject_messages: vec![],
        defer_rollback: false,
    })
}

fn search_dir(
    dir: &str,
    re: &Regex,
    exts: &HashSet<String>,
    results: &mut Vec<(String, usize)>,
    context_lines: usize,
) -> Result<()> {
    for entry in fs::read_dir(dir).context("读取目录失败")? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            // 安全兜底：跳过隐藏目录、node_modules、target、tokenizer
            if !name.starts_with('.') && name != "node_modules" && name != "target" && name != "tokenizer" {
                search_dir(&path.to_string_lossy(), re, exts, results, context_lines)?;
            }
        } else {
            // 安全兜底：跳过调试日志文件，避免自引用循环
            if name == "api_debug.json" {
                continue;
            }
            // 白名单检查：只搜指定扩展名
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if exts.contains(&ext.to_lowercase()) {
                    search_file(&path.to_string_lossy(), re, results, context_lines)?;
                }
            }
        }
    }
    Ok(())
}

/// 搜索单个文件，如果匹配则 push (formatted_text, match_count) 到 results
fn search_file(path: &str, re: &Regex, results: &mut Vec<(String, usize)>, context_lines: usize) -> Result<()> {
    let content = match read_file_robust(path) {
        Ok(c) => normalize(&c),
        Err(_) => return Ok(()), // 二进制文件跳过
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut file_parts = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if re.is_match(line).unwrap_or(false) {
            let start = i.saturating_sub(context_lines);
            let end = (i + context_lines + 1).min(lines.len());
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
        let count = file_parts.len();
        results.push((format!("[{}]\n{}", path, file_parts.join("\n---\n")), count));
    }
    Ok(())
}
