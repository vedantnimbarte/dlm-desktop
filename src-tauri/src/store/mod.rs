//! Persistent conversation, message and API-log storage.
//!
//! dlm's server is stateless — it holds no conversation, and every request
//! replays the full message array — so all history is this app's
//! responsibility. SQLite rather than JSON files because search across a few
//! thousand messages is a real feature (FTS5 does it in one query) and because
//! a half-written JSON blob loses a conversation while a torn transaction does
//! not.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub model_id: Option<String>,
    pub created: i64,
    pub updated: i64,
    /// Message count, so the sidebar can show it without loading the bodies.
    #[serde(default)]
    pub message_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: String,
    pub conversation_id: String,
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub ts: i64,
    /// Per-turn measurements (tok/s, TTFT, token counts, finish reason) as JSON.
    pub meta: Option<String>,
}

/// One request/response the app made, for the API log panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiLogEntry {
    #[serde(default)]
    pub id: i64,
    pub ts: i64,
    pub method: String,
    pub path: String,
    pub status: Option<i64>,
    pub latency_ms: Option<i64>,
    pub ttft_ms: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub request_preview: Option<String>,
    pub response_preview: Option<String>,
    pub error: Option<String>,
}

/// A search hit: the message plus the conversation it belongs to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub conversation_id: String,
    pub conversation_title: String,
    pub message_id: String,
    pub role: String,
    /// FTS5-generated excerpt with the match marked by `[` `]`.
    pub snippet: String,
    pub ts: i64,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (creating if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::Other(format!("create {}: {e}", parent.display())))?;
        }
        let conn = Connection::open(path)?;
        Self::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory database, for tests.
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init(conn: &Connection) -> Result<()> {
        // WAL so a long write never blocks the UI's reads. foreign_keys is off
        // by default in SQLite and the cascade below depends on it.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS conversations (
                id       TEXT PRIMARY KEY,
                title    TEXT NOT NULL,
                model_id TEXT,
                created  INTEGER NOT NULL,
                updated  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id              TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL
                                REFERENCES conversations(id) ON DELETE CASCADE,
                seq             INTEGER NOT NULL,
                role            TEXT NOT NULL,
                content         TEXT NOT NULL,
                ts              INTEGER NOT NULL,
                meta_json       TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_messages_conv
                ON messages(conversation_id, seq);

            -- Standalone FTS table rather than an external-content one: the
            -- content here is rewritten as tokens stream in, and keeping
            -- external-content triggers correct through partial updates is more
            -- fragile than simply owning a copy of the text.
            CREATE VIRTUAL TABLE IF NOT EXISTS message_search USING fts5(
                message_id      UNINDEXED,
                conversation_id UNINDEXED,
                content
            );

            CREATE TABLE IF NOT EXISTS api_log (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                ts                INTEGER NOT NULL,
                method            TEXT NOT NULL,
                path              TEXT NOT NULL,
                status            INTEGER,
                latency_ms        INTEGER,
                ttft_ms           INTEGER,
                prompt_tokens     INTEGER,
                completion_tokens INTEGER,
                request_preview   TEXT,
                response_preview  TEXT,
                error             TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_api_log_ts ON api_log(ts DESC);
            "#,
        )?;
        Ok(())
    }

    // --- conversations ------------------------------------------------------

    pub fn create_conversation(&self, id: &str, title: &str, now: i64) -> Result<Conversation> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO conversations (id, title, model_id, created, updated)
             VALUES (?1, ?2, NULL, ?3, ?3)",
            params![id, title, now],
        )?;
        Ok(Conversation {
            id: id.into(),
            title: title.into(),
            model_id: None,
            created: now,
            updated: now,
            message_count: 0,
        })
    }

    pub fn list_conversations(&self) -> Result<Vec<Conversation>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.title, c.model_id, c.created, c.updated,
                    (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id)
             FROM conversations c
             ORDER BY c.updated DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Conversation {
                id: r.get(0)?,
                title: r.get(1)?,
                model_id: r.get(2)?,
                created: r.get(3)?,
                updated: r.get(4)?,
                message_count: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn rename_conversation(&self, id: &str, title: &str, now: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET title = ?2, updated = ?3 WHERE id = ?1",
            params![id, title, now],
        )?;
        Ok(())
    }

    pub fn set_conversation_model(&self, id: &str, model_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET model_id = ?2 WHERE id = ?1",
            params![id, model_id],
        )?;
        Ok(())
    }

    pub fn delete_conversation(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // The message rows cascade; the FTS table is not a real foreign key, so
        // it is cleaned explicitly. Missing this leaks search hits pointing at
        // conversations that no longer exist.
        conn.execute(
            "DELETE FROM message_search WHERE conversation_id = ?1",
            params![id],
        )?;
        conn.execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
        Ok(())
    }

    // --- messages -----------------------------------------------------------

    pub fn append_message(&self, m: &StoredMessage) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO messages (id, conversation_id, seq, role, content, ts, meta_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                m.id,
                m.conversation_id,
                m.seq,
                m.role,
                m.content,
                m.ts,
                m.meta
            ],
        )?;
        conn.execute(
            "UPDATE conversations SET updated = ?2 WHERE id = ?1",
            params![m.conversation_id, m.ts],
        )?;
        if !m.content.is_empty() {
            conn.execute(
                "INSERT INTO message_search (message_id, conversation_id, content)
                 VALUES (?1, ?2, ?3)",
                params![m.id, m.conversation_id, m.content],
            )?;
        }
        Ok(())
    }

    /// Replace a message's body — used once when a streamed turn completes,
    /// rather than on every token.
    pub fn update_message(&self, id: &str, content: &str, meta: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let conversation_id: Option<String> = conn
            .query_row(
                "SELECT conversation_id FROM messages WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(conversation_id) = conversation_id else {
            return Ok(()); // deleted mid-stream; nothing to update
        };
        conn.execute(
            "UPDATE messages SET content = ?2, meta_json = ?3 WHERE id = ?1",
            params![id, content, meta],
        )?;
        conn.execute(
            "DELETE FROM message_search WHERE message_id = ?1",
            params![id],
        )?;
        if !content.is_empty() {
            conn.execute(
                "INSERT INTO message_search (message_id, conversation_id, content)
                 VALUES (?1, ?2, ?3)",
                params![id, conversation_id, content],
            )?;
        }
        Ok(())
    }

    pub fn messages(&self, conversation_id: &str) -> Result<Vec<StoredMessage>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, seq, role, content, ts, meta_json
             FROM messages WHERE conversation_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![conversation_id], |r| {
            Ok(StoredMessage {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                seq: r.get(2)?,
                role: r.get(3)?,
                content: r.get(4)?,
                ts: r.get(5)?,
                meta: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Delete a message and everything after it — the operation behind
    /// "regenerate" and "edit and branch".
    pub fn truncate_from(&self, conversation_id: &str, seq: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM message_search WHERE message_id IN
                 (SELECT id FROM messages WHERE conversation_id = ?1 AND seq >= ?2)",
            params![conversation_id, seq],
        )?;
        conn.execute(
            "DELETE FROM messages WHERE conversation_id = ?1 AND seq >= ?2",
            params![conversation_id, seq],
        )?;
        Ok(())
    }

    pub fn next_seq(&self, conversation_id: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let max: Option<i64> = conn.query_row(
            "SELECT MAX(seq) FROM messages WHERE conversation_id = ?1",
            params![conversation_id],
            |r| r.get(0),
        )?;
        Ok(max.map(|m| m + 1).unwrap_or(0))
    }

    /// Full-text search across every conversation.
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT s.conversation_id, c.title, s.message_id, m.role,
                    snippet(message_search, 2, '[', ']', '…', 12), m.ts
             FROM message_search s
             JOIN messages m      ON m.id = s.message_id
             JOIN conversations c ON c.id = s.conversation_id
             WHERE message_search MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        // Quote the query so user input is treated as a phrase rather than FTS5
        // syntax; a stray `"` or `*` would otherwise be a query error rather
        // than a search for that character.
        let phrase = format!("\"{}\"", trimmed.replace('"', "\"\""));
        let rows = stmt.query_map(params![phrase, limit], |r| {
            Ok(SearchHit {
                conversation_id: r.get(0)?,
                conversation_title: r.get(1)?,
                message_id: r.get(2)?,
                role: r.get(3)?,
                snippet: r.get(4)?,
                ts: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // --- api log ------------------------------------------------------------

    pub fn log_api(&self, e: &ApiLogEntry) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO api_log (ts, method, path, status, latency_ms, ttft_ms,
                                  prompt_tokens, completion_tokens,
                                  request_preview, response_preview, error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                e.ts,
                e.method,
                e.path,
                e.status,
                e.latency_ms,
                e.ttft_ms,
                e.prompt_tokens,
                e.completion_tokens,
                e.request_preview,
                e.response_preview,
                e.error,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn api_log(&self, limit: i64) -> Result<Vec<ApiLogEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, ts, method, path, status, latency_ms, ttft_ms,
                    prompt_tokens, completion_tokens,
                    request_preview, response_preview, error
             FROM api_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(ApiLogEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                method: r.get(2)?,
                path: r.get(3)?,
                status: r.get(4)?,
                latency_ms: r.get(5)?,
                ttft_ms: r.get(6)?,
                prompt_tokens: r.get(7)?,
                completion_tokens: r.get(8)?,
                request_preview: r.get(9)?,
                response_preview: r.get(10)?,
                error: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn clear_api_log(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM api_log", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(conv: &str, id: &str, seq: i64, role: &str, content: &str) -> StoredMessage {
        StoredMessage {
            id: id.into(),
            conversation_id: conv.into(),
            seq,
            role: role.into(),
            content: content.into(),
            ts: 1000 + seq,
            meta: None,
        }
    }

    fn store_with_one_conversation() -> (Store, String) {
        let s = Store::in_memory().unwrap();
        s.create_conversation("c1", "First chat", 1000).unwrap();
        (s, "c1".to_string())
    }

    #[test]
    fn conversations_round_trip() {
        let (s, c) = store_with_one_conversation();
        let list = s.list_conversations().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, c);
        assert_eq!(list[0].message_count, 0);
    }

    #[test]
    fn messages_come_back_in_sequence_order() {
        let (s, c) = store_with_one_conversation();
        // Insert out of order to prove the ORDER BY, not the insert order.
        s.append_message(&msg(&c, "m2", 1, "assistant", "second"))
            .unwrap();
        s.append_message(&msg(&c, "m1", 0, "user", "first"))
            .unwrap();

        let msgs = s.messages(&c).unwrap();
        assert_eq!(
            msgs.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[test]
    fn next_seq_starts_at_zero_and_increments() {
        let (s, c) = store_with_one_conversation();
        assert_eq!(s.next_seq(&c).unwrap(), 0);
        s.append_message(&msg(&c, "m1", 0, "user", "hi")).unwrap();
        assert_eq!(s.next_seq(&c).unwrap(), 1);
    }

    #[test]
    fn appending_bumps_the_conversation_timestamp() {
        let (s, c) = store_with_one_conversation();
        s.append_message(&msg(&c, "m1", 5, "user", "hi")).unwrap();
        let list = s.list_conversations().unwrap();
        assert_eq!(list[0].updated, 1005);
        assert_eq!(list[0].message_count, 1);
    }

    #[test]
    fn deleting_a_conversation_cascades_to_its_messages() {
        let (s, c) = store_with_one_conversation();
        s.append_message(&msg(&c, "m1", 0, "user", "hello world"))
            .unwrap();
        s.delete_conversation(&c).unwrap();

        assert!(s.list_conversations().unwrap().is_empty());
        assert!(s.messages(&c).unwrap().is_empty());
        // The FTS table has no foreign key, so this is the case that silently
        // leaks if the explicit cleanup is ever dropped.
        assert!(s.search("hello", 10).unwrap().is_empty());
    }

    #[test]
    fn search_finds_a_message_and_marks_the_hit() {
        let (s, c) = store_with_one_conversation();
        s.append_message(&msg(&c, "m1", 0, "user", "how does layer streaming work"))
            .unwrap();

        let hits = s.search("streaming", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_title, "First chat");
        assert!(hits[0].snippet.contains("[streaming]"), "{:?}", hits[0]);
    }

    /// User input is a phrase, not FTS5 syntax. Without quoting, a bare `"`
    /// is a query syntax error rather than a search.
    #[test]
    fn search_does_not_choke_on_fts_metacharacters() {
        let (s, c) = store_with_one_conversation();
        s.append_message(&msg(&c, "m1", 0, "user", "quote \" and star *"))
            .unwrap();
        assert!(s.search("\"", 10).is_ok());
        assert!(s.search("*", 10).is_ok());
        assert!(s.search("AND OR NOT", 10).is_ok());
    }

    #[test]
    fn empty_search_returns_nothing_rather_than_everything() {
        let (s, c) = store_with_one_conversation();
        s.append_message(&msg(&c, "m1", 0, "user", "content"))
            .unwrap();
        assert!(s.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn updating_a_message_reindexes_it() {
        let (s, c) = store_with_one_conversation();
        s.append_message(&msg(&c, "m1", 0, "assistant", ""))
            .unwrap();
        s.update_message("m1", "the finished answer", Some(r#"{"tokensPerSec":4.2}"#))
            .unwrap();

        let msgs = s.messages(&c).unwrap();
        assert_eq!(msgs[0].content, "the finished answer");
        assert!(msgs[0].meta.as_deref().unwrap().contains("4.2"));
        assert_eq!(s.search("finished", 10).unwrap().len(), 1);
        // ...and the old (empty) index entry must not linger as a duplicate.
        assert_eq!(s.search("answer", 10).unwrap().len(), 1);
    }

    #[test]
    fn updating_a_deleted_message_is_a_no_op_not_an_error() {
        let (s, _c) = store_with_one_conversation();
        assert!(s.update_message("gone", "text", None).is_ok());
    }

    #[test]
    fn truncate_from_drops_the_tail_for_regenerate() {
        let (s, c) = store_with_one_conversation();
        s.append_message(&msg(&c, "m1", 0, "user", "alpha"))
            .unwrap();
        s.append_message(&msg(&c, "m2", 1, "assistant", "bravo"))
            .unwrap();
        s.append_message(&msg(&c, "m3", 2, "user", "charlie"))
            .unwrap();

        s.truncate_from(&c, 1).unwrap();

        let msgs = s.messages(&c).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "alpha");
        assert!(s.search("bravo", 10).unwrap().is_empty());
    }

    #[test]
    fn api_log_returns_newest_first_and_clears() {
        let s = Store::in_memory().unwrap();
        for i in 0..3 {
            s.log_api(&ApiLogEntry {
                id: 0,
                ts: 100 + i,
                method: "POST".into(),
                path: "/v1/chat/completions".into(),
                status: Some(200),
                latency_ms: Some(10 * i),
                ttft_ms: None,
                prompt_tokens: None,
                completion_tokens: None,
                request_preview: None,
                response_preview: None,
                error: None,
            })
            .unwrap();
        }
        let log = s.api_log(10).unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].ts, 102, "newest first");

        s.clear_api_log().unwrap();
        assert!(s.api_log(10).unwrap().is_empty());
    }
}
