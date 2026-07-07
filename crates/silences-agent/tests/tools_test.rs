//! 工具集成测试
//!
//! 每个工具测试：正常路径 + 错误路径 + 逆操作（如适用）。

use std::collections::HashSet;
use std::fs;
use std::sync::Arc;

use anyhow::Result;
use silences_agent::checkpoint_stack::CheckpointStack;
use silences_agent::toolcall::{self, ToolDef, ToolOutcome};
use silences_agent::toolcall::regret::ToolHistory;
use tokio::sync::Mutex;

/// 创建一个测试环境：temp 目录 + 一些文件
fn setup_test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("silences-test-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // 创建测试文件
    fs::write(
        dir.join("hello.rs"),
        "// hello world\nfn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    fs::write(dir.join("greeting.py"), "# a python script\nprint(\"hi\")\n").unwrap();
    fs::write(dir.join("empty.txt"), "").unwrap();
    dir
}

fn tools() -> Vec<ToolDef> {
    let history = Arc::new(Mutex::new(ToolHistory::new(5)));
    toolcall::all_tools(
        history,
        Arc::new(Mutex::new(HashSet::new())),
        Arc::new(CheckpointStack::new()),
        None,
        Default::default(),
    )
}

/// 帮助调用工具
async fn call(name: &str, args: serde_json::Value) -> Result<ToolOutcome> {
    let tools = tools();
    toolcall::execute_tool(&tools, name, args).await
}

// ============================================================
// glance
// ============================================================

#[tokio::test]
async fn test_glance_dir() {
    let dir = setup_test_dir("glance-dir");
    let result = call("glance", serde_json::json!({"path": dir})).await.unwrap();
    assert!(!result.summary.is_empty());
    assert!(result.summary.contains("[DIR]") || result.summary.contains("[FILE]"));
    // 逆操作应为 None（只读工具）
    assert!(result.inverse.is_none());
}

#[tokio::test]
async fn test_glance_file() {
    let dir = setup_test_dir("glance-file");
    let path = dir.join("hello.rs").to_string_lossy().to_string();
    let result = call("glance", serde_json::json!({"path": path})).await.unwrap();
    assert!(result.summary.contains("[FILE]"));
    assert!(result.summary.contains("hello.rs"));
}

#[tokio::test]
async fn test_glance_not_found() {
    let result = call("glance", serde_json::json!({"path": "/nonexistent/path"}))
        .await;
    assert!(result.is_err());
}

// ============================================================
// grep
// ============================================================

#[tokio::test]
async fn test_grep_found() {
    let dir = setup_test_dir("grep-found");
    let path = dir.to_string_lossy().to_string();
    let result = call("grep", serde_json::json!({"path": path, "pattern": "hello", "extensions": ["rs","py"]}))
        .await
        .unwrap();
    assert!(result.summary.contains("hello"));
    assert!(result.inverse.is_none());
}

#[tokio::test]
async fn test_grep_not_found() {
    let dir = setup_test_dir("grep-nf");
    let path = dir.to_string_lossy().to_string();
    let result = call("grep", serde_json::json!({"path": path, "pattern": "ZZZZNOTFOUND", "extensions": ["rs","py"]}))
        .await
        .unwrap();
    assert!(result.summary.contains("无匹配"));
}

// ============================================================
// read
// ============================================================

#[tokio::test]
async fn test_read_file() {
    let dir = setup_test_dir("read-file");
    let path = dir.join("hello.rs").to_string_lossy().to_string();
    let result = call("read", serde_json::json!({"path": path})).await.unwrap();
    assert!(result.summary.contains("hello"));
    assert!(result.inverse.is_none());
}

#[tokio::test]
async fn test_read_empty() {
    let dir = setup_test_dir("read-empty");
    let path = dir.join("empty.txt").to_string_lossy().to_string();
    let result = call("read", serde_json::json!({"path": path})).await.unwrap();
    assert!(result.summary.contains("空文件") || result.summary.contains("empty"));
}

#[tokio::test]
async fn test_read_not_found() {
    let result = call("read", serde_json::json!({"path": "/nonexistent/file.rs"}))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_read_with_range() {
    let dir = setup_test_dir("read-range");
    let path = dir.join("hello.rs").to_string_lossy().to_string();
    let result = call("read", serde_json::json!({"path": path, "start_line": 2, "end_line": 3}))
        .await
        .unwrap();
    assert!(result.summary.contains("main"));
}

// ============================================================
// create / write
// ============================================================

#[tokio::test]
async fn test_create_file() {
    let dir = setup_test_dir("create-file");
    let path = dir.join("new_file.rs").to_string_lossy().to_string();
    let result = call("write", serde_json::json!({"path": path, "content": "fn new() {}"}))
        .await
        .unwrap();
    assert!(result.summary.contains("new_file.rs"));
    assert!(std::path::Path::new(&path).exists());

    // 验证逆操作
    let inverse = result.inverse.unwrap();
    let undo = inverse.apply().unwrap();
    assert!(undo.contains("删除") || undo.contains("deleted") || undo.contains("恢复"));
    assert!(!std::path::Path::new(&path).exists());
}

#[tokio::test]
async fn test_create_existing() {
    let dir = setup_test_dir("create-exist");
    let path = dir.join("hello.rs").to_string_lossy().to_string();
    let result = call("write", serde_json::json!({"path": path, "content": "x"}))
        .await;
    // 写前检查已移除，覆写已有文件应成功
    assert!(result.is_ok());
    let outcome = result.unwrap();
    assert!(outcome.summary.contains("hello.rs"));
    // 验证逆操作可以恢复原文
    let inverse = outcome.inverse.unwrap();
    inverse.apply().unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("hello world"));
}

// ============================================================
// edit
// ============================================================

#[tokio::test]
async fn test_edit_with_line() {
    let dir = setup_test_dir("edit-line");
    let path = dir.join("hello.rs").to_string_lossy().to_string();
    let result = call(
        "edit",
        serde_json::json!({"file": path, "pattern": "hello", "replacement": "world", "line": 1}),
    )
    .await
    .unwrap();
    assert!(result.summary.contains("edited") || result.summary.contains("编辑"));

    // 逆操作应恢复内容
    let inverse = result.inverse.unwrap();
    inverse.apply().unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("hello"));
}

#[tokio::test]
async fn test_edit_no_line_unique_match() {
    let dir = setup_test_dir("edit-unique");
    let path = dir.join("hello.rs").to_string_lossy().to_string();
    // "println" 只在 hello.rs 中出现一次
    let result = call(
        "edit",
        serde_json::json!({"file": path, "pattern": "println", "replacement": "print"}),
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_edit_no_line_multiple_matches() {
    let dir = setup_test_dir("edit-multi");
    // 创建一个有多处匹配的文件
    fs::write(
        dir.join("multi.txt"),
        "apple\nbanana\napple\ncherry\napple\n",
    )
    .unwrap();
    let path = dir.join("multi.txt").to_string_lossy().to_string();
    // 不指定 line，apple 匹配 3 处 → 应报错
    let result = call(
        "edit",
        serde_json::json!({"file": path, "pattern": "apple", "replacement": "orange"}),
    )
    .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("匹配不唯一") || err.contains("not unique"));
}

#[tokio::test]
async fn test_edit_pattern_not_found() {
    let dir = setup_test_dir("edit-nope");
    let path = dir.join("hello.rs").to_string_lossy().to_string();
    let result = call(
        "edit",
        serde_json::json!({"file": path, "pattern": "NONEXISTENT", "replacement": "x", "line": 1}),
    )
    .await;
    assert!(result.is_err());
}

// ============================================================
// replace
// ============================================================

#[tokio::test]
async fn test_replace_in_file() {
    let dir = setup_test_dir("repl-file");
    let path = dir.join("hello.rs").to_string_lossy().to_string();
    let result = call(
        "replace",
        serde_json::json!({"path": path, "pattern": "hello", "replacement": "world", "extensions": ["rs"]}),
    )
    .await
    .unwrap();
    assert!(result.summary.contains("替换完成") || result.summary.contains("批量替换"));
    assert!(result.inverse.is_some());
}

#[tokio::test]
async fn test_replace_no_match() {
    let dir = setup_test_dir("repl-nope");
    let path = dir.to_string_lossy().to_string();
    let result = call(
        "replace",
        serde_json::json!({"path": path, "pattern": "ZZZZZ", "replacement": "x", "extensions": ["rs", "txt"]}),
    )
    .await
    .unwrap();
    assert!(result.summary.contains("无匹配"));
}

#[tokio::test]
async fn test_replace_dry_run() {
    let dir = setup_test_dir("repl-dry");
    let path = dir.join("hello.rs").to_string_lossy().to_string();
    let result = call(
        "replace",
        serde_json::json!({"path": path, "pattern": "hello", "replacement": "world",
          "extensions": ["rs"], "dry_run": true}),
    )
    .await
    .unwrap();
    assert!(result.summary.contains("DRY RUN"), "dry_run 应返回预览: {}", result.summary);
    // 文件内容应未被修改
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("hello"), "dry_run 不应修改文件: {:?}", content);
    assert_eq!(content.matches("hello").count(), 2, "dry_run 不应减少 hello");
    assert!(result.inverse.is_none(), "dry_run 不应有逆操作");
}

// ============================================================
// command
// ============================================================

#[tokio::test]
async fn test_command_echo() {
    let result = call(
        "command",
        serde_json::json!({"command": "echo hello_silences"}),
    )
    .await
    .unwrap();
    assert!(result.summary.contains("hello_silences"));
    assert!(result.inverse.is_none()); // command 不可撤销
}

// ============================================================
// trash
// ============================================================

#[tokio::test]
async fn test_trash_file() {
    let dir = setup_test_dir("trash-file");
    let src = dir.join("hello.rs");
    let src_s = src.to_string_lossy().to_string();
    let result = call("trash", serde_json::json!({"path": src_s}))
        .await
        .unwrap();
    assert!(result.summary.contains("TRASHED") || result.summary.contains("回收站"));
    // 原文件不应存在
    assert!(!src.exists());

    // 验证逆操作恢复文件
    let inverse = result.inverse.unwrap();
    inverse.apply().unwrap();
    assert!(src.exists());
}

// ============================================================
// regret
// ============================================================

#[tokio::test]
async fn test_regret_undo_create() {
    let dir = setup_test_dir("regret-create");
    let history = Arc::new(Mutex::new(ToolHistory::new(5)));
    let tools = toolcall::all_tools(
        history.clone(),
        Arc::new(Mutex::new(HashSet::new())),
        Arc::new(CheckpointStack::new()),
        None,
        Default::default(),
    );

    // 创建一个文件
    let path = dir.join("undo_test.txt").to_string_lossy().to_string();
    let outcome = toolcall::execute_tool(
        &tools,
        "write",
        serde_json::json!({"path": path, "content": "test content"}),
    )
    .await
    .unwrap();
    assert!(std::path::Path::new(&path).exists());

    // 记录逆操作
    {
        let mut h = history.lock().await;
        h.push("write", outcome.inverse.unwrap());
    }

    // 调用 regret（还原 create）
    let regret_outcome = toolcall::execute_tool(&tools, "regret", serde_json::json!({}))
        .await
        .unwrap();
    assert!(regret_outcome.summary.contains("UNDO") || regret_outcome.summary.contains("regret"));
    assert!(!std::path::Path::new(&path).exists());
}

#[tokio::test]
async fn test_regret_empty_history() {
    let history = Arc::new(Mutex::new(ToolHistory::new(5)));
    let tools = toolcall::all_tools(
        history,
        Arc::new(Mutex::new(HashSet::new())),
        Arc::new(CheckpointStack::new()),
        None,
        Default::default(),
    );

    // 空历史下调用 regret → 应失败
    let result = toolcall::execute_tool(&tools, "regret", serde_json::json!({}))
        .await
        .unwrap();
    assert!(
        result.summary.contains("失败") || result.summary.contains("没有")
            || result.summary.contains("fail")
    );
}

// ============================================================
// 清理
// ============================================================

/// 测试后清理临时目录
fn cleanup(name: &str) {
    let dir = std::env::temp_dir().join(format!("silences-test-{name}"));
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_cleanup() {
    for name in [
        "glance-dir", "glance-file", "grep-found", "grep-nf",
        "read-file", "read-empty", "read-range",
        "create-file", "create-exist",
        "edit-line", "edit-unique", "edit-multi", "edit-nope",
        "repl-file", "repl-nope", "repl-dry",
        "trash-file", "regret-create",
    ] {
        cleanup(name);
    }
}
