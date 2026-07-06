//! SQLite 数据库 —— 会话、消息、Token 用量持久化

use anyhow::Result;
use rusqlite::Connection;
use silences_core::{Message, ToolCallValue, TokenUsage};

/// 数据库句柄
pub struct Db {
    conn: Connection,
}

impl Db {
    /// 打开（或创建）数据库文件
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// 创建内存数据库（测试用）
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// 建表 + 增量迁移
    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id          TEXT PRIMARY KEY,
                created_at  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  TEXT NOT NULL,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                reasoning_content TEXT,
                created_at  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS token_usage (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id        TEXT NOT NULL,
                round             INTEGER NOT NULL,
                input_tokens      INTEGER NOT NULL DEFAULT 0,
                output_tokens     INTEGER NOT NULL DEFAULT 0,
                cache_hit_tokens  INTEGER NOT NULL DEFAULT 0,
                cache_miss_tokens INTEGER NOT NULL DEFAULT 0,
                cost_yuan         REAL NOT NULL DEFAULT 0.0,
                created_at        TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS context_snapshots (
                session_id  TEXT PRIMARY KEY,
                messages    TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );
            ",
        )?;
        // 所有旧表迁移已全部完成，不再需要 ALTER TABLE 语句。
        // 如果未来增加新列，在此处添加对应 ALTER TABLE 即可。
        Ok(())
    }

    // ── 会话 ──

    /// 列出所有会话（按创建时间降序）
    pub fn list_sessions(&self) -> Result<Vec<silences_core::Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.created_at,
                    (SELECT m.content FROM messages m WHERE m.session_id = s.id AND m.role = 'user' ORDER BY m.id LIMIT 1) AS preview,
                    s.name
             FROM sessions s ORDER BY s.created_at DESC",
        )?;
        let sessions = stmt
            .query_map([], |row| {
                Ok(silences_core::Session {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    preview: row.get(2)?,
                    name: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    pub fn create_session(&self) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute("INSERT INTO sessions (id, created_at) VALUES (?1, ?2)", rusqlite::params![id, now])?;
        Ok(id)
    }

    pub fn rename_session(&self, id: &str, name: &str) -> Result<()> {
        let name = if name.is_empty() { None } else { Some(name) };
        self.conn.execute(
            "UPDATE sessions SET name = ?1 WHERE id = ?2",
            rusqlite::params![name, id],
        )?;
        Ok(())
    }

    /// 删除会话及其所有消息和用量记录
    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM messages WHERE session_id = ?1", rusqlite::params![id])?;
        self.conn.execute("DELETE FROM token_usage WHERE session_id = ?1", rusqlite::params![id])?;
        self.conn.execute("DELETE FROM context_snapshots WHERE session_id = ?1", rusqlite::params![id])?;
        self.conn.execute("DELETE FROM sessions WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    // ── 消息 ──

    /// 保存一条消息
    pub fn save_message(&self, session_id: &str, msg: &Message) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let tool_calls_json = msg.tool_calls.as_ref().map(|tc| serde_json::to_string(tc).unwrap());
        self.conn.execute(
            "INSERT INTO messages (session_id, role, name, content, reasoning_content, tool_calls, tool_call_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![session_id, msg.role, msg.name, msg.content, msg.reasoning_content, tool_calls_json, msg.tool_call_id, now],
        )?;
        Ok(())
    }

    /// 获取会话的所有可见消息（排除 hidden=1 的回滚消息）
    pub fn get_messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content, reasoning_content, tool_calls, tool_call_id, name
             FROM messages WHERE session_id = ?1 AND (hidden IS NULL OR hidden=0)
             ORDER BY id",
        )?;
        let msgs = stmt
            .query_map(rusqlite::params![session_id], |row| {
                let tool_calls_str: Option<String> = row.get(3)?;
                let tool_calls = tool_calls_str
                    .and_then(|s| serde_json::from_str::<Vec<ToolCallValue>>(&s).ok());
                Ok(Message {
                    role: row.get(0)?,
                    content: row.get(1)?,
                    reasoning_content: row.get(2)?,
                    name: row.get(5)?,
                    tool_calls,
                    tool_call_id: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(msgs)
    }

    /// 隐藏此会话中所有 id > after_id 且尚未隐藏的消息
    pub fn hide_messages_after(&self, session_id: &str, after_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET hidden=1 WHERE session_id=?1 AND id>?2 AND (hidden IS NULL OR hidden=0)",
            rusqlite::params![session_id, after_id],
        )?;
        Ok(())
    }

    /// 获取此会话当前的最大消息 ID（用于回滚边界定位）
    pub fn get_max_message_id(&self, session_id: &str) -> Result<Option<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT MAX(id) FROM messages WHERE session_id=?1",
        )?;
        let result = stmt.query_row(rusqlite::params![session_id], |row| row.get(0));
        match result {
            Ok(Some(id)) => Ok(Some(id)),
            Ok(None) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ── 上下文快照 ──

    /// 保存上下文快照（刷新页面后恢复用）
    pub fn save_context_snapshot(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        let json = serde_json::to_string(messages)?;
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR REPLACE INTO context_snapshots (session_id, messages, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![session_id, json, now],
        )?;
        Ok(())
    }

    /// 读取持久化的上下文快照
    pub fn get_context_snapshot(&self, session_id: &str) -> Result<Option<Vec<Message>>> {
        let mut stmt = self.conn.prepare(
            "SELECT messages FROM context_snapshots WHERE session_id = ?1",
        )?;
        let result = stmt.query_row(rusqlite::params![session_id], |row| {
            let json: String = row.get(0)?;
            serde_json::from_str(&json)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        });
        match result {
            Ok(msgs) => Ok(Some(msgs)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// 删除此会话中指定 name 的所有消息（用于刷新 CONTEXT.md / 任务列表）
    pub fn delete_messages_by_name(&self, session_id: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM messages WHERE session_id=?1 AND name=?2",
            rusqlite::params![session_id, name],
        )?;
        Ok(())
    }

    // ── Token 用量 ──

    pub fn save_usage(&self, session_id: &str, round: u32, usage: &TokenUsage) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO token_usage (session_id, round, input_tokens, output_tokens, cache_hit_tokens, cache_miss_tokens, cost_yuan, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                session_id,
                round,
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_hit_tokens,
                usage.cache_miss_tokens,
                usage.cost_yuan,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn get_total_usage(&self, session_id: &str) -> Result<Option<TokenUsage>> {
        let mut stmt = self.conn.prepare(
            "SELECT
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(cache_hit_tokens), 0),
                COALESCE(SUM(cache_miss_tokens), 0),
                COALESCE(SUM(cost_yuan), 0.0)
             FROM token_usage WHERE session_id = ?1",
        )?;
        let result = stmt.query_row(rusqlite::params![session_id], |row| {
            Ok(TokenUsage::new(
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, u32>(3)?,
            ))
        })?;
        if result.input_tokens == 0 && result.output_tokens == 0 {
            Ok(None)
        } else {
            Ok(Some(result))
        }
    }

    // ── 设置 ──

    /// 获取单个设置值
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT value FROM settings WHERE key = ?1",
        )?;
        let result = stmt.query_row(rusqlite::params![key], |row| {
            row.get::<_, String>(0)
        });
        match result {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// 设置一个值
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    /// 删除一个设置
    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM settings WHERE key = ?1", rusqlite::params![key])?;
        Ok(())
    }

    /// 获取所有设置
    pub fn get_all_settings(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (k, v) = row?;
            map.insert(k, v);
        }
        Ok(map)
    }

    /// 获取会话的按轮次明细
    pub fn get_round_usage(&self, session_id: &str) -> Result<Vec<(u32, TokenUsage)>> {
        let mut stmt = self.conn.prepare(
            "SELECT round, input_tokens, output_tokens, cache_hit_tokens, cache_miss_tokens, cost_yuan
             FROM token_usage WHERE session_id = ?1 ORDER BY round",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![session_id], |row| {
                let round: u32 = row.get(0)?;
                let usage = TokenUsage::new(
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                );
                Ok((round, usage))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_session() {
        let db = Db::open_in_memory().unwrap();
        let id = db.create_session().unwrap();
        assert!(!id.is_empty());
    }

    #[test]
    fn test_save_and_get_messages() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        db.save_message(&sid, &Message::new("user", "你好")).unwrap();
        let mut asst_msg = Message::new("assistant", "你好！");
        asst_msg.reasoning_content = Some("thinking...".into());
        db.save_message(&sid, &asst_msg).unwrap();
        let msgs = db.get_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "你好");
        assert_eq!(msgs[1].reasoning_content, Some("thinking...".into()));
    }

    #[test]
    fn test_save_and_get_usage() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        let usage = TokenUsage::new(1000, 200, 800, 200);
        db.save_usage(&sid, 1, &usage).unwrap();
        let total = db.get_total_usage(&sid).unwrap().unwrap();
        assert_eq!(total.input_tokens, 1000);
        assert_eq!(total.cost_yuan, usage.cost_yuan);
    }
}
