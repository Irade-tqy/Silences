//! Silences 后端入口

use std::env;

use anyhow::Result;
use silences_db::Db;
use silences_llm::LlmClient;

#[tokio::main]
async fn main() -> Result<()> {
    // 数据库路径
    let db_path = env::var("SILENCES_DB_PATH")
        .unwrap_or_else(|_| "silences.db".to_string());
    let db = Db::open(&db_path)?;

    // 先从 DB 读 API key，再 fallback 到环境变量；都没有也能启动，用户可在设置页面配置
    let env_api_key = env::var("DEEPSEEK_API_KEY").ok();
    let db_api_key = db.get_setting("api_key").ok().flatten();
    let api_key = db_api_key.or(env_api_key).unwrap_or_default();
    if api_key.is_empty() {
        eprintln!("[silences-server] 警告: 未配置 DEEPSEEK_API_KEY，请在设置页面中添加");
    }
    let base_url = env::var("DEEPSEEK_BASE_URL")
        .unwrap_or_else(|_| "https://api.deepseek.com".to_string());
    let model = env::var("DEEPSEEK_MODEL")
        .unwrap_or_else(|_| "deepseek-v4-flash".to_string());
    let bind = env::var("SILENCES_BIND")
        .unwrap_or_else(|_| "127.0.0.1:0412".to_string());
    let max_context = env::var("SILENCES_MAX_CONTEXT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);

    let mut llm = LlmClient::new(api_key, base_url, model);
    // 调试日志目录（默认项目根目录，写入 api_debug.json）
    let debug_dir = env::var("SILENCES_DEBUG_DIR").unwrap_or_else(|_| ".".to_string());
    let path = std::path::PathBuf::from(&debug_dir);
    llm = llm.with_debug_dir(path);
    let debug_dir_display = if debug_dir == "." { "当前目录" } else { &debug_dir };
    eprintln!("[silences-server] API 调试日志目录: {debug_dir_display} -> api_debug.json");
    // 尝试加载 tokenizer（用于缓存 padding）
    let tokenizer_path = env::var("SILENCES_TOKENIZER")
        .unwrap_or_else(|_| "tokenizer/tokenizer.json".to_string());
    if std::path::Path::new(&tokenizer_path).exists() {
        llm = llm.with_tokenizer(&tokenizer_path);
    }

    // 项目根目录（可选，不设置则默认使用 cwd）
    let project_root = env::var("SILENCES_PROJECT_ROOT")
        .ok()
        .map(std::path::PathBuf::from);

    silences_server::serve(llm, db, &bind, max_context, project_root).await?;
    Ok(())
}
