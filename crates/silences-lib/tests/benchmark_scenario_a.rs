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
    let out_dir = PathBuf::from("target").join("bench-record");
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

    // 1. 创建 Silences 实例，指向 dailyPlanner worktree
    let silences = Silences::new(SilencesConfig {
        db_path: ":memory:".to_string(),
        api_key,
        base_url: None,
        model: Some("deepseek-v4-flash".to_string()),
        system_prompt: None,
        project_root: Some(worktree.clone()),
        tool_limits: None,
        warmup_enabled: false,
    })
    .expect("创建 Silences 实例失败");

    // 2. Scenario A 初始 prompt
    let prompt = "我用番茄钟，切换了一些页面后切回去，发现系统时钟过了 5 分钟，它才进行了 1 分钟。修一下。";

    // 3. 发送消息给 agent，让它修复 bug
    let result = silences.process_turn("bench-scenario-a", prompt).await;

    // 4. 根据结果保存记录
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

            println!(
                "\n✅ process_turn 成功完成！\n\
                 工作区: {:?}\n\
                 记录保存到: {:?}\n\
                 \n\
                 agent 回复末尾:\n{}\n\
                 \n\
                 请手动检查 worktree 中的文件修改是否正确。",
                worktree,
                record_path,
                turn.reply.chars().rev().take(500).collect::<String>().chars().rev().collect::<String>()
            );

            // 这里不做 git diff 断言 —— 由用户亲自检查变动的正确性
        }
        Err(e) => {
            let err_msg = format!("{}", e);

            // 即使失败也保存记录
            let _record_path = save_record(prompt, "", &Vec::<serde_json::Value>::new(), &None::<serde_json::Value>, &Some(err_msg.clone()), &worktree)
                .expect("保存失败记录失败");

            panic!("process_turn 失败: {}", err_msg);
        }
    }
}
