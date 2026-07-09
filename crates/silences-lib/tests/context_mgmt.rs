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
     - 删除不必要的消息（如试探性的错误读、无关的文件浏览）\n\
     - 把多条消息合并为一条\n\
     - 把长内容（如 read 返回的完整文件）截断为短摘要\n\n\
     原则：\n\
     - 前几条消息的任何修改（删除/截断/改写）都会破坏 prefix cache，增加后续每次请求的延迟和成本\n\
     - 除非这几条消息完全没有保留价值且压缩收益远大于缓存损失，否则不要动它们\n\
     - 默认越靠前的消息越要保留原样\n\
     - 如果删除一条消息后后续必须重新做一遍，就不该删\n\
     - 长文件内容截断为短摘要，保留文件名和关键行号\n\
     - tool call 和 tool result 必须成对保留或成对删除\n\
     - 消息列表的最后一条是本压缩指令，不要包含在输出中\n\
     - 每条输出消息必须有 role, content, reasoning_content 字段（不存在时设为 \"\"）\n\n\
     输出必须是 JSON 对象，包含 analysis 和 script 字段：\n\
     {\"analysis\": \"<一句话说明>\", \"script\": \"<Python 代码>\"}\n\
     script 示例（选择要保留的索引，Python 会处理配对）：\n\
     import sys,json;msgs=json.load(sys.stdin);msgs=msgs[:-1]\n\
     keep=[msgs[i] for i in [0,1,4,5,6]]\n\
     json.dump(keep,sys.stdout)";

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

/// 修复丢包：如果 tool_result 在压缩结果中缺少对应的 tool_call，
/// 从原始消息中找到对应的 assistant 消息并补回
fn fix_orphan_results(compressed: &mut Vec<Message>, originals: &[Message]) {
    // 收集所有 tool_call_id
    let tc_ids: Vec<&str> = compressed.iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .map(|tc| tc.id.as_str())
        .collect();

    let mut insertions: Vec<(usize, Message)> = Vec::new();
    for (i, m) in compressed.iter().enumerate() {
        if let Some(ref tr_id) = m.tool_call_id {
            if !tc_ids.contains(&tr_id.as_str()) {
                // 在原始消息中找到包含此 tool_call_id 的 assistant 消息
                if let Some(orig) = originals.iter().find(|om| {
                    om.tool_calls.as_ref().map_or(false, |tcs| tcs.iter().any(|tc| tc.id == *tr_id))
                }) {
                    insertions.push((i, orig.clone()));
                }
            }
        }
    }
    // 从后往前插入，保持顺序
    for (pos, msg) in insertions.into_iter().rev() {
        compressed.insert(pos, msg);
    }
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
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!(
            "脚本退出码: {}\n=== STDERR ===\n{}\n=== STDOUT ===\n{}",
            out.status, stderr, stdout
        ));
    }

    let new_msgs: Vec<Message> =
        serde_json::from_str(&stdout).map_err(|e| {
            format!("输出 JSON 解析失败: {e}\n=== 完整 STDOUT ===\n{stdout}")
        })?;
    Ok(new_msgs)
}

/// 基本健全性检查，返回 Err 而非 panic
fn check_invariants(before: &[Message], after: &[Message], label: &str) -> Result<(), String> {
    // 1. 第一条 system 消息必须保留
    let first_sys_before = before.iter().find(|m| m.role == "system");
    let first_sys_after = after.iter().find(|m| m.role == "system");
    if first_sys_before.is_some() != first_sys_after.is_some() {
        return Err(format!("[{label}] 第一条 system 消息丢失"));
    }
    if let (Some(b), Some(a)) = (first_sys_before, first_sys_after) {
        if b.content != a.content {
            return Err(format!("[{label}] system 消息被修改"));
        }
    }

    // 2. 第一条 user 消息必须保留
    let first_user_before = before.iter().find(|m| m.role == "user");
    let first_user_after = after.iter().find(|m| m.role == "user");
    if first_user_before.is_some() != first_user_after.is_some() {
        return Err(format!("[{label}] 第一条 user 消息丢失"));
    }

    // 3. 压缩后消息数不应超过原始
    if after.len() > before.len() {
        return Err(format!(
            "[{label}] 消息数增加了: {} → {}",
            before.len(),
            after.len()
        ));
    }

    // 4. 不能有孤立的 tool_result（tool_call 可以没有对应 result，这是对话末尾未完成的调用）
    // 但 tool_result 必须有一个对应的 tool_call
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
    for tr_id in &all_tr_ids {
        if !all_tc_ids.contains(tr_id) {
            return Err(format!("[{label}] 孤立 tool_result: {tr_id}"));
        }
    }

    println!(
        "  [{label}] ✅ {} → {} 条，所有检查通过",
        before.len(),
        after.len()
    );
    Ok(())
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

    // 输出目录
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let out_dir = PathBuf::from("bench-record").join(format!("ctx-mgmt-{ts}"));
    fs::create_dir_all(&out_dir).unwrap();
    eprintln!("输出目录: {:?}", out_dir);

    let scenarios = [
        ("scenario_1_explore", "5 条，简单探索，几乎不需压缩"),
        ("scenario_2_edit", "7 条，read+edit，不需压缩"),
        ("scenario_3_search", "25 条，重度探索+死胡同+重复 read"),
        ("scenario_4_error", "16 条，项目探索+计时器搜索"),
        ("scenario_5_routing", "24 条，路由探索+设置页面检查"),
        ("scenario_6_useful_content", "9 条，应保留有用内容，删除无关读"),
        ("scenario_7_prefix_suffix", "13 条，前缀错误不改+后缀压缩"),
    ];

    for (name, desc) in &scenarios {
        println!("\n=== {name}: {desc} ===");
        let messages = load_fixture(name);
        println!("  加载 {len} 条消息", len = messages.len());

        // 注入上下文管理提示词作为 user 消息
        let mut ctx_messages = messages.clone();
        ctx_messages.push(Message {
            role: "user".into(),
            content: CTX_MGMT_PROMPT.to_string(),
            name: None,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        });

        // 调用 JSON Output + 执行脚本，失败时重试一次
        let mut compressed = None;
        for attempt in 1..=2 {
            // 大场景用更高 max_tokens 防止截断
            let max_tok: u32 = if name.starts_with("scenario_7") || name.starts_with("scenario_3") { 8192 } else { 4096 };
            let output = match llm
                .chat_json(&ctx_messages, None, None, max_tok, true)
                .await
            {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("  ❌ chat_json 失败 (attempt {attempt}): {e}");
                    continue;
                }
            };

            let _analysis = output["analysis"].as_str().unwrap_or("?");
            let script = output["script"].as_str().unwrap_or("");
            println!("  === RAW OUTPUT (attempt {attempt}) ===");
            println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
            println!("  === END RAW ===");

            // 保存原始响应
            let raw_path = out_dir.join(format!("{name}_raw_attempt{attempt}.json"));
            fs::write(&raw_path, serde_json::to_string_pretty(&output).unwrap()).ok();

            if script.is_empty() {
                eprintln!("  ⚠️ 模型未产出 script（认为不需压缩）");
                break;
            }

            match run_script(script, &ctx_messages) {
                Ok(mut c) => {
                    fix_orphan_results(&mut c, &ctx_messages);
                    compressed = Some(c);
                    break;
                }
                Err(e) => {
                    eprintln!("  ❌ 脚本执行失败 (attempt {attempt}): {e}");
                    if attempt == 2 {
                        eprintln!("  ❌ 重试后仍失败，放弃");
                    }
                }
            }
        }

        if let Some(ref c) = compressed {
            match check_invariants(&messages, c, name) {
                Ok(()) => (),
                Err(e) => eprintln!("  ❌ 不变量检查失败: {e}"),
            }

            // 场景专属语义检查
            match *name {
                "scenario_6_useful_content" => {
                    // 检查：system + user 保留，最终回答保留，进行了压缩
                    let has_sys = c.iter().any(|m| m.role == "system");
                    let has_user = c.iter().any(|m| m.role == "user");
                    let has_answer = c.iter().any(|m|
                        m.content.contains("圆角") || m.content.contains("shadow-md")
                        || m.content.contains("bg-blue-500"));
                    let was_compressed = c.len() < messages.len();

                    if has_sys && has_user && has_answer {
                        println!("  ✅ [场景专用] system+user+回答保留，压缩={}", was_compressed);
                    } else {
                        eprintln!("  ⚠️ [场景专用] sys={} user={} answer={} compressed={}",
                            has_sys, has_user, has_answer, was_compressed);
                    }
                }
                "scenario_7_prefix_suffix" => {
                    // 检查：前缀错误读必须保留
                    let err_text = "app/settings/page.tsx 不存在";
                    let has_prefix_err = c.iter().any(|m| m.content.contains(err_text));
                    if has_prefix_err {
                        println!("  ✅ [场景专用] 前缀错误读保留");
                    } else {
                        eprintln!("  ❌ [场景专用] 前缀错误读丢失！不应被删除");
                    }

                    // 检查：进行了压缩
                    if c.len() < messages.len() {
                        println!("  ✅ [场景专用] 进行了压缩 ({} → {})", messages.len(), c.len());
                    } else {
                        eprintln!("  ⚠️ [场景专用] 未压缩 ({} → {})", messages.len(), c.len());
                    }
                }
                _ => {}
            }

            // 保存压缩结果
            let out_path = out_dir.join(format!("{name}_compressed.json"));
            fs::write(&out_path, serde_json::to_string_pretty(c).unwrap()).ok();
            // 保存输入摘要（角色+内容长度）
            let summary: Vec<serde_json::Value> = messages.iter().map(|m| {
                serde_json::json!({
                    "role": m.role,
                    "content_len": m.content.len(),
                    "content_head": m.content.chars().take(80).collect::<String>(),
                    "has_tool_calls": m.tool_calls.is_some(),
                    "has_tool_call_id": m.tool_call_id.is_some(),
                })
            }).collect();
            fs::write(
                out_dir.join(format!("{name}_input_summary.json")),
                serde_json::to_string_pretty(&summary).unwrap(),
            ).ok();
        }
    }
    eprintln!("\n全部输出: {:?}", out_dir);
}
