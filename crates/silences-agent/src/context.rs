//! 项目上下文管理：读取 SILENCES.md（稳定上下文）和 CONTEXT.md（动态进度）
//!
//! 消息结构中的位置：
//!   [A, u_user, o(1), tsks, u_orch(审批), 摘要(缓存), B_stable, B_delta, u_usr(继续)]
//!
//! 每个会话有独立的 SILENCES.md 和 CONTEXT.md，存储在 .silences/sessions/{id}/
//! SILENCES.md = B_stable，只读，存放项目架构/约定。
//! CONTEXT.md  = B_delta，由模型用文件工具更新，整文件就是动态进度。

use std::fs;
use std::path::{Path, PathBuf};

/// 解析后的项目上下文
#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// 项目根目录（server 所在目录）
    pub root: PathBuf,
    /// 会话上下文目录（.silences/sessions/{id}）
    pub session_dir: PathBuf,
    /// SILENCES.md 内容（稳定上下文，B_stable）
    pub silences_md: Option<String>,
    /// CONTEXT.md 完整内容（动态进度，B_delta）
    pub context_delta: Option<String>,
}

/// 读取文件内容
fn read_file(root: &Path, name: &str) -> Option<String> {
    let path = root.join(name);
    if path.exists() {
        std::fs::read_to_string(&path).ok()
    } else {
        None
    }
}

/// 读取会话的 CONTEXT.md，供 agent 循环截断后刷新用
pub fn read_context_md(session_dir: &Path) -> Option<String> {
    read_file(session_dir, "CONTEXT.md")
}

/// 读取会话的 SILENCES.md
pub fn read_silences_md(session_dir: &Path) -> Option<String> {
    read_file(session_dir, "SILENCES.md")
}

/// 初始化会话上下文文件
///
/// - SILENCES.md：从项目根目录复制（如不存在则创建空的）
/// - CONTEXT.md：从模板创建
///
/// 如果文件已存在则不覆盖。
pub fn init_session_context(project_root: &Path, session_id: &str) -> std::io::Result<PathBuf> {
    let session_dir = project_root
        .join(".silences")
        .join("sessions")
        .join(session_id);
    fs::create_dir_all(&session_dir)?;

    // SILENCES.md：从 templates/SILENCES.md 复制，替换占位符为实际路径
    let silences_path = session_dir.join("SILENCES.md");
    if !silences_path.exists() {
        let tpl = project_root.join("templates").join("SILENCES.md");
        if tpl.exists() {
            let content = std::fs::read_to_string(&tpl)?
                .replace("{SilencesDataDirectory}", &session_dir.to_string_lossy());
            std::fs::write(&silences_path, content)?;
        } else {
            fs::write(&silences_path, "")?;
        }
    }

    // CONTEXT.md：从 templates/CONTEXT.md 复制
    let context_path = session_dir.join("CONTEXT.md");
    if !context_path.exists() {
        let tpl = project_root.join("templates").join("CONTEXT.md");
        if tpl.exists() {
            fs::copy(tpl, &context_path)?;
        } else {
            fs::write(&context_path, "")?;
        }
    }

    Ok(session_dir)
}

/// 删除会话上下文文件
pub fn delete_session_context(project_root: &Path, session_id: &str) {
    let session_dir = project_root
        .join(".silences")
        .join("sessions")
        .join(session_id);
    if session_dir.exists() {
        let _ = fs::remove_dir_all(&session_dir);
    }
}

/// 构建会话上下文目录路径
pub fn session_context_dir(project_root: &Path, session_id: &str) -> PathBuf {
    project_root
        .join(".silences")
        .join("sessions")
        .join(session_id)
}

/// 加载项目上下文（从会话目录）
///
/// `project_root` 为 None 时使用当前工作目录。
/// `session_id` 为 None 时使用项目根目录本身。
pub fn load_project_context(project_root: Option<&Path>, session_id: Option<&str>) -> ProjectContext {
    let root = project_root.map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let session_dir = session_id
        .map(|id| root.join(".silences").join("sessions").join(id))
        .unwrap_or_else(|| root.clone());

    ProjectContext {
        silences_md: read_file(&session_dir, "SILENCES.md"),
        context_delta: read_file(&session_dir, "CONTEXT.md"),
        root,
        session_dir,
    }
}

/// 构建 warmup 所需的消息内容（A + u_user + SILENCES.md）
pub fn build_warmup_text(
    history_text: &str,    // A = 之前上下文（纯文本摘要）
    user_message: &str,    // 用户原始输入
    silences_md: &str,     // SILENCES.md 内容
) -> String {
    let mut parts = Vec::new();
    if !history_text.is_empty() {
        parts.push(history_text);
    }
    parts.push(user_message);
    parts.push(silences_md);
    parts.join("\n\n")
}
