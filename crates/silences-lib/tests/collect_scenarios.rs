//! 场景采集测试 — 运行真实的 process_turn 并保存 messages 到 fixtures/
//!
//! 每个 scenario 独立运行：重置 worktree，新建 session，发一条 prompt，
//! 等待 agent 自然结束（只跑一轮 process_turn），然后保存 messages JSON。
//!
//! 运行：cargo test --test collect_scenarios -- --nocapture --ignored
//!
//! 输出：crates/silences-lib/tests/fixtures/scenario_1_explore.json
//!       crates/silences-lib/tests/fixtures/scenario_2_edit.json
//!       crates/silences-lib/tests/fixtures/scenario_3_search.json
//!       crates/silences-lib/tests/fixtures/scenario_4_error.json
//!       crates/silences-lib/tests/fixtures/scenario_5_routing.json

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use silences_lib::{Silences, SilencesConfig};

// ─── Helpers ──────────────────────────────────────────────────────────────

fn get_api_key() -> Option<String> {
    let db_path = std::env::var("SILENCES_DB_PATH")
        .unwrap_or_else(|_| "E:/programs/Silences/silences.db".to_string());
    let p = PathBuf::from(&db_path);
    if p.exists() {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            if let Ok(mut stmt) = conn.prepare("SELECT value FROM settings WHERE key = 'api_key'") {
                if let Ok(key) = stmt.query_row([], |row| row.get::<_, String>(0)) {
                    if !key.is_empty() {
                        return Some(key);
                    }
                }
            }
        }
    }
    std::env::var("DEEPSEEK_API_KEY").ok().filter(|k| !k.is_empty())
}

fn load_system_prompt() -> Option<String> {
    let db_path = std::env::var("SILENCES_DB_PATH")
        .unwrap_or_else(|_| "E:/programs/Silences/silences.db".to_string());
    let p = PathBuf::from(&db_path);
    if !p.exists() {
        return None;
    }
    let conn = rusqlite::Connection::open(&db_path).ok()?;
    let mut stmt = conn
        .prepare("SELECT value FROM settings WHERE key = 'system_prompt'")
        .ok()?;
    stmt.query_row([], |row| row.get::<_, String>(0)).ok()
}

fn worktree_path() -> PathBuf {
    PathBuf::from(
        std::env::var("BENCH_WORKTREE")
            .unwrap_or_else(|_| "E:/Programs/dailyPlanner-001".to_string()),
    )
}

fn reset_worktree(worktree: &PathBuf) {
    let s = Command::new("git")
        .args(["checkout", "--", "."])
        .current_dir(worktree)
        .status()
        .expect("git checkout 失败");
    assert!(s.success(), "重置 worktree 失败");
    let _ = std::fs::remove_dir_all(worktree.join(".silences"));
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

/// 保存 messages，如果超过 30 条则截断到前 25 条
fn save_messages(filename: &str, messages: &[serde_json::Value]) {
    let dir = fixtures_dir();
    fs::create_dir_all(&dir).unwrap();

    let truncated: Vec<_> = if messages.len() > 30 {
        messages.iter().take(25).cloned().collect()
    } else {
        messages.to_vec()
    };

    let output = serde_json::json!({
        "messages": truncated,
        "total_count": messages.len(),
        "truncated": messages.len() > 30,
    });

    let path = dir.join(filename);
    fs::write(&path, serde_json::to_string_pretty(&output).unwrap()).unwrap();
    eprintln!("  保存: {:?}", path);
}

/// 打印 token 用量（从 TurnResult 的 usage 字段序列化后读取）
fn print_usage(usage_value: &serde_json::Value) {
    if let Some(input) = usage_value["input_tokens"].as_u64() {
        eprintln!(
            "  Token: in={}, out={}, cache_hit={}",
            input,
            usage_value["output_tokens"].as_u64().unwrap_or(0),
            usage_value["cache_hit_tokens"].as_u64().unwrap_or(0),
        );
    }
}

// ─── Scenario runner ─────────────────────────────────────────────────────

async fn run_scenario(
    silences: &Silences,
    worktree: &PathBuf,
    prompt: &str,
    scenario_name: &str,
    output_filename: &str,
) {
    eprintln!("\n===== {} =====", scenario_name);
    eprintln!("Prompt: {prompt}");

    // 重置 worktree
    reset_worktree(worktree);

    // 新建 session
    let session_id = silences.create_session().await.expect("创建 session 失败");

    // 切换到 worktree 目录
    let orig_cwd = std::env::current_dir().ok();
    std::env::set_current_dir(worktree).unwrap();

    let t0 = std::time::Instant::now();
    match silences.process_turn(&session_id, prompt).await {
        Ok(result) => {
            let elapsed = t0.elapsed();
            eprintln!("  耗时: {:.1}s", elapsed.as_secs_f64());
            eprintln!("  消息数: {}", result.messages.len());

            // 序列化 messages 和 usage（不直接命名私有类型）
            let msgs_json = serde_json::to_value(&result.messages).unwrap_or_default();
            let usage_json = serde_json::to_value(&result.usage).unwrap_or_default();
            print_usage(&usage_json);

            eprintln!(
                "  回复末尾: {}",
                result
                    .reply
                    .chars()
                    .rev()
                    .take(200)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            );

            // msgs_json 是一个 JSON 数组，转换为 Vec<Value>
            let msgs: Vec<serde_json::Value> = msgs_json
                .as_array()
                .map(|a| a.clone())
                .unwrap_or_default();
            save_messages(output_filename, &msgs);
        }
        Err(e) => {
            eprintln!("  ❌ 错误: {e:#}");
        }
    }

    // 恢复 CWD
    if let Some(c) = orig_cwd {
        let _ = std::env::set_current_dir(c);
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

/// 所有场景在一个测试函数中顺序执行，避免并行 git checkout 冲突
#[tokio::test]
#[ignore]
async fn collect_all_scenarios() {
    let api_key = match get_api_key() {
        Some(k) => k,
        None => {
            eprintln!("SKIP: 未配置 API Key");
            return;
        }
    };
    let system_prompt = load_system_prompt();
    let worktree = worktree_path();
    assert!(worktree.exists(), "worktree 不存在: {:?}", worktree);

    eprintln!("系统提示词: {}", if system_prompt.is_some() { "已加载" } else { "未加载" });
    eprintln!("Worktree: {:?}", worktree);
    eprintln!("Fixtures 目录: {:?}", fixtures_dir());

    let silences = Silences::new(SilencesConfig {
        db_path: ":memory:".to_string(),
        api_key,
        base_url: None,
        model: Some("deepseek-v4-flash".to_string()),
        system_prompt,
        project_root: Some(worktree.clone()),
        tool_limits: None,
        warmup_enabled: false,
        debug_dir: None,
    })
    .expect("创建 Silences 失败");

    // 场景 1: 工具调用错误恢复
    run_scenario(
        &silences,
        &worktree,
        "看一下 components/pomodoro/PomodoroTimer.tsx 里的 tick 函数",
        "场景 1: 工具调用错误恢复",
        "scenario_1_explore.json",
    )
    .await;

    // 场景 2: 简单修复
    run_scenario(
        &silences,
        &worktree,
        "在 components/pomodoro/PomodoroTimer.tsx 的第 1 行加一个注释 // Pomodoro timer component",
        "场景 2: 简单修复",
        "scenario_2_edit.json",
    )
    .await;

    // 场景 3: 探索后定位
    run_scenario(
        &silences,
        &worktree,
        "找到项目里所有和计时器相关的代码",
        "场景 3: 探索后定位",
        "scenario_3_search.json",
    )
    .await;

    // 场景 4: 工具调用错误恢复（复杂探索，目标 ~20 条）
    run_scenario(
        &silences,
        &worktree,
        "帮我看看项目结构，然后看一下计时器组件",
        "场景 4: 工具调用错误恢复",
        "scenario_4_error.json",
    )
    .await;

    // 场景 5: 无关探索 + 最终定位（目标 ~20 条）
    run_scenario(
        &silences,
        &worktree,
        "看看项目里有几个页面路由，然后检查一下设置页面有没有 bug",
        "场景 5: 无关探索 + 最终定位",
        "scenario_5_routing.json",
    )
    .await;

    eprintln!("\n===== 全部完成 =====");
}
