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
                created_at  TEXT NOT NULL,
                name        TEXT
            );

            CREATE TABLE IF NOT EXISTS messages (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  TEXT NOT NULL,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                reasoning_content TEXT,
                name        TEXT,
                tool_calls  TEXT,
                tool_call_id TEXT,
                hidden      INTEGER DEFAULT 0,
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

            CREATE TABLE IF NOT EXISTS surgery_waits (
                session_id      TEXT PRIMARY KEY,
                condition       TEXT NOT NULL,
                messages_snapshot TEXT NOT NULL,
                created_at      TEXT NOT NULL
            );

            ",
        )?;
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

    /// 获取会话的所有消息（不过滤 hidden，前端完整展示）
    pub fn get_messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content, reasoning_content, tool_calls, tool_call_id, name
             FROM messages WHERE session_id = ?1
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

    // ── 手术刀 wait 状态 ──

    /// 保存 wait 状态（持久化，server 重启后恢复）
    pub fn save_surgery_wait(&self, session_id: &str, condition: &str, snapshot: &[Message]) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let json = serde_json::to_string(snapshot)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO surgery_waits (session_id, condition, messages_snapshot, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, condition, json, now],
        )?;
        Ok(())
    }

    /// 读取 wait 状态
    pub fn get_surgery_wait(&self, session_id: &str) -> Result<Option<(String, Vec<Message>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT condition, messages_snapshot FROM surgery_waits WHERE session_id = ?1",
        )?;
        let result = stmt.query_row(rusqlite::params![session_id], |row| {
            let condition: String = row.get(0)?;
            let json: String = row.get(1)?;
            let messages: Vec<Message> = serde_json::from_str(&json)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            Ok((condition, messages))
        });
        match result {
            Ok(pair) => Ok(Some(pair)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// 删除 wait 状态（条件达成或取消时）
    pub fn delete_surgery_wait(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM surgery_waits WHERE session_id = ?1",
            rusqlite::params![session_id],
        )?;
        Ok(())
    }

    /// 列出所有 pending 的 wait
    pub fn list_pending_waits(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id FROM surgery_waits",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let ids: Vec<String> = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
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
    use silences_core::ToolCallFunction;

    // ── Session CRUD ──

    #[test]
    fn test_create_session() {
        let db = Db::open_in_memory().unwrap();
        let id = db.create_session().unwrap();
        assert!(!id.is_empty());
    }

    #[test]
    fn test_rename_session() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        db.rename_session(&sid, "我的会话").unwrap();
        let sessions = db.list_sessions().unwrap();
        assert_eq!(sessions[0].name.as_deref(), Some("我的会话"));
    }

    #[test]
    fn test_rename_session_empty_name() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        // First give it a name, then clear it
        db.rename_session(&sid, "temporary").unwrap();
        db.rename_session(&sid, "").unwrap();
        let sessions = db.list_sessions().unwrap();
        assert_eq!(sessions[0].name.as_deref(), None, "empty name should become None");
    }

    #[test]
    fn test_rename_nonexistent_session() {
        let db = Db::open_in_memory().unwrap();
        // Renaming a non-existent session is a no-op (UPDATE affects 0 rows)
        let result = db.rename_session("nonexistent-id", "新名字");
        assert!(result.is_ok());
    }

    #[test]
    fn test_delete_session() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        // Add a message and usage so we can verify they are cleaned up
        db.save_message(&sid, &Message::new("user", "你好")).unwrap();
        db.save_usage(&sid, 1, &TokenUsage::new(100, 50, 10, 20)).unwrap();
        assert_eq!(db.get_messages(&sid).unwrap().len(), 1);
        assert!(db.get_total_usage(&sid).unwrap().is_some());

        db.delete_session(&sid).unwrap();

        // Verify cascade: messages gone, usage gone, session gone
        assert!(db.get_messages(&sid).unwrap().is_empty(), "messages should be empty after session delete");
        assert!(db.get_total_usage(&sid).unwrap().is_none(), "usage should be None after session delete");
        let sessions = db.list_sessions().unwrap();
        assert!(sessions.iter().all(|s| s.id != sid), "deleted session should not appear in list");
    }

    #[test]
    fn test_delete_deleted_session() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        db.delete_session(&sid).unwrap();
        // Deleting an already-deleted session should be idempotent
        let result = db.delete_session(&sid);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_sessions_empty() {
        let db = Db::open_in_memory().unwrap();
        let sessions = db.list_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_list_sessions_one() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        let sessions = db.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, sid);
        assert_eq!(sessions[0].name, None);
        assert_eq!(sessions[0].preview, None);
    }

    #[test]
    fn test_list_sessions_multiple_ordered() {
        let db = Db::open_in_memory().unwrap();
        let sid1 = db.create_session().unwrap();
        let sid2 = db.create_session().unwrap();
        let sessions = db.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        // Most recently created session should be first (ORDER BY created_at DESC)
        assert_eq!(sessions[0].id, sid2);
        assert_eq!(sessions[1].id, sid1);
    }

    // ── Messages ──

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
    fn test_save_message_with_name() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        let msg = Message::new_user("orch", "系统指令");
        db.save_message(&sid, &msg).unwrap();
        let msgs = db.get_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].name.as_deref(), Some("orch"));
    }

    #[test]
    fn test_save_message_with_tool_calls() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        let tool_call = ToolCallValue {
            id: "call_1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "get_weather".into(),
                arguments: r#"{"city":"北京"}"#.into(),
            },
        };
        let msg = Message::new_tool_call(vec![tool_call]);
        db.save_message(&sid, &msg).unwrap();
        let msgs = db.get_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 1);
        let saved_tc = msgs[0].tool_calls.as_ref().unwrap();
        assert_eq!(saved_tc.len(), 1);
        assert_eq!(saved_tc[0].id, "call_1");
        assert_eq!(saved_tc[0].function.name, "get_weather");
    }

    #[test]
    fn test_save_message_with_tool_call_id() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        let msg = Message::new_tool_result("call_1", "北京今天25度");
        db.save_message(&sid, &msg).unwrap();
        let msgs = db.get_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(msgs[0].role, "tool");
        assert_eq!(msgs[0].content, "北京今天25度");
    }

    #[test]
    fn test_hidden_message_still_returned() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        db.save_message(&sid, &Message::new("user", "你好")).unwrap();
        // 即使隐藏，get_messages 也应返回所有消息
        db.hide_messages_after(&sid, 0).unwrap();
        let msgs = db.get_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 1, "get_messages 应返回 hidden 消息");
    }

    #[test]
    fn test_multiple_messages_preserve_order() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        db.save_message(&sid, &Message::new("user", "first")).unwrap();
        db.save_message(&sid, &Message::new("assistant", "second")).unwrap();
        db.save_message(&sid, &Message::new("user", "third")).unwrap();
        db.save_message(&sid, &Message::new("assistant", "fourth")).unwrap();
        let msgs = db.get_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].content, "second");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[2].content, "third");
        assert_eq!(msgs[2].role, "user");
        assert_eq!(msgs[3].content, "fourth");
        assert_eq!(msgs[3].role, "assistant");
    }

    #[test]
    fn test_get_messages_different_roles() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        db.save_message(&sid, &Message::new("system", "You are a helpful assistant")).unwrap();
        db.save_message(&sid, &Message::new("user", "hello")).unwrap();
        db.save_message(&sid, &Message::new("assistant", "hi")).unwrap();
        let msgs = db.get_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[2].role, "assistant");
    }

    #[test]
    fn test_get_messages_nonexistent_session() {
        let db = Db::open_in_memory().unwrap();
        let msgs = db.get_messages("nonexistent").unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_delete_messages_by_name() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        db.save_message(&sid, &Message::new_user("orch", "指令1")).unwrap();
        db.save_message(&sid, &Message::new("user", "普通消息")).unwrap();
        db.save_message(&sid, &Message::new_user("orch", "指令2")).unwrap();
        assert_eq!(db.get_messages(&sid).unwrap().len(), 3);

        db.delete_messages_by_name(&sid, "orch").unwrap();
        let msgs = db.get_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "普通消息");
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn test_delete_messages_by_name_no_match() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        db.save_message(&sid, &Message::new("user", "hello")).unwrap();
        // Deleting with a non-matching name should be a no-op
        db.delete_messages_by_name(&sid, "orch").unwrap();
        let msgs = db.get_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 1, "non-matching delete should be no-op");
        assert_eq!(msgs[0].content, "hello");
    }

    // ── Settings ──

    #[test]
    fn test_set_get_setting() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting("theme", "dark").unwrap();
        let val = db.get_setting("theme").unwrap();
        assert_eq!(val.as_deref(), Some("dark"));
    }

    #[test]
    fn test_get_nonexistent_setting() {
        let db = Db::open_in_memory().unwrap();
        let val = db.get_setting("nonexistent_key").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_update_setting() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting("theme", "dark").unwrap();
        db.set_setting("theme", "light").unwrap();
        let val = db.get_setting("theme").unwrap();
        assert_eq!(val.as_deref(), Some("light"));
    }

    #[test]
    fn test_set_setting_empty_string() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting("key", "").unwrap();
        let val = db.get_setting("key").unwrap();
        assert_eq!(val, Some(String::new()));
    }

    #[test]
    fn test_delete_setting() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting("theme", "dark").unwrap();
        db.delete_setting("theme").unwrap();
        let val = db.get_setting("theme").unwrap();
        assert_eq!(val, None);
    }

    // ── Context Snapshots ──

    #[test]
    fn test_save_get_context_snapshot() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        let messages = vec![
            Message::new("system", "You are helpful"),
            Message::new("user", "你好"),
        ];
        db.save_context_snapshot(&sid, &messages).unwrap();
        let restored = db.get_context_snapshot(&sid).unwrap().unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].content, "You are helpful");
        assert_eq!(restored[1].content, "你好");
    }

    #[test]
    fn test_overwrite_context_snapshot() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        db.save_context_snapshot(&sid, &[Message::new("user", "第一版")]).unwrap();
        db.save_context_snapshot(&sid, &[Message::new("user", "第二版")]).unwrap();
        let restored = db.get_context_snapshot(&sid).unwrap().unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].content, "第二版");
    }

    #[test]
    fn test_get_nonexistent_context_snapshot() {
        let db = Db::open_in_memory().unwrap();
        let result = db.get_context_snapshot("nonexistent-session").unwrap();
        assert!(result.is_none());
    }

    // ── Token Usage ──

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

    #[test]
    fn test_get_total_usage_nonexistent_session() {
        let db = Db::open_in_memory().unwrap();
        let usage = db.get_total_usage("nonexistent").unwrap();
        assert!(usage.is_none());
    }

    #[test]
    fn test_save_usage_multiple_rounds() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_session().unwrap();
        db.save_usage(&sid, 1, &TokenUsage::new(100, 10, 50, 50)).unwrap();
        db.save_usage(&sid, 2, &TokenUsage::new(200, 20, 100, 100)).unwrap();
        db.save_usage(&sid, 3, &TokenUsage::new(300, 30, 150, 150)).unwrap();
        let total = db.get_total_usage(&sid).unwrap().unwrap();
        assert_eq!(total.input_tokens, 600);
        assert_eq!(total.output_tokens, 60);
        assert_eq!(total.cache_hit_tokens, 300);
        assert_eq!(total.cache_miss_tokens, 300);
    }
}
