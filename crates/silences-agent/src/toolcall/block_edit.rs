//! block_edit — 按起始/结束行标记替换块范围
//! 纯文本匹配（line.contains），无正则
//! raw=false（默认）：自动标准化换行符和缩进
//! raw=true：保持原始格式

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde_json::Value;

use silences_core::ToolLimits;

use super::{
    is_tabsensitive, normalize, read_file_robust, truncate_head_tok, InverseOp, TABSENSITIVE_WARNING,
    ToolDef, ToolOutcome,
};

static BLOCK_EDIT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn tool(console_dir: Option<PathBuf>, limits: ToolLimits) -> ToolDef {
    ToolDef {
        name: "block_edit",
        description:
            "将文件中起始行到结束行之间的内容替换为指定文本，匹配失败时显示最近似的位置。\nwhy: 替换跨多行的代码块[可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "文件绝对路径"
                },
                "start": {
                    "type": "string",
                    "description": "起始行匹配文本"
                },
                "end": {
                    "type": "string",
                    "description": "结束行匹配文本"
                },
                "replacement": {
                    "type": "string",
                    "description": "替换内容"
                },
                "include_start": {
                    "type": "boolean",
                    "description": "是否替换起始行（默认 true）"
                },
                "include_end": {
                    "type": "boolean",
                    "description": "是否替换结束行（默认 true）"
                },
                "line": {
                    "type": "integer",
                    "description": "取 (start行+end行)/2 最接近此行的匹配"
                },
                "raw": {
                    "type": "boolean",
                    "description": "true=不执行 CRLF/Tab 标准化（默认 false）"
                }
            },
            "required": ["file", "start", "end", "replacement"],
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
    let start_pattern = args["start"].as_str().context("缺少 start 参数")?;
    let end_pattern = args["end"].as_str().context("缺少 end 参数")?;
    let replacement = args["replacement"].as_str().context("缺少 replacement 参数")?;
    let include_start = args.get("include_start").and_then(Value::as_bool).unwrap_or(true);
    let include_end = args.get("include_end").and_then(Value::as_bool).unwrap_or(true);
    let target_line = args.get("line").and_then(Value::as_u64);
    let use_raw = args.get("raw").and_then(Value::as_bool).unwrap_or(false);

    let original = read_file_robust(file)?;

    let (content, warning) = if use_raw {
        (original.clone(), String::new())
    } else if is_tabsensitive(file) {
        (original.clone(), format!("\n{}", TABSENSITIVE_WARNING))
    } else {
        (normalize(&original), String::new())
    };

    let lines: Vec<&str> = content.lines().collect();

    // ── 查找所有 (start_line, end_line) 配对 ──
    let pairs = find_pairs(&lines, start_pattern, end_pattern);

    if pairs.is_empty() {
        let feedback = build_failure_feedback(
            file, &content, start_pattern, end_pattern, target_line,
            limits.edit_context_lines, &console_dir,
        );
        anyhow::bail!("{}", feedback);
    }

    // ── 选择配对 ──
    let (start_idx, end_idx) = select_pair(&pairs, target_line)?;

    // ── 计算替换行范围 ──
    let range_start = start_idx + if include_start { 0 } else { 1 };
    let range_end = end_idx + if include_end { 1 } else { 0 };

    if range_start > range_end {
        anyhow::bail!(
            "替换范围无效：start 第 {} 行，end 第 {} 行，include_start={}, include_end={} 导致空范围",
            start_idx + 1, end_idx + 1, include_start, include_end,
        );
    }
    if range_start == range_end {
        anyhow::bail!(
            "替换范围为空（start 和 end 在同一行），请调整 include_start/include_end",
        );
    }

    // ── 计算字节偏移并执行替换 ──
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(content.match_indices('\n').map(|(i, _)| i + 1))
        .collect();

    let byte_start = line_starts.get(range_start).copied().unwrap_or(content.len());
    let byte_end = line_starts.get(range_end).copied().unwrap_or(content.len());

    let new_content = format!("{}{}{}", &content[..byte_start], replacement, &content[byte_end..]);

    fs::write(file, &new_content).context("写入文件失败")?;

    let file_owned = file.to_string();
    let lines_count = range_end - range_start;
    Ok(ToolOutcome {
        summary: format!(
            "已替换 {} 第 {}-{} 行（共 {} 行）{}",
            file, range_start + 1, range_end, lines_count, warning,
        ),
        inverse: Some(InverseOp::new(
            format!("block_edit on {}", file),
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

/// 纯文本匹配：查找所有 (start_line, end_line) 配对
/// 从 start 下一行开始搜索第一个 end，不嵌套。
fn find_pairs<'a>(lines: &[&'a str], start: &str, end: &str) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].contains(start) {
            // 从下一行开始找 end
            let mut found = false;
            for j in (i + 1)..lines.len() {
                if lines[j].contains(end) {
                    pairs.push((i, j));
                    i = j + 1;
                    found = true;
                    break;
                }
            }
            if !found {
                // start 匹配了但后续无 end → 跳过该 start，继续往后搜
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    pairs
}

/// 选择配对：唯一时自动选；多对时按 line 参数消歧
fn select_pair(pairs: &[(usize, usize)], target_line: Option<u64>) -> Result<(usize, usize)> {
    match target_line {
        None => {
            if pairs.len() == 1 {
                Ok(pairs[0])
            } else {
                anyhow::bail!(
                    "匹配不唯一（{} 对），请指定 line 参数选择目标对",
                    pairs.len()
                )
            }
        }
        Some(tl) => {
            let tl = tl as usize;
            pairs
                .iter()
                .min_by_key(|(start, end)| {
                    let mid = (start + end) / 2;
                    (mid as isize - tl as isize).abs()
                })
                .copied()
                .context("选择配对时出错")
        }
    }
}

/// 匹配失败时生成包含上下文的反馈
fn build_failure_feedback(
    path: &str,
    content: &str,
    start_pattern: &str,
    end_pattern: &str,
    target_line: Option<u64>,
    context_lines: usize,
    console_dir: &Option<PathBuf>,
) -> String {
    let total_lines = content.lines().count();
    #[allow(unused_assignments)]
    let mut body = String::new();

    // 先试 start 是否匹配
    let start_lines: Vec<usize> = content
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(start_pattern))
        .map(|(i, _)| i)
        .collect();

    if start_lines.is_empty() {
        // start 没匹配：模糊查找
        body = format!("❌ block_edit 失败：起始行匹配失败\n\n未找到起始行匹配 '{}'。", start_pattern);
        let candidates = fuzzy_find_best(content, start_pattern, 3);
        if !candidates.is_empty() {
            body.push_str("\n最接近的候选位置：\n");
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
        } else {
            body.push_str("\n文件中无任何接近的内容。");
        }
        body.push_str("\n提示：如需查看文件内容，可使用 read 工具");
    } else {
        // start 匹配了，但后续找不到 end
        body = format!(
            "❌ block_edit 失败：结束行匹配失败\n\n起始行 '{}' 已匹配（{} 处），但之后未找到结束行 '{}'。",
            start_pattern,
            start_lines.len(),
            end_pattern,
        );

        // 显示最近的一个 start 位置附近
        let anchor = if let Some(tl_val) = target_line {
            let tl = tl_val as usize;
            let mut best = start_lines[0];
            let mut best_dist = usize::MAX;
            for &ln in &start_lines {
                let dist = if ln > tl { ln - tl } else { tl - ln };
                if dist < best_dist { best_dist = dist; best = ln; }
            }
            best
        } else {
            start_lines[0]
        };
        let anchor_show = anchor + 1;
        let start = 1.max(anchor_show.saturating_sub(context_lines));
        let end = total_lines.min(anchor_show + context_lines);
        let ctx: Vec<String> = content.lines()
            .skip(start - 1)
            .take(end - start + 1)
            .enumerate()
            .map(|(i, l)| {
                let lineno = start + i;
                let arrow = if lineno == anchor_show { "→" } else { " " };
                format!("{} {:>6}\t{}", arrow, lineno, l)
            })
            .collect();
        body.push_str(&format!(
            "\n起始行第 {} 行附近（±{} 行）的内容：\n{}",
            anchor_show, context_lines, ctx.join("\n"),
        ));
        body.push_str(&format!(
            "\n提示：检查 end 模式 '{}' 是否正确，或确认 start 之后是否存在 end 行。",
            end_pattern,
        ));
    }

    // 截断过长反馈
    const FEEDBACK_TRUNCATE_TOK: usize = 600;
    if let Some((truncated, _)) = truncate_opt(&body, FEEDBACK_TRUNCATE_TOK) {
        if let Some(cd) = console_dir {
            let _ = fs::create_dir_all(cd);
            let seq = BLOCK_EDIT_COUNTER.fetch_add(1, Ordering::Relaxed);
            let out_path = cd.join(format!("block_edit_fail_{seq}.out"));
            let full = format!(
                "block_edit 匹配失败 (file: {})\nstart: {}\nend: {}\n{}\n",
                path, start_pattern, end_pattern, body
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

/// 截断辅助
fn truncate_opt(text: &str, max_tok: usize) -> Option<(String, bool)> {
    let (truncated, was_truncated) = truncate_head_tok(text, max_tok);
    if was_truncated { Some((truncated, true)) } else { None }
}

/// 模糊匹配：Levenshtein 距离找最接近的行
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

/// 归一化 Levenshtein 距离 [0,1]，0=完全相同
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
