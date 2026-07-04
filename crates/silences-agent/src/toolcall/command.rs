//! command — 在 PowerShell 中执行命令
//!
//! stdout/stderr 按 token 数截断后保存完整输出到 session 专属的 console/ 目录，
//! 模型可通过绝对路径读取完整文件。
//!
//! Windows 的 PowerShell 输出通常是系统活动代码页编码（如中文 GBK、日文 Shift_JIS），
//! 而非 UTF-8。此模块会自动检测编码并转换为 UTF-8，确保模型正确接收含非 ASCII
//! 字符的命令输出。

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::Value;

use super::{truncate_head_tok, ToolDef, ToolOutcome};
use silences_core::ToolLimits;

/// 尝试将字节数据解码为 UTF-8。
fn decode_to_utf8(bytes: &[u8]) -> String {
    if let Ok(s) = String::from_utf8(bytes.to_vec()) {
        return s;
    }
    const WINDOWS_CODEPAGES: &[&encoding_rs::Encoding] = &[
        encoding_rs::GBK,
        encoding_rs::SHIFT_JIS,
        encoding_rs::EUC_KR,
        encoding_rs::WINDOWS_1252,
    ];
    for encoding in WINDOWS_CODEPAGES {
        if let Some(decoded) = encoding.decode_without_bom_handling_and_without_replacement(bytes) {
            return decoded.into_owned();
        }
    }
    String::from_utf8_lossy(bytes).to_string()
}

pub fn tool(console_dir: Option<PathBuf>, limits: ToolLimits) -> ToolDef {
    ToolDef {
        name: "command",
        description:
            "what: 在 PowerShell 中执行命令。\nwhy: 需要运行脚本、编译、测试等操作时使用。\nhow: 如删除请用 trash 代替。[不可撤销]",
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的 PowerShell 命令"
                },
                "work_dir": {
                    "type": "string",
                    "description": "工作目录的绝对路径（可选，默认项目根目录）"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
        handler: Box::new(move |args| {
            let cd = console_dir.clone();
            Box::pin(execute(args, cd, limits))
        }),
    }
}

async fn execute(args: Value, console_dir: Option<PathBuf>, limits: ToolLimits) -> Result<ToolOutcome> {
    let command = args["command"].as_str().context("缺少 command 参数")?;
    let work_dir = args["work_dir"].as_str().map(|s| s.to_string());

    let output = tokio::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(work_dir.as_deref().unwrap_or("."))
        .output()
        .await
        .context("执行命令失败")?;

    let stdout = decode_to_utf8(&output.stdout);
    let stderr = decode_to_utf8(&output.stderr);

    let stdout_max = limits.command_stdout_max_tok;
    let stderr_max = limits.command_stderr_max_tok;

    let (summary_stdout, stdout_truncated) = truncate_head_tok(&stdout, stdout_max);
    let summary_stdout = if stdout_truncated {
        let path = save_console_file(&console_dir, command, "stdout", &stdout);
        format!(
            "{}...\n[stdout 仅显示前 {stdout_max} tok，完整内容 ({len}B) 已保存至 {path}]\n",
            summary_stdout,
            len = stdout.len(),
        )
    } else {
        stdout.clone()
    };

    let (summary_stderr, stderr_truncated) = truncate_head_tok(&stderr, stderr_max);
    let summary_stderr = if stderr_truncated {
        let path = save_console_file(&console_dir, command, "stderr", &stderr);
        format!(
            "{}...\n[stderr 仅显示前 {stderr_max} tok，完整内容 ({len}B) 已保存至 {path}]\n",
            summary_stderr,
            len = stderr.len(),
        )
    } else {
        stderr.clone()
    };

    let mut summary = format!("$ {}\n", command);
    if !summary_stdout.is_empty() {
        summary.push_str(&summary_stdout);
    }
    if !summary_stderr.is_empty() {
        if !summary_stdout.is_empty() {
            summary.push('\n');
        }
        summary.push_str(&format!("stderr:\n{}", summary_stderr));
    }
    if let Some(code) = output.status.code() {
        summary.push_str(&format!("\n退出码: {}", code));
    }

    Ok(ToolOutcome {
        summary,
        inverse: None,
        rollback: false,
        approval_pending: None,
        inject_messages: vec![],
        defer_rollback: false,
    })
}

fn timestamp() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let millis = dur.subsec_millis();
    format!("{}_{:03}", secs, millis)
}

/// 保存完整输出到 console/{ts}_{tag}_{stream}.out，返回绝对路径
fn save_console_file(console_dir: &Option<PathBuf>, command: &str, stream: &str, content: &str) -> String {
    let dir = match console_dir {
        Some(d) => d.clone(),
        None => return "（未保存，无会话目录）".into(),
    };
    let _ = std::fs::create_dir_all(&dir);
    let ts = timestamp();
    let tag: String = command
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .take(40)
        .collect();
    let tag = if tag.is_empty() { "cmd".to_string() } else { tag };
    let file_path = dir.join(format!("{}_{}_{}.out", ts, tag, stream));
    let _ = std::fs::write(&file_path, content);
    file_path.to_string_lossy().to_string()
}
