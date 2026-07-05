//! raw_edit — 替换文件中第一个匹配，不执行标准化
//! regex=true（默认）：全文正则匹配（支持 PCRE 锚点）
//! regex=false：纯文本字面量匹配

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use fancy_regex::Regex;
use serde_json::Value;

use silences_core::ToolLimits;

use super::{read_file_robust, truncate_head_tok, InverseOp, ToolDef, ToolOutcome};

static RAW_EDIT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn tool(console_dir: Option<PathBuf>, limits: ToolLimits) -> ToolDef {
    ToolDef {
        name: "raw_edit",
        description:
            "将文件中匹配的第一个结果替换为指定字符串。不执行换行符和缩进标准化。\nwhy: 需要保持文件原始格式（CRLF、Tab 等）时使用。\nhow: regex=true 全文正则匹配（默认）；regex=false 纯文本字面量匹配。\n匹配失败时显示最近似的位置。[可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "文件绝对路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "regex=true 为正则表达式；regex=false 为纯文本字面量"
                },
                "replacement": {
                    "type": "string",
                    "description": "要替换为的字符串"
                },
                "line": {
                    "type": "integer",
                    "description": "目标行号。不指定 line 且匹配唯一时自动选择；不指定 line 且匹配不唯一时报错。"
                },
                "regex": {
                    "type": "boolean",
                    "description": "true=正则模式, false=纯文本字面量模式（默认）"
                }
            },
            "required": ["file", "pattern", "replacement"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let cd = console_dir.clone();
            Box::pin(async move { execute(args, cd, limits).await })
        }),
    }
}

async fn execute(args: Value, console_dir: Option<PathBuf>, limits: ToolLimits) -> Result<ToolOutcome> {
    let file = args["file"].as_str().context("缺少 file 参数")?;
    let raw_pattern = args["pattern"].as_str().context("缺少 pattern 参数")?;
    let replacement = args["replacement"].as_str().context("缺少 replacement 参数")?;
    let target_line = args.get("line").and_then(Value::as_u64);
    let use_regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);

    let original = read_file_robust(file)?;

    // ── 匹配阶段 ──
    let matches_positions = if use_regex {
        find_regex_matches(&original, raw_pattern)?
    } else {
        find_literal_matches(&original, raw_pattern)
    };

    // ── 匹配失败：反馈 ──
    if matches_positions.is_empty() {
        let feedback = build_failure_feedback(
            file, &original, raw_pattern, use_regex, target_line,
            limits.edit_context_lines, &console_dir,
        );
        anyhow::bail!("{}", feedback);
    }

    // ── 构建行起始字节偏移表 ──
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(original.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let pos_to_line = |pos: usize| -> usize {
        match line_starts.binary_search(&pos) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    };

    // ── 选择匹配位置 ──
    let (abs_start, abs_end) = match target_line {
        Some(tl) => {
            let tl = tl as usize;
            matches_positions
                .into_iter()
                .min_by_key(|&(start, _)| {
                    (pos_to_line(start) as isize - tl as isize).abs()
                })
                .context("选择匹配时出错")?
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
    let new_content = format!("{}{}{}", &original[..abs_start], replacement, &original[abs_end..]);

    fs::write(file, &new_content).context("写入文件失败")?;

    let file_owned = file.to_string();
    Ok(ToolOutcome {
        summary: format!("已编辑 {}:{}", file, match_line),
        inverse: Some(InverseOp::new(
            format!("raw_edit on {}", file),
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

fn find_regex_matches(content: &str, pattern: &str) -> Result<Vec<(usize, usize)>> {
    let re = Regex::new(pattern).context("正则表达式无效")?;
    let mut positions = Vec::new();
    for m in re.find_iter(content) {
        let m = m.context("正则匹配错误")?;
        positions.push((m.start(), m.end()));
    }
    Ok(positions)
}

fn find_literal_matches<'c>(content: &'c str, pattern: &str) -> Vec<(usize, usize)> {
    let mut positions = Vec::new();
    let mut search_start = 0;
    while let Some(found) = content[search_start..].find(pattern) {
        let abs_start = search_start + found;
        let abs_end = abs_start + pattern.len();
        positions.push((abs_start, abs_end));
        search_start = abs_end;
    }
    positions
}

fn build_failure_feedback(
    path: &str,
    content: &str,
    pattern: &str,
    use_regex: bool,
    target_line: Option<u64>,
    context_lines: usize,
    console_dir: &Option<PathBuf>,
) -> String {
    let total_lines = content.lines().count();
    let mut body = String::from("未找到匹配。");

    if let Some(line) = target_line {
        let line = line as usize;
        let start = 1.max(line.saturating_sub(context_lines));
        let end = total_lines.min(line + context_lines);
        let page: Vec<String> = content.lines()
            .skip(start - 1)
            .take(end - start + 1)
            .enumerate()
            .map(|(i, l)| {
                let lineno = start + i;
                let arrow = if lineno == line { "→" } else { " " };
                format!("{} {:>6}\t{}", arrow, lineno, l)
            })
            .collect();
        body.push_str(&format!(
            "\n目标第 {} 行附近（±{} 行）的内容：\n{}",
            line, context_lines, page.join("\n"),
        ));
    } else if !use_regex {
        let candidates = fuzzy_find_best(content, pattern, 3);
        if !candidates.is_empty() {
            body.push_str(&format!("\n最接近的候选位置：\n"));
            for (idx, (line_num, _, dist)) in candidates.iter().enumerate() {
                body.push_str(&format!(
                    "  {}. 第 {} 行附近（相似度 {:.0}%）\n",
                    idx + 1,
                    line_num + 1,
                    (1.0 - dist) * 100.0,
                ));
                let start = 1.max(line_num.saturating_sub(context_lines / 2));
                let end = total_lines.min(line_num + context_lines / 2);
                let ctx: Vec<String> = content.lines()
                    .skip(start - 1)
                    .take(end - start + 1)
                    .enumerate()
                    .map(|(i, l)| {
                        let lineno = start + i;
                        let arrow = if lineno == line_num + 1 { "→" } else { " " };
                        format!("{} {:>6}\t{}", arrow, lineno, l)
                    })
                    .collect();
                body.push_str(&format!("{}\n", ctx.join("\n")));
            }
        }
        body.push_str("\n提示：如需查看整行，可使用 read 工具");
    } else {
        body.push_str("\n当前为正则模式，如需纯文本匹配请设置 regex=false。");
    }

    const FEEDBACK_TRUNCATE_TOK: usize = 600;
    if let Some((truncated, _)) = truncate_opt(&body, FEEDBACK_TRUNCATE_TOK) {
        if let Some(cd) = console_dir {
            let _ = fs::create_dir_all(cd);
            let seq = RAW_EDIT_COUNTER.fetch_add(1, Ordering::Relaxed);
            let out_path = cd.join(format!("raw_edit_fail_{seq}.out"));
            let full = format!(
                "raw_edit 匹配失败 (file: {})\n{}\n{}\n",
                path,
                if use_regex { "模式: 正则" } else { "模式: 纯文本" },
                body
            );
            let _ = fs::write(&out_path, &full);
            body = format!(
                "{}\n[完整反馈已保存至 {}]",
                truncated,
                out_path.display(),
            );
        }
    }

    body
}

fn truncate_opt(text: &str, max_tok: usize) -> Option<(String, bool)> {
    let (truncated, was_truncated) = truncate_head_tok(text, max_tok);
    if was_truncated { Some((truncated, true)) } else { None }
}

fn fuzzy_find_best<'c>(
    content: &'c str,
    pattern: &str,
    top_n: usize,
) -> Vec<(usize, String, f64)> {
    let mut scored: Vec<(usize, String, f64)> = content
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            let max_len = l.len().max(pattern.len());
            let dist = if max_len == 0 {
                0.0
            } else {
                levenshtein_ratio(l, pattern)
            };
            (i, l.to_string(), dist)
        })
        .collect();

    scored.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
    scored.truncate(top_n);
    scored
}

fn levenshtein_ratio(a: &str, b: &str) -> f64 {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();
    if n == 0 { return if m == 0 { 0.0 } else { 1.0 }; }
    if m == 0 { return 1.0; }

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0; m + 1];

    for i in 0..n {
        curr[0] = i + 1;
        for j in 0..m {
            let cost = if a_chars[i] == b_chars[j] { 0 } else { 1 };
            curr[j + 1] = (prev[j] + cost)
                .min(curr[j] + 1)
                .min(prev[j + 1] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    let max_len = n.max(m);
    if max_len == 0 { 0.0 } else { prev[m] as f64 / max_len as f64 }
}
