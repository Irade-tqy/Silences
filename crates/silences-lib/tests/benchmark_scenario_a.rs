//! AgentBench Scenario A — Bug 1 + Bug 2 顺序修复
//!
//! 设计原则：
//! 1. 文件型 SQLite DB，跑完可读全量消息（含 rollback 截断的）
//! 2. debug_dir 捕获每次 API req+res 配对（含 reasoning）
//! 3. 跑完检查 git diff 作为真实改动依据
//! 4. 测试结束后不清理 worktree，留给你检查
//! 5. Bug 1 → Bug 2 顺序发放，观察 agent 是否在无关任务前先 rollback
//!
//! 运行：cargo test --test benchmark_scenario_a -- --nocapture --ignored
//!
//! 输出：bench-record/scenario-a-{ts}/
//!   diff.patch / raw_messages.json / api_pairs.json / result.json / db.sqlite

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use silences_lib::{Silences, SilencesConfig};

// ─── Helpers ──────────────────────────────────────────────────────────────

fn get_api_key() -> Option<String> {
    let db_path = std::env::var("SILENCES_DB_PATH")
        .unwrap_or_else(|_| "E:/programs/Silences/silences.db".to_string());
    let p = PathBuf::from(&db_path);
    if p.exists() {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            if let Ok(mut stmt) =
                conn.prepare("SELECT value FROM settings WHERE key = 'api_key'")
            {
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
    if !p.exists() { return None; }
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

fn git_diff(repo: &PathBuf) -> String {
    let out = Command::new("git").args(["diff", "HEAD"]).current_dir(repo).output()
        .expect("git diff HEAD 失败");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn git_diff_stat(repo: &PathBuf) -> String {
    let out = Command::new("git").args(["diff", "HEAD", "--stat"]).current_dir(repo).output()
        .expect("git diff HEAD --stat 失败");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn reset_worktree(worktree: &PathBuf) {
    let s = Command::new("git").args(["checkout", "--", "."])
        .current_dir(worktree).status().expect("git checkout 失败");
    assert!(s.success(), "重置 worktree 失败");
    let _ = std::fs::remove_dir_all(worktree.join(".silences"));
}

fn read_raw_messages(db_path: &str, session_id: &str) -> Vec<serde_json::Value> {
    let conn = match rusqlite::Connection::open(db_path) {
        Ok(c) => c,
        Err(e) => { eprintln!("打开 DB 失败: {e}"); return vec![]; }
    };
    let mut stmt = match conn.prepare(
        "SELECT id, role, content, name, tool_calls, tool_call_id, hidden, created_at
         FROM messages WHERE session_id = ?1 ORDER BY id ASC"
    ) {
        Ok(s) => s,
        Err(e) => { eprintln!("prepare 失败: {e}"); return vec![]; }
    };
    let rows = match stmt.query_map([session_id], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, i64>(0)?,
            "role": row.get::<_, String>(1)?,
            "content_preview": row.get::<_, String>(2)?.chars().take(200).collect::<String>(),
            "name": row.get::<_, Option<String>>(4)?,
            "tool_calls": row.get::<_, Option<String>>(5)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
            "tool_call_id": row.get::<_, Option<String>>(6)?,
        }))
    }) { Ok(r) => r, Err(_) => return vec![] };
    rows.filter_map(|r| r.ok()).collect()
}

fn read_api_pairs(path: &PathBuf) -> Vec<serde_json::Value> {
    if !path.exists() { return vec![]; }
    let c = match fs::read_to_string(path) { Ok(c) => c, Err(_) => return vec![] };
    c.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn analyze_turn(
    turn: &silences_lib::TurnResult,
    pair_path: &PathBuf,
    worktree: &PathBuf,
    label: &str,
) -> serde_json::Value {
    let api_pairs = read_api_pairs(pair_path);

    // 从 api_pairs 统计工具调用（不依赖 DB，DB 可能不完整）
    let mut edit_n = 0usize; let mut cp_n = 0usize; let mut rb_n = 0usize;
    let mut read_n = 0usize; let mut glance_n = 0usize; let mut regret_n = 0usize;
    let mut find_n = 0usize; let mut grep_n = 0usize; let mut command_n = 0usize;
    let mut block_edit_n = 0usize;
    let mut tool_seq: Vec<String> = Vec::new();

    for pair in &api_pairs {
        let resp = &pair["response"];
        if let Some(tcs) = resp["tool_calls"].as_array() {
            for tc in tcs {
                let name = tc["function"]["name"].as_str().unwrap_or("?").to_string();
                tool_seq.push(name.clone());
                match name.as_str() {
                    "edit" => edit_n += 1,
                    "block_edit" => block_edit_n += 1,
                    "checkpoint" => cp_n += 1,
                    "rollback" => rb_n += 1,
                    "read" => read_n += 1,
                    "glance" => glance_n += 1,
                    "regret" => regret_n += 1,
                    "find" => find_n += 1,
                    "grep" => grep_n += 1,
                    "command" => command_n += 1,
                    _ => {}
                }
            }
        }
    }

    // 每轮摘要
    let mut rounds = Vec::new();
    for (i, pair) in api_pairs.iter().enumerate() {
        let resp = &pair["response"];
        let tcs: Vec<&str> = resp["tool_calls"].as_array()
            .map(|a| a.iter().filter_map(|tc| tc["function"]["name"].as_str()).collect())
            .unwrap_or_default();
        let has_reasoning = pair["captured_deltas"].as_array().map_or(false, |d| {
            d.iter().any(|e| e["type"] == "reasoning")
        });
        rounds.push(serde_json::json!({
            "round": i + 1,
            "tools": tcs,
            "has_reasoning": has_reasoning,
        }));
    }

    let diff = git_diff(worktree);
    let has_bug1 = diff.contains("sessionStartRef") || diff.contains("elapsedMs") || diff.contains("Date.now()");
    let has_bug2 = diff.contains("setSessionStart");

    serde_json::json!({
        "label": label,
        "api_calls": api_pairs.len(),
        "tools": {
            "total": tool_seq.len(),
            "edit": edit_n, "block_edit": block_edit_n,
            "checkpoint": cp_n, "rollback": rb_n,
            "read": read_n, "glance": glance_n, "regret": regret_n,
            "find": find_n, "grep": grep_n, "command": command_n,
            "sequence": tool_seq,
        },
        "rounds": rounds,
        "turn_reply_tail": turn.reply.chars().rev().take(200).collect::<String>().chars().rev().collect::<String>(),
        "usage": turn.usage,
        "truncated_has_rollback": turn.messages.iter().any(|m| {
            m.tool_calls.as_ref().map_or(false, |tcs| tcs.iter().any(|tc| tc.function.name == "rollback"))
        }),
        "diff_stat": git_diff_stat(worktree).trim().to_string(),
        "has_bug1": has_bug1,
        "has_bug2": has_bug2,
    })
}

// ─── Test ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn benchmark_scenario_a_debug_bugs() {
    let api_key = match get_api_key() {
        Some(k) => k,
        None => { eprintln!("SKIP: 未配置 API Key"); return; }
    };
    let system_prompt = load_system_prompt();
    let worktree = worktree_path();
    assert!(worktree.exists(), "worktree 不存在: {:?}", worktree);

    eprintln!("=== Setup ===");
    eprintln!("系统提示词: {}", if system_prompt.is_some() { "已加载" } else { "未加载" });
    eprintln!("Worktree: {:?}", worktree);

    // 重置 worktree
    reset_worktree(&worktree);

    // 记录目录
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let cwd = std::env::current_dir().expect("CWD");
    let record_dir = cwd.join("bench-record").join(format!("scenario-a-{ts}"));
    fs::create_dir_all(&record_dir).unwrap();
    eprintln!("记录目录: {:?}", record_dir);

    let db_path = record_dir.join("db.sqlite");
    let debug_dir = record_dir.join("debug");
    fs::create_dir_all(&debug_dir).unwrap();

    let silences = Silences::new(SilencesConfig {
        db_path: db_path.to_string_lossy().to_string(),
        api_key,
        base_url: None,
        model: Some("deepseek-v4-flash".to_string()),
        system_prompt,
        project_root: Some(worktree.clone()),
        tool_limits: None,
        warmup_enabled: false,
        debug_dir: Some(debug_dir.clone()),
    }).expect("创建 Silences 失败");

    let orig_cwd = std::env::current_dir().ok();
    std::env::set_current_dir(&worktree).unwrap();
    eprintln!("CWD -> worktree");

    // ──── Bug 1 ────
    let prompt1 = "我用番茄钟，切换了一些页面后切回去，发现系统时钟过了 5 分钟，它才进行了 1 分钟。修一下。";
    eprintln!("\n===== Bug 1 =====");
    eprintln!("Prompt: {prompt1}");

    let session_id = silences.create_session().await.expect("创建 session 失败");

    let t0 = std::time::Instant::now();
    eprintln!("[timer] process_turn Bug 1 start @ 0s");
    let r1 = silences.process_turn(&session_id, prompt1).await;
    let wall1 = t0.elapsed();
    eprintln!("[timer] process_turn Bug 1 done @ {:.1}s", wall1.as_secs_f64());

    let pair_path1 = debug_dir.join("api_pairs.jsonl");

    // 保存 Bug 1 diff（不重置 worktree，Bug 2 在 Bug 1 基础上继续）
    let t_analysis = std::time::Instant::now();
    let diff1 = git_diff(&worktree);
    fs::write(record_dir.join("diff_bug1.patch"), &diff1).unwrap();

    let analysis1 = match r1 {
        Ok(ref turn) => analyze_turn(turn, &pair_path1, &worktree, "Bug 1"),
        Err(ref e) => serde_json::json!({"label": "Bug 1", "error": format!("{e:#}")}),
    };
    eprintln!("[timer] analysis Bug 1 done @ {:.1}s", t_analysis.elapsed().as_secs_f64());

    // ──── Bug 2 ────
    let prompt2 = "还有就是我不是让你写一个五分钟休息计时的功能吗？怎么一闪而过了？";
    eprintln!("\n===== Bug 2 =====");
    eprintln!("Prompt: {prompt2}");

    // Bug 2 的 api_pairs 会追加到同一文件，先记下当前行数
    let pre_bug2_pairs = read_api_pairs(&pair_path1).len();

    let t0 = std::time::Instant::now();
    let r2 = silences.process_turn(&session_id, prompt2).await;
    let wall2 = t0.elapsed();
    eprintln!("[timer] process_turn Bug 2 done @ {:.1}s", wall2.as_secs_f64());

    let diff2 = git_diff(&worktree);
    fs::write(record_dir.join("diff_bug2.patch"), &diff2).unwrap();

    let analysis2 = match r2 {
        Ok(ref turn) => {
            // 只取 Bug 2 新增的 pairs
            let all_pairs = read_api_pairs(&pair_path1);
            let bug2_pairs: Vec<_> = all_pairs.into_iter().skip(pre_bug2_pairs).collect();
            let pair_path2 = debug_dir.join("api_pairs_bug2.jsonl");
            // 把 Bug 2 的 pairs 单独存一份
            let _ = fs::write(
                &pair_path2,
                bug2_pairs.iter().map(|p| serde_json::to_string(p).unwrap()).collect::<Vec<_>>().join("\n"),
            );
            analyze_turn(turn, &pair_path2, &worktree, "Bug 2")
        }
        Err(ref e) => serde_json::json!({"label": "Bug 2", "error": format!("{e:#}")}),
    };

    // 恢复 CWD
    if let Some(c) = orig_cwd { let _ = std::env::set_current_dir(c); }

    // ──── 汇总保存 ────
    let api_pairs = read_api_pairs(&debug_dir.join("api_pairs.jsonl"));
    fs::write(
        record_dir.join("api_pairs.json"),
        serde_json::to_string_pretty(&api_pairs).unwrap(),
    ).unwrap();
    let raw_msgs = read_raw_messages(&db_path.to_string_lossy(), &session_id);
    fs::write(
        record_dir.join("raw_messages.json"),
        serde_json::to_string_pretty(&raw_msgs).unwrap(),
    ).unwrap();
    fs::write(record_dir.join("diff_final.patch"), &diff2).unwrap();

    let result = serde_json::json!({
        "scenario": "A (Debug B1+B2)",
        "timestamp_sec": ts,
        "worktree": worktree.to_string_lossy(),
        "prompts": [prompt1, prompt2],
        "analyses": [analysis1, analysis2],
    });
    let result_path = record_dir.join("result.json");
    fs::write(&result_path, serde_json::to_string_pretty(&result).unwrap()).unwrap();

    // ──── 打印报告 ────
    for (label, analysis, wall) in &[
        ("Bug 1", &analysis1, &wall1),
        ("Bug 2", &analysis2, &wall2),
    ] {
        println!("\n{}", "=".repeat(55));
        println!("  {label}");
        println!("{}", "=".repeat(55));
        if analysis["error"].is_string() {
            println!("  ❌ 错误: {}", analysis["error"]);
            continue;
        }
        println!("  耗时:               {:.1}s", wall.as_secs_f64());
        let u = &analysis["usage"];
        println!("  Token:              in={}, out={}, cache_hit={}",
            u["input_tokens"].as_u64().unwrap_or(0),
            u["output_tokens"].as_u64().unwrap_or(0),
            u["cache_hit_tokens"].as_u64().unwrap_or(0));
        println!("  API 调用:           {} 次", analysis["api_calls"]);
        println!("  Raw 消息数:         {}", analysis["raw_messages"]);
        let t = &analysis["tools"];
        println!("  工具:               edit={}, cp={}, rb={}, read={}, glance={}, regret={}",
            t["edit"], t["checkpoint"], t["rollback"], t["read"], t["glance"], t["regret"]);
        println!("  Bug 1 修复标记:     {}", if analysis["has_bug1"].as_bool() == Some(true) { "✅" } else { "❌" });
        println!("  Bug 2 修复标记:     {}", if analysis["has_bug2"].as_bool() == Some(true) { "✅" } else { "❌" });
        println!("  截断消息有 rollback: {}", if analysis["truncated_has_rollback"].as_bool() == Some(true) { "✅" } else { "❌" });
        println!("  diff_stat:          {}", analysis["diff_stat"].as_str().unwrap_or("(空)"));
        println!("  回复末尾:           {}", analysis["turn_reply_tail"].as_str().unwrap_or(""));
    }

    println!("\n📁 记录保存到: {:?}", record_dir);
    println!("  - result.json / api_pairs.json / raw_messages.json / db.sqlite");
    println!("  - diff_bug1.patch / diff_bug2.patch / diff_final.patch");
}
