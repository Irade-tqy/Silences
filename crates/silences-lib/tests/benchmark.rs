//! AgentBench 集成测试（Scenario A + B）
//!
//! 使用真实的 DeepSeek API 和 dailyPlanner worktree，
//! 验证 Silences lib 模式能完整处理多场景任务。
//!
//! 运行方式：
//!   DEEPSEEK_API_KEY=sk-xxx cargo test --test benchmark -- --nocapture
//!
//! 跳过方式（无 API key 时自动跳过）：
//!   cargo test --test benchmark
//!
//! 输出：
//!   bench-record/benchmark-{timestamp}.json  — 完整记录

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use silences_lib::{Silences, SilencesConfig};

fn api_key() -> Option<String> {
    // 优先从 DB 读取（DB 中维护了最新的有效 key）
    let db_path = std::env::var("SILENCES_DB_PATH")
        .unwrap_or_else(|_| "E:/programs/Silences/silences.db".to_string());
    let p = std::path::PathBuf::from(&db_path);
    if p.exists() {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            if let Ok(mut stmt) = conn.prepare("SELECT value FROM settings WHERE key = 'api_key'") {
                if let Ok(key) = stmt.query_row([], |row| row.get::<_, String>(0)) {
                    if !key.is_empty() { return Some(key); }
                }
            }
        }
    }
    // 再尝试环境变量
    std::env::var("DEEPSEEK_API_KEY").ok().filter(|k| !k.is_empty())
}

fn worktree_path() -> PathBuf {
    PathBuf::from(
        std::env::var("BENCH_WORKTREE")
            .unwrap_or_else(|_| "E:/Programs/dailyPlanner-001".to_string()),
    )
}

fn load_system_prompt() -> Option<String> {
    let db_path = std::env::var("SILENCES_DB_PATH")
        .unwrap_or_else(|_| "E:/programs/Silences/silences.db".to_string());
    let p = PathBuf::from(&db_path);
    if !p.exists() { return None; }
    let conn = rusqlite::Connection::open(&db_path).ok()?;
    let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = 'system_prompt'").ok()?;
    stmt.query_row([], |row| row.get::<_, String>(0)).ok()
}

fn save_record(filename: &str, data: &serde_json::Value) -> std::io::Result<PathBuf> {
    let out_dir = PathBuf::from("bench-record");
    fs::create_dir_all(&out_dir)?;
    let path = out_dir.join(filename);
    let json = serde_json::to_string_pretty(data)?;
    fs::write(&path, json)?;
    Ok(path)
}

fn reset_worktree(worktree: &PathBuf) {
    let status = std::process::Command::new("git")
        .args(["checkout", "--", "."])
        .current_dir(worktree)
        .status()
        .expect("git checkout 失败");
    assert!(status.success(), "重置 worktree 失败");
}

fn check_rollback(messages: &[impl AsRef<str>]) -> bool {
    messages.iter().any(|m| {
        let s = m.as_ref();
        s.contains("rollback") || s.contains("regret")
    })
}

#[tokio::test]
#[ignore]
async fn benchmark_scenarios() {
    let api_key = match api_key() {
        Some(k) => k,
        None => { eprintln!("SKIP: DEEPSEEK_API_KEY 未设置"); return; }
    };

    let worktree = worktree_path();
    assert!(worktree.exists(), "worktree 不存在: {:?}", worktree);

    let system_prompt = load_system_prompt();
    eprintln!("系统提示词: {}", if system_prompt.is_some() { "已加载" } else { "未加载" });
    eprintln!("Worktree: {:?}", worktree);

    let silences = Silences::new(SilencesConfig {
        db_path: ":memory:".to_string(),
        api_key,
        base_url: None,
        model: Some("deepseek-v4-flash".to_string()),
        system_prompt,
        project_root: Some(worktree.clone()),
        tool_limits: None,
        warmup_enabled: false,
    }).expect("创建 Silences 实例失败");

    let orig_cwd = std::env::current_dir().ok();
    std::env::set_current_dir(&worktree).ok();
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

    // ===== Scenario A: Debug — 修复 2 个 Pomodoro 计时 bug =====
    eprintln!("\n=== Scenario A: 修复 Pomodoro 计时 bug ===");
    let sid_a = silences.create_session().await.expect("创建 session A 失败");
    let prompt_a = "我用番茄钟，切换了一些页面后切回去，发现系统时钟过了 5 分钟，它才进行了 1 分钟。修一下。";

    let result_a = silences.process_turn(&sid_a, prompt_a).await;
    let a = match result_a {
        Ok(r) => r,
        Err(e) => { reset_worktree(&worktree); std::env::set_current_dir(orig_cwd.unwrap()).ok(); panic!("Scenario A 失败: {e}"); }
    };

    let a_used_rollback = check_rollback(
        &a.messages.iter().map(|m| format!("{}: {}", m.role, m.content)).collect::<Vec<_>>()
    );
    let a_tools: Vec<_> = a.messages.iter().filter_map(|m| {
        if m.role == "assistant" { m.tool_calls.as_ref().map(|tc| tc.iter().map(|t| t.function.name.clone()).collect::<Vec<_>>()) } else { None }
    }).collect();

    println!("Scenario A tokens: in={}, out={}, cache={}",
        a.usage.as_ref().map_or(0, |u| u.input_tokens),
        a.usage.as_ref().map_or(0, |u| u.output_tokens),
        a.usage.as_ref().map_or(0, |u| u.cache_hit_tokens));
    println!("Scenario A rollback: {}", if a_used_rollback { "✅" } else { "❌" });
    println!("Scenario A 工具序列: {:?}", a_tools);
    println!("Scenario A 回复末尾: {}",
        a.reply.chars().rev().take(200).collect::<String>().chars().rev().collect::<String>());

    // ===== 重置 worktree 到初始状态 =====
    reset_worktree(&worktree);

    // ===== Scenario B: Feature — 重实现模板功能 =====
    eprintln!("\n=== Scenario B: 重实现模板功能 ===");
    let sid_b = silences.create_session().await.expect("创建 session B 失败");
    let prompt_b = "另外帮我写一个模版功能。就是在创建事件的时候用模版添加，然后增加次数自动增加时间，名字自动变成「模版名 x 次」，备注使用和模版相同的，并且能追踪每个模版的创建个数。默认三个模版：练字（20min），阅读（30min），外出（45min）。用户可以自己添加。";

    let result_b = silences.process_turn(&sid_b, prompt_b).await;
    let b = match result_b {
        Ok(r) => r,
        Err(e) => { reset_worktree(&worktree); std::env::set_current_dir(orig_cwd.unwrap()).ok(); panic!("Scenario B 失败: {e}"); }
    };

    let b_used_rollback = check_rollback(
        &b.messages.iter().map(|m| format!("{}: {}", m.role, m.content)).collect::<Vec<_>>()
    );
    let b_tools: Vec<_> = b.messages.iter().filter_map(|m| {
        if m.role == "assistant" { m.tool_calls.as_ref().map(|tc| tc.iter().map(|t| t.function.name.clone()).collect::<Vec<_>>()) } else { None }
    }).collect();

    println!("Scenario B tokens: in={}, out={}, cache={}",
        b.usage.as_ref().map_or(0, |u| u.input_tokens),
        b.usage.as_ref().map_or(0, |u| u.output_tokens),
        b.usage.as_ref().map_or(0, |u| u.cache_hit_tokens));
    println!("Scenario B rollback: {}", if b_used_rollback { "✅" } else { "❌" });
    println!("Scenario B 工具序列: {:?}", b_tools);
    println!("Scenario B 回复末尾: {}",
        b.reply.chars().rev().take(200).collect::<String>().chars().rev().collect::<String>());

    // ===== 清理 =====
    std::env::set_current_dir(&orig_cwd.unwrap()).ok();
    reset_worktree(&worktree);

    // ===== 保存完整记录 =====
    let record = serde_json::json!({
        "timestamp_sec": ts,
        "worktree": worktree.to_string_lossy(),
        "scenarios": [
            {
                "name": "A (Debug)",
                "prompt": prompt_a,
                "reply": a.reply,
                "messages": a.messages,
                "usage": a.usage,
                "used_rollback": a_used_rollback,
            },
            {
                "name": "B (Feature)",
                "prompt": prompt_b,
                "reply": b.reply,
                "messages": b.messages,
                "usage": b.usage,
                "used_rollback": b_used_rollback,
            },
        ]
    });
    let record_path = save_record(&format!("benchmark-{ts}.json"), &record).expect("保存记录失败");
    println!("\n完整记录保存到: {:?}", record_path);

    // 报告结果
    println!("\n===== 结果汇总 =====");
    println!("Scenario A (Debug)    rollback: {}  tokens: {}",
        if a_used_rollback { "✅" } else { "❌" },
        a.usage.as_ref().map_or(0, |u| u.input_tokens + u.output_tokens));
    println!("Scenario B (Feature)  rollback: {}  tokens: {}",
        if b_used_rollback { "✅" } else { "❌" },
        b.usage.as_ref().map_or(0, |u| u.input_tokens + u.output_tokens));
}
