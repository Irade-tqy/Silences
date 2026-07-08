//! AgentBench Scenario A (Debug) 集成测试
//!
//! 使用真实的 DeepSeek API 和 dailyPlanner worktree，
//! 调用 Silences lib 模式修复 2 个 Pomodoro 计时 bug。
//!
//! 运行方式：
//!   DEEPSEEK_API_KEY=sk-xxx cargo test --test benchmark_scenario_a -- --nocapture
//!
//! 跳过方式（无 API key 时自动跳过）：
//!   cargo test --test benchmark_scenario_a
//!
//! 输出：
//!   target/bench-record/scenario-a-{timestamp}.json  — 完整请求/回复记录

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use silences_lib::{Silences, SilencesConfig};

/// 获取 API key，没有则跳过测试
fn get_api_key() -> Option<String> {
    std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

/// 基准测试的 worktree 路径
fn worktree_path() -> PathBuf {
    PathBuf::from(
        std::env::var("BENCH_WORKTREE")
            .unwrap_or_else(|_| "E:/Programs/dailyPlanner-001".to_string()),
    )
}

/// 数据库路径（包含系统提示词等配置）
fn db_path() -> String {
    std::env::var("SILENCES_DB_PATH")
        .unwrap_or_else(|_| "E:/programs/Silences/silences.db".to_string())
}

/// 保存完整记录到 JSON 文件
fn save_record(
    prompt: &str,
    reply: &str,
    messages: &[impl serde::Serialize],
    usage: &Option<impl serde::Serialize>,
    error: &Option<String>,
    worktree: &PathBuf,
) -> std::io::Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let out_dir = PathBuf::from("bench-record");
    fs::create_dir_all(&out_dir)?;

    let path = out_dir.join(format!("scenario-a-{ts}.json"));

    let record = serde_json::json!({
        "scenario": "A",
        "description": "Debug — 修复 Pomodoro 计时 bug",
        "timestamp_sec": ts,
        "worktree": worktree.to_string_lossy(),
        "prompt": prompt,
        "reply": reply,
        "messages": messages,
        "usage": usage,
        "error": error,
    });

    let json = serde_json::to_string_pretty(&record)?;
    fs::write(&path, json)?;
    Ok(path)
}

#[tokio::test]
async fn benchmark_scenario_a_debug_bugs() {
    let api_key = match get_api_key() {
        Some(k) => k,
        None => {
            eprintln!("SKIP: DEEPSEEK_API_KEY 未设置");
            return;
        }
    };

    let worktree = worktree_path();
    assert!(
        worktree.exists(),
        "worktree 不存在: {:?}。请先运行 benchmark\\setup.bat",
        worktree
    );

    // 加载 DB 中的系统提示词（与 server 模式一致）
    let _system_prompt_present = db_path();
    eprintln!("DB 路径: {}", _system_prompt_present);

    // 切换到 worktree 目录，让 agent 的 glance/grep 探索起点正确
    let orig_cwd = std::env::current_dir().ok();
    if let Err(e) = std::env::set_current_dir(&worktree) {
        panic!("无法切换到 worktree {:?}: {e}", worktree);
    }
    eprintln!("CWD 切换到 worktree: {:?}", worktree);

    // 1. 创建 Silences 实例
    let silences = match Silences::new(SilencesConfig {
        db_path: db_path(),
        api_key,
        base_url: None,
        model: Some("deepseek-v4-flash".to_string()),
        system_prompt: None, // ← 从 DB 自动加载
        project_root: Some(worktree.clone()),
        tool_limits: None,
        warmup_enabled: false,
    }) {
        Ok(s) => s,
        Err(e) => {
            if let Some(cwd) = orig_cwd {
                let _ = std::env::set_current_dir(&cwd);
            }
            panic!("创建 Silences 实例失败: {e}");
        }
    };

    // 2. Scenario A 初始 prompt
    let prompt = "我用番茄钟，切换了一些页面后切回去，发现系统时钟过了 5 分钟，它才进行了 1 分钟。修一下。";

    // 3. 发送消息给 agent
    let result = silences.process_turn("bench-scenario-a", prompt).await;

    // 恢复 CWD
    if let Some(cwd) = orig_cwd {
        let _ = std::env::set_current_dir(&cwd);
    }

    // 4. 处理结果
    match result {
        Ok(turn) => {
            let record_path = save_record(
                prompt,
                &turn.reply,
                &turn.messages,
                &turn.usage,
                &None,
                &worktree,
            )
            .expect("保存记录失败");

            if let Some(ref usage) = turn.usage {
                println!(
                    "Token 用量: 输入={}, 输出={}, 缓存命中={}",
                    usage.input_tokens, usage.output_tokens, usage.cache_hit_tokens
                );
            }

            // 检查是否使用了 rollback
            let all_text: String = turn
                .messages
                .iter()
                .map(|m| format!("{}: {}", m.role, m.content))
                .collect::<Vec<_>>()
                .join("\n");
            let has_rollback = all_text.contains("rollback") || all_text.contains("regret");

            // 提取工具调用序列
            let tool_seq: Vec<String> = turn
                .messages
                .iter()
                .filter_map(|m| {
                    if m.role == "assistant" {
                        m.tool_calls.as_ref().map(|tcs| {
                            tcs.iter()
                                .map(|tc| tc.function.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                    } else {
                        None
                    }
                })
                .collect();

            println!(
                "\n✅ process_turn 成功完成！\n\
                 工作区: {:?}\n\
                 记录保存到: {:?}\n\
                 使用了 rollback/regret: {}\n\
                 工具调用序列:\n  {}\n",
                worktree,
                record_path,
                if has_rollback { "✅" } else { "❌" },
                tool_seq.join("\n  "),
            );

            let reply_tail: String = turn
                .reply
                .chars()
                .rev()
                .take(300)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            println!("agent 回复末尾:\n{}\n", reply_tail);
        }
        Err(e) => {
            let err_msg = format!("{}", e);
            let _record_path = save_record(
                prompt,
                "",
                &Vec::<serde_json::Value>::new(),
                &None::<serde_json::Value>,
                &Some(err_msg.clone()),
                &worktree,
            )
            .expect("保存失败记录失败");
            panic!("process_turn 失败: {}", err_msg);
        }
    }
}
