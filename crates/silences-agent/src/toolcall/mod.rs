//! 工具调度中心
//!
//! 注册所有工具、按名称路由、执行。

pub mod glance;
pub mod grep;
pub mod read;
pub mod edit;
pub mod block_edit;
pub mod write;
pub mod replace;
pub mod find;
pub mod regret;
pub mod command;
pub mod trash;
pub mod rename;
pub mod checkpoint;
pub mod rollback;
pub mod list_checkpoints;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use crate::checkpoint_stack::CheckpointStack;

use anyhow::{Context, Result};
use serde_json::Value;
use silences_core::ToolLimits;
use tokenizers::Tokenizer;
use tokio::sync::Mutex;

use self::regret::ToolHistory;

/// 写前检查保护：记录已被 read 读取过的文件路径。
/// write 工具覆写文件前必须在此注册表中。
pub type ReadTracker = Arc<Mutex<HashSet<String>>>;

/// 工具定义
pub struct ToolDef {
    pub name: &'static str,
    /// what + why + how 三段式描述（展示给 LLM）
    pub description: &'static str,
    /// JSON Schema（strict 模式）
    pub schema: Value,
    /// 执行函数
    pub handler: Box<dyn Fn(Value) -> BoxFuture<'static, Result<ToolOutcome>> + Send + Sync>,
}

// BoxFuture 别名
use std::fmt;
use std::future::Future;
use std::pin::Pin;
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 工具执行结果
pub struct ToolOutcome {
    /// 展示给用户的摘要
    pub summary: String,
    /// 用于 regret 的逆操作（None 表示不可撤销）
    pub inverse: Option<InverseOp>,
    /// 若为 true，agent loop 在此轮工具全部执行后回退消息到 checkpoint
    pub rollback: bool,
    /// 若设置，表示需要用户审批，值为审批会话 ID
    pub approval_pending: Option<String>,
    /// 工具执行后注入到对话中的额外消息（例如 rollback 注入 orch 指令）
    pub inject_messages: Vec<silences_core::Message>,
    /// 延迟回退到下一轮（用于 rollback：tool result 指示 LLM 更新 CONTEXT.md，下一轮再截断）
    pub defer_rollback: bool,
}

impl ToolOutcome {
    /// 创建一个简单的成功 ToolOutcome（带 summary，其他字段为默认值）
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            inverse: None,
            rollback: false,
            approval_pending: None,
            inject_messages: vec![],
            defer_rollback: false,
        }
    }
}

impl fmt::Debug for ToolOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolOutcome")
            .field("summary", &self.summary)
            .field("inverse", &self.inverse.is_some())
            .field("rollback", &self.rollback)
            .field("approval_pending", &self.approval_pending.is_some())
            .field("inject_messages", &self.inject_messages.len())
            .field("defer_rollback", &self.defer_rollback)
            .finish()
    }
}

/// 逆操作 —— 每个工具在自己的文件中构造逆操作闭包
pub struct InverseOp {
    pub description: String,
    apply: Box<dyn Fn() -> Result<String> + Send + Sync>,
}

impl InverseOp {
    pub fn new<F>(description: String, apply: F) -> Self
    where
        F: Fn() -> Result<String> + Send + Sync + 'static,
    {
        Self { description, apply: Box::new(apply) }
    }

    pub fn apply(&self) -> Result<String> {
        (self.apply)()
    }
}

const TABSENSITIVE_WARNING: &str =
    "⚠ Makefile / Dockerfile 检测到：已保留原始 Tab 格式，未做标准化。如需标准化编辑请使用 edit。";

/// 文件名是否属于 tab 敏感类型（Makefile recipe、Dockerfile heredoc 等依赖行首 tab）
pub fn is_tabsensitive(path: &str) -> bool {
    let fname = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    fname.eq_ignore_ascii_case("makefile")
        || fname.eq_ignore_ascii_case("gnumakefile")
        || fname.eq_ignore_ascii_case("dockerfile")
        || fname.ends_with(".mk")
}

/// \r\n → \n，行首连续 tab → 4 空格
pub fn normalize(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    s.split('\n')
        .map(|line| {
            let tabs = line.len() - line.trim_start_matches('\t').len(); // 行首 tab 字节数
            let rest = &line[tabs..];
            format!("{}{}", "    ".repeat(tabs), rest)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 编码鲁棒的文件读取函数。
///
/// 优先尝试 UTF-8（快速路径），失败后依次尝试常见 Windows 代码页编码：
/// GBK (CP936), Shift_JIS (CP932), EUC-KR (CP949), Windows-1252 (CP1252)。
/// 全部失败则回退到 `from_utf8_lossy`。
pub fn read_file_robust(path: &str) -> Result<String, anyhow::Error> {
    let bytes = std::fs::read(path).context("读取文件失败")?;

    // UTF-8 快速路径
    if let Ok(s) = std::str::from_utf8(&bytes) {
        return Ok(s.to_owned());
    }

    // 尝试常见 Windows 代码页
    const WINDOWS_CODEPAGES: &[&encoding_rs::Encoding] = &[
        encoding_rs::GBK,
        encoding_rs::SHIFT_JIS,
        encoding_rs::EUC_KR,
        encoding_rs::WINDOWS_1252,
    ];

    for encoding in WINDOWS_CODEPAGES {
        if let Some(decoded) =
            encoding.decode_without_bom_handling_and_without_replacement(&bytes)
        {
            return Ok(decoded.into_owned());
        }
    }

    // 兜底
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// 惰性加载 DeepSeek tokenizer。
/// 搜索顺序：SILENCES_TOKENIZER_PATH 环境变量 → 本项目已知相对路径。
pub(super) fn get_tokenizer() -> Option<&'static Tokenizer> {
    static TOKENIZER: OnceLock<Option<Tokenizer>> = OnceLock::new();
    TOKENIZER
        .get_or_init(|| {
            let candidates: &[&str] = &[
                // 环境变量覆盖
                // 运行时常见路径
                "./tokenizer/tokenizer.json",
                "../tokenizer/tokenizer.json",
                "../../tokenizer/tokenizer.json",
            ];

            // 优先环境变量
            let mut paths: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(env_path) = std::env::var("SILENCES_TOKENIZER_PATH") {
                paths.push(std::path::PathBuf::from(env_path));
            }
            for c in candidates {
                paths.push(std::path::PathBuf::from(c));
            }

            for p in &paths {
                if p.exists() {
                    match Tokenizer::from_file(p) {
                        Ok(t) => {
                            eprintln!("[toolcall] loaded tokenizer from {}", p.display());
                            return Some(t);
                        }
                        Err(e) => {
                            eprintln!("[toolcall] 警告: tokenizer 加载失败 {}: {e}", p.display());
                        }
                    }
                }
            }
            eprintln!("[toolcall] 警告: 未找到 tokenizer.json，回退字节估算");
            None
        })
        .as_ref()
}

/// 截断字符串到前 max_tok 个 token。
/// 使用 DeepSeek tokenizer 精确计数；回退字节估算（1 tok ≈ 4 字节）。
/// 返回（截断后的字符串, 是否被截断）。
pub(super) fn truncate_head_tok(content: &str, max_tok: usize) -> (String, bool) {
    if let Some(tok) = get_tokenizer() {
        if let Ok(enc) = tok.encode(content, true) {
            if enc.len() <= max_tok {
                return (content.to_owned(), false);
            }
            let offsets = enc.get_offsets();
            let end = offsets[max_tok.saturating_sub(1)].1.min(content.len());
            return (content[..end].to_owned(), true);
        }
    }
    // 回退：字节估算
    let max_bytes = max_tok * 4;
    if content.len() <= max_bytes {
        return (content.to_owned(), false);
    }
    let end = content.floor_char_boundary(max_bytes.min(content.len()));
    (content[..end].to_owned(), true)
}

/// 注册所有工具
pub fn all_tools(
    history: Arc<Mutex<ToolHistory>>,
    read_tracker: ReadTracker,
    cp_stack: Arc<CheckpointStack>,
    session_dir: Option<PathBuf>,
    limits: ToolLimits,
) -> Vec<ToolDef> {
    // grep 等工具的 console 输出目录
    let console_dir = session_dir.as_ref().map(|d| d.join("console"));
    vec![
        glance::tool(console_dir.clone(), limits),
        grep::tool(console_dir.clone(), limits),
        read::tool(read_tracker.clone()),
        write::tool(),
        edit::tool(console_dir.clone(), limits),
        block_edit::tool(console_dir.clone(), limits),
        replace::tool(console_dir.clone(), limits),
        find::tool(console_dir.clone(), limits),
        regret::tool(history),
        command::tool(console_dir, limits),
        trash::tool(),
        rename::tool(),
        checkpoint::tool(cp_stack.clone()),
        rollback::tool(cp_stack.clone()),
        list_checkpoints::tool(cp_stack),
    ]
}

/// 构建 API 格式的 tools 参数
pub fn build_api_tools(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "strict": true,
                    "parameters": t.schema,
                }
            })
        })
        .collect()
}

/// 按名称查找并执行工具
pub async fn execute_tool(
    tools: &[ToolDef],
    name: &str,
    args: Value,
) -> Result<ToolOutcome> {
    let tool = tools.iter().find(|t| t.name == name).ok_or_else(|| {
        anyhow::anyhow!("未知工具: {name}")
    })?;
    (tool.handler)(args).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_tool(name: &'static str) -> ToolDef {
        ToolDef {
            name,
            description: "test tool",
            schema: serde_json::json!({"type": "object", "properties": {}}),
            handler: Box::new(|_| Box::pin(async { Ok(ToolOutcome::new("ok")) })),
        }
    }

    // ── ToolOutcome ──

    #[test]
    fn test_tool_outcome_new() {
        let outcome = ToolOutcome::new("hello");
        assert_eq!(outcome.summary, "hello");
        assert!(outcome.inverse.is_none());
        assert!(!outcome.rollback);
        assert!(outcome.approval_pending.is_none());
        assert!(outcome.inject_messages.is_empty());
        assert!(!outcome.defer_rollback);
    }

    // ── InverseOp ──

    #[test]
    fn test_inverse_op_new_and_apply() {
        let op = InverseOp::new("test undo".into(), || Ok("result".into()));
        assert_eq!(op.description, "test undo");
        assert_eq!(op.apply().unwrap(), "result");
    }

    #[test]
    fn test_inverse_op_apply_error() {
        let op = InverseOp::new("failing".into(), || {
            Err(anyhow::anyhow!("something went wrong"))
        });
        let err = op.apply().unwrap_err();
        assert!(err.to_string().contains("something went wrong"));
    }

    // ── is_tabsensitive ──

    #[test]
    fn test_is_tabsensitive_makefile() {
        assert!(is_tabsensitive("Makefile"));
        assert!(is_tabsensitive("makefile"));
        assert!(is_tabsensitive("GNUmakefile"));
        assert!(is_tabsensitive("gnumakefile"));
    }

    #[test]
    fn test_is_tabsensitive_dockerfile() {
        assert!(is_tabsensitive("Dockerfile"));
        assert!(is_tabsensitive("dockerfile"));
    }

    #[test]
    fn test_is_tabsensitive_dot_mk() {
        assert!(is_tabsensitive("build.mk"));
        assert!(is_tabsensitive("rules.mk"));
    }

    #[test]
    fn test_is_tabsensitive_negative() {
        assert!(!is_tabsensitive("main.rs"));
        assert!(!is_tabsensitive("README.md"));
        assert!(!is_tabsensitive("Cargo.toml"));
    }

    // ── normalize ──

    #[test]
    fn test_normalize_crlf_to_lf() {
        assert_eq!(normalize("a\r\nb\r\nc"), "a\nb\nc");
    }

    #[test]
    fn test_normalize_tabs_to_spaces() {
        assert_eq!(normalize("\tHello"), "    Hello");
        assert_eq!(normalize("\t\tIndented"), "        Indented");
    }

    #[test]
    fn test_normalize_mixed() {
        assert_eq!(
            normalize("\tfoo\r\n\t\tbar\r\nbaz"),
            "    foo\n        bar\nbaz"
        );
    }

    #[test]
    fn test_normalize_no_tabs_no_crlf() {
        assert_eq!(normalize("hello\nworld"), "hello\nworld");
    }

    #[test]
    fn test_normalize_empty() {
        assert_eq!(normalize(""), "");
    }

    // ── build_api_tools ──

    #[test]
    fn test_build_api_tools_names() {
        let tools = vec![dummy_tool("read"), dummy_tool("edit"), dummy_tool("grep")];
        let api = build_api_tools(&tools);

        assert_eq!(api.len(), 3);
        assert_eq!(api[0]["function"]["name"], "read");
        assert_eq!(api[1]["function"]["name"], "edit");
        assert_eq!(api[2]["function"]["name"], "grep");
    }

    #[test]
    fn test_build_api_tools_structure() {
        let tools = vec![dummy_tool("test")];
        let api = build_api_tools(&tools);

        assert_eq!(api[0]["type"], "function");
        assert_eq!(api[0]["function"]["strict"], true);
        assert_eq!(api[0]["function"]["description"], "test tool");
        assert!(api[0]["function"]["parameters"].is_object());
    }

    #[test]
    fn test_build_api_tools_empty() {
        let api = build_api_tools(&[]);
        assert!(api.is_empty());
    }

    // ── truncate_head_tok ──

    #[test]
    fn test_truncate_head_tok_under_limit() {
        let content = "hello world";
        let (result, truncated) = truncate_head_tok(content, 100);
        assert!(!truncated);
        assert_eq!(result, content);
    }

    #[test]
    fn test_truncate_head_tok_over_limit() {
        let content = "hello world this is a very long string that should definitely exceed any reasonable token limit for this test! ".repeat(20);
        let (result, truncated) = truncate_head_tok(&content, 3);
        assert!(truncated);
        assert!(result.len() < content.len());
        assert!(content.starts_with(&result));
    }

    #[test]
    fn test_truncate_head_tok_exact_limit() {
        // Very short content that won't exceed any tokenizer threshold
        let content = "a";
        let (result, truncated) = truncate_head_tok(content, 3);
        assert!(!truncated);
        assert_eq!(result, content);
    }
}
