//! command — 在 PowerShell 中执行命令
//!
//! Windows 的 PowerShell 输出通常是系统活动代码页编码（如中文 GBK、日文 Shift_JIS），
//! 而非 UTF-8。此模块会自动检测编码并转换为 UTF-8，确保模型正确接收含非 ASCII
//! 字符的命令输出。
//!
//! 输出超过阈值时摘要截断，完整输出写入 console 目录供模型读取。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde_json::Value;

use super::{ToolDef, ToolOutcome};

static CMD_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 截断阈值
const STDOUT_MAX: usize = 4000;
const STDERR_MAX: usize = 2000;

/// 尝试将字节数据解码为 UTF-8。
///
/// 优先尝试 UTF-8（快速路径），失败后依次尝试常见 Windows 代码页编码：
/// - GBK (CP936) —— 中文
/// - Shift_JIS (CP932) —— 日文
/// - EUC-KR (CP949) —— 韩文
/// - Windows-1252 (CP1252) —— 西欧
///
/// 全部失败则回退到 `String::from_utf8_lossy`。
fn decode_to_utf8(bytes: &[u8]) -> String {
    // UTF-8 快速路径 —— 已经是有效 UTF-8 的直接用
    if let Ok(s) = String::from_utf8(bytes.to_vec()) {
        return s;
    }

    // 尝试常见 Windows 代码页，用无替换模式检测编码是否有效
    const WINDOWS_CODEPAGES: &[&encoding_rs::Encoding] = &[
        encoding_rs::GBK,          // CP936 中文
        encoding_rs::SHIFT_JIS,    // CP932 日文
        encoding_rs::EUC_KR,       // CP949 韩文
        encoding_rs::WINDOWS_1252, // CP1252 西欧
    ];

    for encoding in WINDOWS_CODEPAGES {
        if let Some(decoded) = encoding.decode_without_bom_handling_and_without_replacement(bytes) {
            return decoded.into_owned();
        }
    }

    // 兜底：用替换字符替代无效字节
    String::from_utf8_lossy(bytes).to_string()
}

pub fn tool(console_dir: Option<PathBuf>) -> ToolDef {
    ToolDef {
        name: "command",
        description:
            "what: 在 PowerShell 中执行命令。\nwhy: 需要运行脚本、编译、测试等操作时使用。\nhow: 如删除请用 trash 代替。\n注意: 输出超过阈值会截断并保存完整版到 console 目录，可按需 read。",
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
            Box::pin(execute(args, cd))
        }),
    }
}

async fn execute(args: Value, console_dir: Option<PathBuf>) -> Result<ToolOutcome> {
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

    // 使用编码感知解码替代 from_utf8_lossy
    let stdout = decode_to_utf8(&output.stdout);
    let stderr = decode_to_utf8(&output.stderr);

    let exit_code = output.status.code();

    let mut summary = format!("$ {}\n", command);

    let stdout_truncated = stdout.len() > STDOUT_MAX;
    let stderr_truncated = stderr.len() > STDERR_MAX;

    if !stdout.is_empty() {
        if stdout_truncated {
            let seq = CMD_COUNTER.fetch_add(1, Ordering::Relaxed);
            let truncated_summary = format!("{}...\n[已截断, 共 {} 字符]", &stdout[..STDOUT_MAX], stdout.len());
            summary.push_str(&truncated_summary);

            // 保存完整 stdout
            if let Some(ref cd) = console_dir {
                let _ = std::fs::create_dir_all(cd);
                let out_path = cd.join(format!("cmd_{seq}_stdout.out"));
                if let Err(e) = std::fs::write(&out_path, &stdout) {
                    summary.push_str(&format!("\n[警告: 完整输出写入失败 {e}]"));
                } else {
                    summary.push_str(&format!("\n[完整输出: {}]", out_path.display()));
                }
            } else {
                summary.push_str("\n[完整输出未保存, 请缩小命令范围]");
            }
        } else {
            summary.push_str(&stdout);
        }
    }

    if !stderr.is_empty() {
        if !stdout.is_empty() {
            summary.push('\n');
        }
        summary.push_str("stderr:\n");
        if stderr_truncated {
            let seq = CMD_COUNTER.fetch_add(1, Ordering::Relaxed);
            let truncated_summary = format!("{}...\n[已截断, 共 {} 字符]", &stderr[..STDERR_MAX], stderr.len());
            summary.push_str(&truncated_summary);

            if let Some(ref cd) = console_dir {
                let out_path = cd.join(format!("cmd_{seq}_stderr.out"));
                if let Err(e) = std::fs::write(&out_path, &stderr) {
                    summary.push_str(&format!("\n[警告: 完整 stderr 写入失败 {e}]"));
                } else {
                    summary.push_str(&format!("\n[完整 stderr: {}]", out_path.display()));
                }
            }
        } else {
            summary.push_str(&stderr);
        }
    }

    if let Some(code) = exit_code {
        summary.push_str(&format!("\n退出码: {}", code));
    }

    Ok(ToolOutcome {
        summary,
        inverse: None, // command 不可撤销
        rollback: false,
        approval_pending: None,
        inject_messages: vec![],
        defer_rollback: false,
    })
}
