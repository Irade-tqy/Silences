//! AgentBench Scenario A 集成测试（重写版）
//!
//! 设计原则：
//! 1. 用文件 DB（非 :memory:），跑完后读取全部原始消息（含被 rollback 截断的）
//! 2. 启用 debug_dir，记录每次 API 请求体到 api_debug.json
//! 3. 跑完后检查 worktree 的实际文件改动（git diff），而不是看截断后的消息
//! 4. 不清理 worktree，让你亲自检查成果
//!
//! 运行方式：
//!   cargo test --test benchmark_scenario_a -- --nocapture --ignored
//!
//! 输出：
//!   bench-record/scenario-a-{timestamp}/
//!     db.sqlite         — 完整 SQLite 数据库（含全部消息，包括被截断的）
//!     api_debug.json    — 每次 API 请求的原始请求体
//!     diff.patch        — worktree 的 git diff（实际文件改动）
//!     result.json       — 结构化分析结果

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use silences_lib::{Silences, SilencesConfig};

// ─── Helpers ──────────────────────────────────────────────────────────────

/// 从真实 DB 或环境变量获取 API key
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

/// 从真实 DB 加载系统提示词
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

/// worktree 路径
fn worktree_path() -> PathBuf {
    PathBuf::from(
        std::env::var("BENCH_WORKTREE")
            .unwrap_or_else(|_| "E:/Programs/dailyPlanner-001".to_string()),
    )
}

/// 执行 git diff，返回 diff 文本
fn git_diff(repo: &PathBuf) -> String {
    let out = Command::new("git")
        .args(["diff"])
        .current_dir(repo)
        .output()
        .expect("git diff 失败");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// 执行 git diff --stat，返回摘要
fn git_diff_stat(repo: &PathBuf) -> String {
    let out = Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(repo)
        .output()
        .expect("git diff --stat 失败");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// 执行 git status --short
fn git_status(repo: &PathBuf) -> String {
    let out = Command::new("git")
        .args(["status", "--short"])
        .current_dir(repo)
        .output()
        .expect("git status 失败");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// 从 SQLite 文件读取指定 session 的全部原始消息（包括被 rollback 截断的消息）
fn read_raw_messages(db_path: &str, session_id: &str) -> Vec<serde_json::Value> {
    let conn = match rusqlite::Connection::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("无法打开 DB {db_path}: {e}");
            return vec![];
        }
    };

    let mut stmt = match conn.prepare(
        "SELECT id, role, content, reasoning_content, name, tool_calls, tool_call_id, hidden, created_at
         FROM messages WHERE session_id = ?1 ORDER BY id ASC",
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("prepare 失败: {e}");
            return vec![];
        }
    };

    let rows = match stmt.query_map([session_id], |row| {
        let id: i64 = row.get(0)?;
        let role: String = row.get(1)?;
        let content: String = row.get(2)?;
        let reasoning: Option<String> = row.get(3)?;
        let name: Option<String> = row.get(4)?;
        let tool_calls_str: Option<String> = row.get(5)?;
        let tool_call_id: Option<String> = row.get(6)?;
        let hidden: bool = row.get::<_, i32>(7)? != 0;
        let created_at: String = row.get(8)?;

        let tool_calls = tool_calls_str
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

        Ok(serde_json::json!({
            "id": id,
            "role": role,
            "content_preview": content.chars().take(200).collect::<String>(),
            "content_len": content.len(),
            "reasoning_preview": reasoning.as_ref().map(|r| r.chars().take(100).collect::<String>()),
            "name": name,
            "tool_calls": tool_calls,
            "tool_call_id": tool_call_id,
            "hidden": hidden,
            "created_at": created_at,
        }))
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("query 失败: {e}");
            return vec![];
        }
    };

    rows.filter_map(|r| r.ok()).collect()
}

/// 读取 api_debug.json（如果存在）
fn read_api_debug_json(path: &PathBuf) -> Vec<serde_json::Value> {
    if !path.exists() {
        return vec![];
    }
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    serde_json::from_str(&content).unwrap_or_default()
}

/// 读取 api_pairs.jsonl（配对请求+响应记录）
fn read_api_pairs(path: &PathBuf) -> Vec<serde_json::Value> {
    if !path.exists() {
        return vec![];
    }
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// 分析工具调用序列：从原始 DB 消息中提取
fn analyze_tool_calls(raw_msgs: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    for msg in raw_msgs {
        let role = msg["role"].as_str().unwrap_or("");
        if role != "assistant" {
            continue;
        }
        if let Some(tcs) = msg["tool_calls"].as_array() {
            for tc in tcs {
                let name = tc["function"]["name"].as_str().unwrap_or("?");
                let args_raw = tc["function"]["arguments"].as_str().unwrap_or("{}");
                let args: serde_json::Value =
                    serde_json::from_str(args_raw).unwrap_or(serde_json::json!({}));
                result.push(serde_json::json!({
                    "msg_id": msg["id"],
                    "tool": name,
                    "args": args,
                }));
            }
        }
    }
    result
}

// ─── Test ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn benchmark_scenario_a_debug_bugs() {
    // === 1. 获取凭证 ===
    let api_key = match get_api_key() {
        Some(k) => k,
        None => {
            eprintln!("SKIP: 未配置 API Key");
            return;
        }
    };
    let system_prompt = load_system_prompt();
    let worktree = worktree_path();
    assert!(
        worktree.exists(),
        "worktree 不存在: {:?}。请先创建 dailyPlanner worktree",
        worktree
    );

    eprintln!("=== Setup ===");
    eprintln!("系统提示词: {}", if system_prompt.is_some() { "已加载" } else { "未加载" });
    eprintln!("Worktree: {:?}", worktree);

    // === 2. 创建记录目录 ===
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let record_dir = PathBuf::from("bench-record").join(format!("scenario-a-{ts}"));
    fs::create_dir_all(&record_dir).expect("创建记录目录失败");
    eprintln!("记录目录: {:?}", record_dir);

    // 文件型 SQLite DB（不在 memory，跑完后可查询全部消息）
    let db_path = record_dir.join("db.sqlite");
    // debug_dir：记录每次 API 请求体（raw 请求）
    let debug_dir = record_dir.join("debug");
    fs::create_dir_all(&debug_dir).unwrap();

    // === 3. 捕获跑前状态 ===
    let pre_diff = git_diff(&worktree);
    let pre_status = git_status(&worktree);
    eprintln!("跑前 git diff 长度: {} 字符", pre_diff.len());
    if !pre_diff.is_empty() {
        eprintln!("⚠️  注意：worktree 在跑前已有未提交改动！");
        eprintln!("{}", pre_diff);
    }
    if !pre_status.is_empty() {
        eprintln!("跑前 git status:\n{}", pre_status);
    }

    // === 4. 创建 Silences 实例 ===
    let silences = match Silences::new(SilencesConfig {
        db_path: db_path.to_string_lossy().to_string(),
        api_key,
        base_url: None,
        model: Some("deepseek-v4-flash".to_string()),
        system_prompt,
        project_root: Some(worktree.clone()),
        tool_limits: None,
        warmup_enabled: false,
        debug_dir: Some(debug_dir),
    }) {
        Ok(s) => s,
        Err(e) => panic!("创建 Silences 失败: {e}"),
    };

    // === 5. 切换到 worktree（让 agent 探索时 CWD 正确） ===
    let orig_cwd = std::env::current_dir().ok();
    if let Err(e) = std::env::set_current_dir(&worktree) {
        panic!("无法切换到 worktree: {e}");
    }
    eprintln!("CWD -> worktree");

    // === 6. 运行 agent ===
    let prompt = "我用番茄钟，切换了一些页面后切回去，发现系统时钟过了 5 分钟，它才进行了 1 分钟。修一下。";
    eprintln!("\n=== 开始运行 Scenario A ===");
    eprintln!("Prompt: {prompt}");

    let start = std::time::Instant::now();
    let session_id = silences.create_session().await.expect("创建 session 失败");
    let result = silences.process_turn(&session_id, prompt).await;
    let elapsed = start.elapsed();

    // 恢复 CWD
    if let Some(ref cwd) = orig_cwd {
        let _ = std::env::set_current_dir(cwd);
    }

    match result {
        Ok(turn) => {
            let wall_sec = elapsed.as_secs();
            eprintln!("\n=== 运行完成 ({wall_sec}s) ===");

            // ── 7a. 捕获 worktree git diff ──
            let diff = git_diff(&worktree);
            let diff_stat = git_diff_stat(&worktree);
            let status = git_status(&worktree);

            // 保存 diff 到文件
            fs::write(record_dir.join("diff.patch"), &diff).unwrap();

            // ── 7b. 从文件 DB 读取全部原始消息 ──
            let raw_msgs = read_raw_messages(
                &db_path.to_string_lossy(),
                &session_id,
            );
            fs::write(
                record_dir.join("raw_messages.json"),
                serde_json::to_string_pretty(&raw_msgs).unwrap(),
            )
            .unwrap();

            // ── 7c. 读取 api_debug.json（原始 API 请求体） ──
            let api_debug_path = record_dir.join("debug").join("api_debug.json");
            let api_requests = read_api_debug_json(&api_debug_path);

            // ── 7c2. 读取 api_pairs.jsonl（配对请求+响应） ──
            let api_pairs_path = record_dir.join("debug").join("api_pairs.jsonl");
            let api_pairs = read_api_pairs(&api_pairs_path);
            // 保存为易读 JSON
            fs::write(
                record_dir.join("api_pairs.json"),
                serde_json::to_string_pretty(&api_pairs).unwrap(),
            )
            .unwrap();

            // 从 api_pairs 提取每轮的概要（LLM 的回应 + 工具调用 + 思考链）
            let mut pair_summaries: Vec<serde_json::Value> = Vec::new();
            for (i, pair) in api_pairs.iter().enumerate() {
                let resp = &pair["response"];
                let text_preview = resp["text"].as_str().unwrap_or("")
                    .chars().take(200).collect::<String>();
                let tcs: Vec<&str> = resp["tool_calls"].as_array()
                    .map(|arr| arr.iter().filter_map(|tc| {
                        tc["function"]["name"].as_str()
                    }).collect())
                    .unwrap_or_default();
                let reasoning_preview = pair["captured_deltas"]
                    .as_array()
                    .and_then(|deltas| {
                        let parts: Vec<&str> = deltas.iter()
                            .filter(|d| d["type"] == "reasoning")
                            .filter_map(|d| d["content"].as_str())
                            .collect();
                        if parts.is_empty() { None } else { Some(parts.concat()) }
                    })
                    .map(|r| {
                        let preview: String = r.chars().take(300).collect();
                        // 如果被截断加标记
                        if r.len() > 300 { format!("{}...", preview) } else { preview }
                    });

                pair_summaries.push(serde_json::json!({
                    "round": i + 1,
                    "text_preview": text_preview,
                    "tool_calls": tcs,
                    "reasoning_preview": reasoning_preview,
                }));
            }

            // ── 7d. 分析工具调用序列 ──
            let tool_seq = analyze_tool_calls(&raw_msgs);
            let edit_calls: Vec<_> = tool_seq
                .iter()
                .filter(|t| t["tool"].as_str() == Some("edit"))
                .collect();
            let checkpoint_calls: Vec<_> = tool_seq
                .iter()
                .filter(|t| t["tool"].as_str() == Some("checkpoint"))
                .collect();
            let rollback_calls: Vec<_> = tool_seq
                .iter()
                .filter(|t| t["tool"].as_str() == Some("rollback"))
                .collect();
            let read_calls: usize = tool_seq
                .iter()
                .filter(|t| t["tool"].as_str() == Some("read"))
                .count();
            let glance_calls: usize = tool_seq
                .iter()
                .filter(|t| t["tool"].as_str() == Some("glance"))
                .count();
            let grep_calls: usize = tool_seq
                .iter()
                .filter(|t| t["tool"].as_str() == Some("grep"))
                .count();

            // ── 7e. 做简单的 diff 检查 ──
            let has_bug1_fix = diff.contains("sessionStartRef")
                || diff.contains("elapsedMs")
                || diff.contains("Date.now()");
            let has_bug2_fix = diff.contains("setSessionStart(Date.now())")
                || diff.contains("setSessionStart(");
            let files_changed_count = diff_stat.lines().count();

            // 检查 process_turn 返回的消息中有没有 rollback
            let truncated_msgs = &turn.messages;
            let truncated_has_rollback = truncated_msgs.iter().any(|m| {
                m.tool_calls.as_ref().map_or(false, |tcs| {
                    tcs.iter().any(|tc| tc.function.name == "rollback")
                })
            });

            // ── 7f. 保存结果记录 ──
            let result_record = serde_json::json!({
                "scenario": "A (Debug)",
                "timestamp_sec": ts,
                "wall_time_sec": wall_sec,
                "worktree": worktree.to_string_lossy(),
                "prompt": prompt,

                // process_turn 返回的摘要信息
                "turn_summary": {
                    "reply_preview": turn.reply.chars().take(500).collect::<String>(),
                    "usage": turn.usage,
                    "truncated_messages_count": truncated_msgs.len(),
                    "truncated_has_rollback": truncated_has_rollback,
                },

                // git diff 分析
                "git_diff_stat": diff_stat.trim().to_string(),
                "git_diff_length": diff.len(),
                "diff_saved_to": "diff.patch",

                // 原始消息统计
                "raw_messages_count": raw_msgs.len(),
                "raw_messages_full_path": record_dir.join("raw_messages.json").to_string_lossy().to_string(),

                // API 请求+响应统计
                "api_request_count": api_requests.len(),
                "api_pairs_count": api_pairs.len(),
                "api_debug_path": api_debug_path.to_string_lossy().to_string(),
                "api_pairs_path": api_pairs_path.to_string_lossy().to_string(),
                "pair_summaries": pair_summaries,

                // 工具调用分析（基于原始 DB 消息，包括截断前的）
                "tool_analysis": {
                    "edit_calls_count": edit_calls.len(),
                    "edit_calls": edit_calls.iter().map(|t| t.clone()).collect::<Vec<_>>(),
                    "checkpoint_calls_count": checkpoint_calls.len(),
                    "checkpoint_calls": checkpoint_calls.iter().map(|t| t.clone()).collect::<Vec<_>>(),
                    "rollback_calls_count": rollback_calls.len(),
                    "rollback_calls": rollback_calls.iter().map(|t| t.clone()).collect::<Vec<_>>(),
                    "read_calls_count": read_calls,
                    "glance_calls_count": glance_calls,
                    "grep_calls_count": grep_calls,
                    "total_tool_calls": tool_seq.len(),
                },

                // Bug 修复检查
                "fix_analysis": {
                    "files_changed": files_changed_count,
                    "has_bug1_fix_reference": has_bug1_fix,
                    "has_bug2_fix_reference": has_bug2_fix,
                },
            });

            let result_path = record_dir.join("result.json");
            fs::write(
                &result_path,
                serde_json::to_string_pretty(&result_record).unwrap(),
            )
            .unwrap();

            // ── 7g. 打印报告 ──
            println!("\n{}", "=".repeat(60));
            println!("  Scenario A 运行报告");
            println!("{}", "=".repeat(60));
            println!("  耗时:                {}s", wall_sec);
            println!("  Token 用量:          in={}, out={}, cache_hit={}",
                turn.usage.as_ref().map_or(0, |u| u.input_tokens),
                turn.usage.as_ref().map_or(0, |u| u.output_tokens),
                turn.usage.as_ref().map_or(0, |u| u.cache_hit_tokens));
            println!("  API 请求次数:        {}", api_pairs.len());
            println!("  Raw DB 消息数:       {}", raw_msgs.len());
            println!("  工具调用总数:        {}", tool_seq.len());
            println!("  文件改动数:          {}", files_changed_count);
            println!();
            println!("  每轮 API 调用摘要:");
            for ps in &pair_summaries {
                let round = ps["round"].as_i64().unwrap_or(0);
                let tools: Vec<&str> = ps["tool_calls"].as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                let has_r = if ps["reasoning_preview"].is_string() { "🤔" } else { "" };
                println!("    Round {round}: tools={tools:?} {has_r}");
            }
            println!();
            println!("  工具调用详情:");
            println!("    edit:        {} 次", edit_calls.len());
            println!("    checkpoint:  {} 次", checkpoint_calls.len());
            println!("    rollback:    {} 次", rollback_calls.len());
            println!("    read:        {} 次", read_calls);
            println!("    glance:      {} 次", glance_calls);
            println!("    grep:        {} 次", grep_calls);
            println!();
            println!("  Bug 修复检查:");
            println!("    Bug 1 (sessionStartRef/Date): {}", if has_bug1_fix { "✅" } else { "❌" });
            println!("    Bug 2 (setSessionStart):      {}", if has_bug2_fix { "✅" } else { "❌" });
            println!();
            println!("  process_turn 返回消息含 rollback: {}", if truncated_has_rollback { "✅" } else { "❌" });
            println!();
            println!("  文件改动统计:");
            for line in diff_stat.lines() {
                println!("    {line}");
            }
            println!();
            println!("  Agent 回复末尾:");
            let reply_tail: String = turn
                .reply
                .chars()
                .rev()
                .take(300)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            println!("    {reply_tail}");
            println!("{}", "=".repeat(60));
            println!();
            println!("📁 记录保存到: {:?}", record_dir);
            println!("  - result.json           (分析摘要)");
            println!("  - api_pairs.json        (配对请求+响应，含 reasoning)");
            println!("  - raw_messages.json     (全部原始消息，含被截断的)");
            println!("  - diff.patch            (实际文件改动)");
            println!("  - db.sqlite             (完整 SQLite 数据库)");
            println!("  - debug/api_pairs.jsonl (原始配对日志，每条一行)");
            println!("  - debug/api_debug.json  (请求体快照)");
            println!();

            // 如果不满足条件可以直接 panic
            if !has_bug1_fix && !has_bug2_fix {
                eprintln!("⚠️  未检测到任何 bug 修复。见 {:?}", result_path);
            }
        }
        Err(e) => {
            // 保存错误记录
            let error_record = serde_json::json!({
                "scenario": "A (Debug)",
                "timestamp_sec": ts,
                "error": format!("{:#}", e),
                "worktree": worktree.to_string_lossy(),
                "prompt": prompt,
            });
            let result_path = record_dir.join("result.json");
            let _ = fs::write(&result_path, serde_json::to_string_pretty(&error_record).unwrap());
            panic!("Scenario A 失败: {:#}", e);
        }
    }
}
