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
    // 调试日志目录（可选）
    if let Ok(debug_dir) = env::var("SILENCES_DEBUG_DIR") {
        let path = std::path::PathBuf::from(&debug_dir);
        llm = llm.with_debug_dir(path);
        eprintln!("[silences-server] API 调试日志目录: {debug_dir}");
    }
    // 尝试加载 tokenizer（用于缓存 padding）
    let tokenizer_path = env::var("SILENCES_TOKENIZER")
        .unwrap_or_else(|_| "tokenizer/tokenizer.json".to_string());
    if std::path::Path::new(&tokenizer_path).exists() {
        llm = llm.with_tokenizer(&tokenizer_path);
    }

    // 从 DB 加载 system prompt 到内存（serve 函数内会从 DB 读取并设置 system_prompt 状态）
    // serve 已经接收 db 引用，启动时自动加载
    silences_server::serve(llm, db, &bind, max_context).await?;
    Ok(())
}
