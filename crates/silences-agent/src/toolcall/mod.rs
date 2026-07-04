//! 工具调度中心
//!
//! 注册所有工具、按名称路由、执行。

pub mod glance;
pub mod grep;
pub mod read;
pub mod raw_read;
pub mod edit;
pub mod raw_edit;
pub mod write;
pub mod replace;
pub mod find;
pub mod regret;
pub mod command;
pub mod trash;
pub mod start_task;
pub mod end_task;
pub mod add_task;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;

use crate::queue::TaskQueue;

use anyhow::{Context, Result};
use serde_json::Value;
use tokenizers::Tokenizer;
use tokio::sync::Mutex;

use self::regret::ToolHistory;

/// 写前检查保护：记录已被 read / raw_read 读取过的文件路径。
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
    /// 工具执行后注入到对话中的额外消息（例如 end_task 注入 u_orch）
    pub inject_messages: Vec<silences_core::Message>,
    /// 延迟回退到下一轮（用于 end_task：先注入 inject_messages，等模型更新 CONTEXT.md 后再回退）
    pub defer_rollback: bool,
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

/// 展开 pattern 中的反引号转义区域：
/// `` `literal text` `` → 内部自动 regex::escape（纯文本匹配）
/// `\`` 在反引号区域内表示字面反引号
/// 反引号外的部分保持原样（正则表达式）
///
/// 示例：
/// `` `fn main()`*\n `` → 匹配 "fn main()" 后跟正则 `*\n`
/// `` `def reg():` `` → 匹配字面 "def reg():"
pub fn expand_pattern(pattern: &str) -> String {
    let mut result = String::new();
    let mut chars = pattern.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '`' {
            // 进入纯文本区域：收集到未转义的反引号为止
            let mut literal = String::new();
            loop {
                match chars.next() {
                    None => {
                        // 未闭合的反引号 — 当成字面量处理
                        result.push('`');
                        result.push_str(&literal);
                        break;
                    }
                    Some('`') => {
                        // 纯文本区域结束
                        result.push_str(&regex::escape(&literal));
                        break;
                    }
                    Some('\\') if chars.peek() == Some(&'`') => {
                        // 转义的反引号 \`
                        chars.next();
                        literal.push('`');
                    }
                    Some(c) => {
                        literal.push(c);
                    }
                }
            }
        } else {
            result.push(c);
        }
    }

    result
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
fn get_tokenizer() -> Option<&'static Tokenizer> {
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

/// 大文件自动截断预览。
///
/// 使用 DeepSeek tokenizer 精确计数；tokenizer 不可用时回退字节估算（1 tok ≈ 4 字节）。
/// 用 tokenizer offset 信息在精确的字符边界截断，保留开头 `head_tok` + 结尾 `tail_tok`。
///
/// 返回（截断后的内容, 是否被截断）。
pub fn auto_truncate(
    content: &str,
    threshold_tok: usize,
    head_tok: usize,
    tail_tok: usize,
) -> (String, bool) {
    // ── 用真实 tokenizer 精确计数 ──
    if let Some(tok) = get_tokenizer() {
        if let Ok(enc) = tok.encode(content, true) {
            let total = enc.len();
            if total <= threshold_tok || head_tok + tail_tok >= total {
                return (content.to_owned(), false);
            }

            let head_tok = head_tok.min(total);
            let tail_tok = tail_tok.min(total);

            let offsets = enc.get_offsets();
            let head_end = offsets[head_tok.saturating_sub(1)].1.min(content.len());
            let tail_start = offsets[total - tail_tok].0;
            let tail_start = tail_start.max(head_end).min(content.len());

            let truncated = format!(
                "{}…\n[截断：文件较大 (~{} tok)，仅显示开头 {} tok + 结尾 {} tok]\n…{}",
                &content[..head_end],
                total,
                head_tok,
                tail_tok,
                &content[tail_start..]
            );
            return (truncated, true);
        }
    }

    // ── 回退：字节估算 ──
    let threshold = threshold_tok * 4;
    let head = head_tok * 4;
    let tail = tail_tok * 4;

    if content.len() <= threshold {
        return (content.to_owned(), false);
    }

    let head_end = content.floor_char_boundary(head.min(content.len()));
    let tail_start = content
        .floor_char_boundary(content.len().saturating_sub(tail))
        .max(head_end);

    let truncated = format!(
        "{}…\n[截断：文件较大 (~{}B)，仅显示开头 ~{}tok + 结尾 ~{}tok]\n…{}",
        &content[..head_end],
        content.len(),
        head_tok,
        tail_tok,
        &content[tail_start..]
    );

    (truncated, true)
}

/// 注册所有工具
pub fn all_tools(history: Arc<Mutex<ToolHistory>>, read_tracker: ReadTracker, queue: Arc<TaskQueue>) -> Vec<ToolDef> {
    vec![
        glance::tool(),
        grep::tool(),
        read::tool(read_tracker.clone()),
        raw_read::tool(read_tracker.clone()),
        write::tool(read_tracker),
        edit::tool(),
        raw_edit::tool(),
        replace::tool(),
        find::tool(),
        regret::tool(history),
        command::tool(),
        trash::tool(),
        start_task::tool(queue.clone()),
        end_task::tool(queue.clone()),
        add_task::tool(queue),

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
