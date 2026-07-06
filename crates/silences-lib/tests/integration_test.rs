//! silences-lib 集成测试
//!
//! 使用 :memory: SQLite 数据库，不依赖外部文件系统或 LLM 调用。

use silences_core::Message;
use silences_lib::{Silences, SilencesConfig};

/// 创建一个使用 :memory: 数据库的 Silences 实例（不调用 process_turn 的测试均使用此 helper）
fn make_silences() -> Silences {
    Silences::new(SilencesConfig {
        db_path: ":memory:".to_string(),
        api_key: "sk-test-key".to_string(),
        base_url: None,
        model: None,
        system_prompt: None,
        project_root: None,
        tool_limits: None,
        warmup_enabled: false,
    })
    .unwrap()
}

// ─────────────────────────────────────────────
// 1. Silences::new() with valid config
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_new_valid_config() {
    let _s = make_silences();
    // 创建成功即算通过
}

// ─────────────────────────────────────────────
// 2. Silences::new() with missing DB path
//    (SQLite 不会自动创建父目录，因此不可写的路径应报错)
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_new_missing_db_path() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let bad_path = dir.path().join("nonexistent_subdir").join("test.db");
    let result = Silences::new(SilencesConfig {
        db_path: bad_path.to_str().unwrap().to_string(),
        api_key: "sk-test".to_string(),
        base_url: None,
        model: None,
        system_prompt: None,
        project_root: None,
        tool_limits: None,
        warmup_enabled: false,
    });
    assert!(result.is_err(), "should fail when parent directory doesn't exist");
    Ok(())
}

// ─────────────────────────────────────────────
// 3. create_session() 返回非空 ID
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_create_session_returns_non_empty_id() -> anyhow::Result<()> {
    let s = make_silences();
    let sid = s.create_session().await?;
    assert!(!sid.is_empty(), "session ID should not be empty");
    Ok(())
}

// ─────────────────────────────────────────────
// 4. create_session() 两次调用返回不同 ID
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_create_session_unique_ids() -> anyhow::Result<()> {
    let s = make_silences();
    let id1 = s.create_session().await?;
    let id2 = s.create_session().await?;
    assert_ne!(id1, id2, "two sessions must have different IDs");
    Ok(())
}

// ─────────────────────────────────────────────
// 5. get_context() 在新会话上应返回空列表
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_get_context_new_session_empty() -> anyhow::Result<()> {
    let s = make_silences();
    let sid = s.create_session().await?;
    let ctx = s.get_context(&sid).await?;
    assert!(ctx.is_empty(), "new session should have empty context");
    Ok(())
}

// ─────────────────────────────────────────────
// 6. create_session_from_context() with empty vec
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_create_session_from_context_empty() -> anyhow::Result<()> {
    let s = make_silences();
    let sid = s.create_session_from_context(vec![]).await?;
    assert!(!sid.is_empty(), "should return a non-empty session ID");
    let ctx = s.get_context(&sid).await?;
    assert!(ctx.is_empty(), "session from empty context should also be empty");
    Ok(())
}

// ─────────────────────────────────────────────
// 7. create_session_from_context() 传入多条消息，消息可检索
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_create_session_from_context_with_messages() -> anyhow::Result<()> {
    let s = make_silences();
    let msgs = vec![
        Message::new("user", "Hello"),
        Message::new("assistant", "Hi there"),
    ];
    let sid = s.create_session_from_context(msgs.clone()).await?;
    let ctx = s.get_context(&sid).await?;
    assert_eq!(ctx.len(), 2, "should retrieve 2 messages");
    assert_eq!(ctx[0].role, "user");
    assert_eq!(ctx[0].content, "Hello");
    assert_eq!(ctx[1].role, "assistant");
    assert_eq!(ctx[1].content, "Hi there");
    Ok(())
}

// ─────────────────────────────────────────────
// 8. process_turn() 空 API Key 应报 "未配置 API Key"
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_process_turn_empty_api_key() -> anyhow::Result<()> {
    let s = Silences::new(SilencesConfig {
        db_path: ":memory:".to_string(),
        api_key: "".to_string(),
        base_url: None,
        model: None,
        system_prompt: None,
        project_root: None,
        tool_limits: None,
        warmup_enabled: false,
    })?;
    let result = s.process_turn("test-session", "Hello").await;
    assert!(result.is_err(), "empty API key should cause an error");
    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(
        err_msg.contains("未配置 API Key"),
        "error message should mention missing API key, got: {}",
        err_msg
    );
    Ok(())
}

// ─────────────────────────────────────────────
// 9. delete_session() 删除已有会话
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_delete_session_existing() -> anyhow::Result<()> {
    let s = make_silences();
    let msgs = vec![Message::new("user", "test message")];
    let sid = s.create_session_from_context(msgs).await?;

    // 确认删除前有消息
    let ctx = s.get_context(&sid).await?;
    assert_eq!(ctx.len(), 1, "session should have 1 message before deletion");

    // 删除
    s.delete_session(&sid).await?;

    // 确认删除后无消息
    let ctx = s.get_context(&sid).await?;
    assert!(ctx.is_empty(), "session should have no messages after deletion");
    Ok(())
}

// ─────────────────────────────────────────────
// 10. delete_session() 删除不存在的会话（幂等）
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_delete_session_nonexistent() -> anyhow::Result<()> {
    let s = make_silences();
    let result = s.delete_session("non-existent-id").await;
    assert!(result.is_ok(), "deleting non-existent session should be idempotent (Ok(()))");
    Ok(())
}

// ─────────────────────────────────────────────
// 11. get_context() 在添加消息后正确返回完整消息列表
// ─────────────────────────────────────────────

#[tokio::test]
async fn test_get_context_after_adding_messages() -> anyhow::Result<()> {
    let s = make_silences();
    let msgs = vec![
        Message::new("system", "You are a helpful assistant."),
        Message::new("user", "What is Rust?"),
        Message::new("assistant", "Rust is a systems programming language."),
    ];
    let sid = s.create_session_from_context(msgs.clone()).await?;
    let ctx = s.get_context(&sid).await?;

    assert_eq!(ctx.len(), 3, "should retrieve all 3 messages");
    for (i, expected) in msgs.iter().enumerate() {
        assert_eq!(ctx[i].role, expected.role, "message {} role mismatch", i);
        assert_eq!(ctx[i].content, expected.content, "message {} content mismatch", i);
    }
    Ok(())
}
