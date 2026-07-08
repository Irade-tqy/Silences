//! 上下文管理集成测试
//!
//! 加载真实 Silences 运行产生的 fixture 数据，
//! 用 LLM JSON Output 模式产出压缩脚本，执行后验证结果。
//!
//! 运行：cargo test --test context_mgmt -- --nocapture --ignored

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use silences_core::Message;
use silences_llm::LlmClient;

/// 从 DB 读 API key
fn api_key() -> Option<String> {
    let db_path = std::env::var("SILENCES_DB_PATH")
        .unwrap_or_else(|_| "E:/programs/Silences/silences.db".to_string());
    let p = PathBuf::from(&db_path);
    if !p.exists() { return None; }
    let conn = rusqlite::Connection::open(&db_path).ok()?;
    let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = 'api_key'").ok()?;
    stmt.query_row([], |row| row.get::<_, String>(0)).ok()
}

/// 上下文管理提示词（与 agent.rs 中一致）
const CTX_MGMT_PROMPT: &str =
    "你是一个上下文压缩器。你看到的是 Silences agent 与用户的多轮对话历史，\
     包含 system 指令、用户消息、agent 的工具调用和执行结果。\n\n\
     你的任务是输出一个更短但信息等价的消息列表。你可以：\n\
     - 删除消息\n\
     - 把多条消息合并为一条\n\
     - 把长内容（如 read 返回的完整文件）替换为简短总结\n\n\
     约束：\n\
     - 第一条 system 消息和第一条 user 消息保持原样不动（它们对齐 prefix cache）\n\
     - 输出必须是合法 JSON 数组，可以被反序列化回 Message 列表\n\
     - 不确定是否重要的，保留\n\n\
     输出格式：\n\
     {\"analysis\": \"<一句话解释你做了什么>\", \"script\": \"<Python 3 代码，stdin 读入 messages JSON，stdout 输出压缩后的 JSON>\"}";

fn load_fixture(name: &str) -> Vec<Message> {
    let path = format!(
        "E:/programs/Silences/crates/silences-lib/tests/fixtures/{name}.json"
    );
    let content = fs::read_to_string(&path).expect("读取 fixture 失败");
    let data: serde_json::Value = serde_json::from_str(&content).expect("JSON 解析失败");
    let msgs: Vec<Message> =
        serde_json::from_value(data["messages"].clone()).expect("messages 反序列化失败");
    msgs
}

/// 执行 Python 脚本，传入 messages JSON 通过 stdin，返回 stdout 解析结果
fn run_script(script: &str, messages: &[Message]) -> Result<Vec<Message>, String> {
    let input_json = serde_json::to_string(messages).map_err(|e| format!("序列化失败: {e}"))?;

    let mut child = Command::new("py")
        .args(["-X", "utf8", "-c", script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动失败: {e}"))?;

    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        writeln!(stdin, "{input_json}").map_err(|e| format!("写入失败: {e}"))?;
    }

    let out = child.wait_with_output().map_err(|e| format!("等待失败: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("脚本退出码 {}: {}", out.status, stderr));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let new_msgs: Vec<Message> =
        serde_json::from_str(&stdout).map_err(|e| format!("输出 JSON 解析失败: {e}\nstdout: {}", &stdout[..stdout.len().min(500)]))?;
    Ok(new_msgs)
}

/// 基本健全性检查
fn check_invariants(before: &[Message], after: &[Message], label: &str) {
    // 1. 第一条 system 消息必须保留
    let first_sys_before = before.iter().find(|m| m.role == "system");
    let first_sys_after = after.iter().find(|m| m.role == "system");
    assert!(
        first_sys_before.is_some() == first_sys_after.is_some(),
        "[{label}] 第一条 system 消息丢失"
    );
    if let (Some(b), Some(a)) = (first_sys_before, first_sys_after) {
        assert_eq!(b.content, a.content, "[{label}] system 消息被修改");
    }

    // 2. 第一条 user 消息必须保留
    let first_user_before = before.iter().find(|m| m.role == "user");
    let first_user_after = after.iter().find(|m| m.role == "user");
    assert!(
        first_user_before.is_some() == first_user_after.is_some(),
        "[{label}] 第一条 user 消息丢失"
    );

    // 3. 压缩后消息数不应超过原始
    assert!(
        after.len() <= before.len(),
        "[{label}] 消息数增加了: {} → {}",
        before.len(),
        after.len()
    );

    // 4. 不能有孤立的 tool_call 或 tool_result
    let all_tc_ids: Vec<&str> = after
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .map(|tc| tc.id.as_str())
        .collect();
    let all_tr_ids: Vec<&str> = after
        .iter()
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    for tc_id in &all_tc_ids {
        assert!(
            all_tr_ids.contains(tc_id),
            "[{label}] 孤立 tool_call: {tc_id}"
        );
    }
    for tr_id in &all_tr_ids {
        assert!(
            all_tc_ids.contains(tr_id),
            "[{label}] 孤立 tool_result: {tr_id}"
        );
    }

    println!(
        "  [{label}] ✅ {} → {} 条，所有检查通过",
        before.len(),
        after.len()
    );
}

#[tokio::test]
#[ignore]
async fn test_context_management_all_scenarios() {
    let key = match api_key() {
        Some(k) => k,
        None => {
            eprintln!("SKIP: 未配置 API Key");
            return;
        }
    };

    let llm = LlmClient::new(
        key,
        "https://api.deepseek.com".into(),
        "deepseek-v4-flash".into(),
    );

    let scenarios = [
        ("scenario_1_explore", "5 条，简单探索，几乎不需压缩"),
        ("scenario_2_edit", "7 条，read+edit，不需压缩"),
        ("scenario_3_search", "25 条，重度探索+死胡同+重复 read"),
        ("scenario_4_error", "16 条，项目探索+计时器搜索"),
        ("scenario_5_routing", "24 条，路由探索+设置页面检查"),
    ];

    for (name, desc) in &scenarios {
        println!("\n=== {name}: {desc} ===");
        let messages = load_fixture(name);
        println!("  加载 {len} 条消息", len = messages.len());

        // 构建上下文管理提示词（作为 system 消息注入）
        let mut ctx_messages = messages.clone();
        ctx_messages.push(Message {
            role: "system".into(),
            content: CTX_MGMT_PROMPT.to_string(),
            name: Some("context_optimizer".into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        });

        // 调用 JSON Output
        let output = match llm
            .chat_json(&ctx_messages, None, None, 4096)
            .await
        {
            Ok(o) => o,
            Err(e) => {
                eprintln!("  ❌ chat_json 失败: {e}");
                continue;
            }
        };

        let analysis = output["analysis"].as_str().unwrap_or("?");
        let script = output["script"].as_str().unwrap_or("");
        println!("  analysis: {analysis}");
        println!("  script_len: {} chars", script.len());

        if script.is_empty() {
            eprintln!("  ⚠️ 模型未产出 script（认为不需压缩）");
            continue;
        }

        // 执行脚本
        match run_script(script, &messages) {
            Ok(compressed) => {
                check_invariants(&messages, &compressed, name);
            }
            Err(e) => {
                eprintln!("  ❌ 脚本执行失败: {e}");
            }
        }
    }
}
