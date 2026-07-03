//! 工具调度中心
//!
//! 注册所有工具、按名称路由、执行。

pub mod glance;
pub mod grep;
pub mod read;
pub mod raw_read;
pub mod create;
pub mod edit;
pub mod raw_edit;
pub mod replace;
pub mod find;
pub mod regret;
pub mod command;
pub mod trash;

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use self::regret::ToolHistory;
use tokio::sync::Mutex;

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
}

impl fmt::Debug for ToolOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolOutcome")
            .field("summary", &self.summary)
            .field("inverse", &self.inverse.is_some())
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

/// 注册所有工具
pub fn all_tools(history: Arc<Mutex<ToolHistory>>) -> Vec<ToolDef> {
    vec![
        glance::tool(),
        grep::tool(),
        read::tool(),
        raw_read::tool(),
        create::tool(),
        edit::tool(),
        raw_edit::tool(),
        replace::tool(),
        find::tool(),
        regret::tool(history),
        command::tool(),
        trash::tool(),
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
