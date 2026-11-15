//! SQLite WAL implementation of `SessionStore` for `biocompass` local single-binary deployments.

use crate::ids::{OrgId, RunId, SessionId};
use crate::traits::SessionStore;
use crate::types::{
    CompassError, CompassResult, RunRecord, SessionEvent, SessionEventKind, SessionRecord,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;

/// Durable SQLite WAL session and run store.
#[derive(Clone)]
pub struct SqliteSessionStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteSessionStore {
    /// Open or create a WAL-mode SQLite database at `db_path` (or `:memory:` in tests).
    pub fn open(db_path: impl AsRef<Path>) -> CompassResult<Self> {
        let path = db_path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| CompassError::Storage(format!("create db dir: {e}")))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| CompassError::Storage(format!("open sqlite {}: {e}", path.display())))?;
        Self::init_conn(conn)
    }

    /// Open an in-memory SQLite store for deterministic unit tests.
    pub fn in_memory() -> CompassResult<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| CompassError::Storage(format!("open in-memory sqlite: {e}")))?;
        Self::init_conn(conn)
    }

    fn init_conn(conn: Connection) -> CompassResult<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS schema_meta (
                 version INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS sessions (
                 session_id TEXT PRIMARY KEY,
                 org_id TEXT NOT NULL,
                 title TEXT NOT NULL,
                 active_grant_id TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS session_events (
                 event_id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
                 seq INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 content TEXT NOT NULL,
                 metadata_json TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(session_id, seq)
             );
             CREATE TABLE IF NOT EXISTS runs (
                 run_id TEXT PRIMARY KEY,
                 org_id TEXT NOT NULL,
                 session_id TEXT,
                 created_at TEXT NOT NULL,
                 record_json TEXT NOT NULL
             );",
        )
        .map_err(|e| CompassError::Storage(format!("init sqlite schema: {e}")))?;

        let existing: Option<i64> = conn
            .query_row("SELECT version FROM schema_meta LIMIT 1", [], |r| r.get(0))
            .optional()
            .map_err(|e| CompassError::Storage(format!("read schema_meta: {e}")))?;
        if existing.is_none() {
            conn.execute(
                "INSERT INTO schema_meta (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )
            .map_err(|e| CompassError::Storage(format!("write schema_meta: {e}")))?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    async fn with_conn<F, T>(&self, f: F) -> CompassResult<T>
    where
        F: FnOnce(&mut Connection) -> CompassResult<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| CompassError::Internal("sqlite mutex poisoned".into()))?;
            f(&mut guard)
        })
        .await
        .map_err(|e| CompassError::Internal(format!("sqlite blocking task join error: {e}")))?
    }
}

fn parse_ts(raw: &str) -> CompassResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| CompassError::Storage(format!("invalid rfc3339 timestamp '{raw}': {e}")))
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn create_session(
        &self,
        org_id: &OrgId,
        title: &str,
        active_grant_id: &str,
    ) -> CompassResult<SessionRecord> {
        let record = SessionRecord {
            session_id: SessionId::from(format!("ses-{}", Uuid::new_v4().simple())),
            org_id: org_id.clone(),
            title: title.trim().to_owned(),
            active_grant_id: active_grant_id.trim().to_owned(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let cloned = record.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO sessions (session_id, org_id, title, active_grant_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    cloned.session_id.as_str(),
                    cloned.org_id.as_str(),
                    cloned.title,
                    cloned.active_grant_id,
                    cloned.created_at.to_rfc3339(),
                    cloned.updated_at.to_rfc3339(),
                ],
            )
            .map_err(|e| CompassError::Storage(format!("insert session: {e}")))?;
            Ok(())
        })
        .await?;
        Ok(record)
    }

    async fn update_session_grant(
        &self,
        session_id: &SessionId,
        active_grant_id: &str,
    ) -> CompassResult<SessionRecord> {
        let sid = session_id.clone();
        let grant_id = active_grant_id.trim().to_owned();
        let now = Utc::now().to_rfc3339();
        self.with_conn(move |conn| {
            let changed = conn
                .execute(
                    "UPDATE sessions SET active_grant_id = ?1, updated_at = ?2 WHERE session_id = ?3",
                    params![grant_id, now, sid.as_str()],
                )
                .map_err(|e| CompassError::Storage(format!("update session grant: {e}")))?;
            if changed == 0 {
                return Err(CompassError::NotFound(format!("session {sid}")));
            }
            Ok(())
        })
        .await?;
        self.get_session(session_id).await
    }

    async fn get_session(&self, session_id: &SessionId) -> CompassResult<SessionRecord> {
        let sid = session_id.clone();
        self.with_conn(move |conn| {
            let row = conn
                .query_row(
                    "SELECT session_id, org_id, title, active_grant_id, created_at, updated_at
                     FROM sessions WHERE session_id = ?1",
                    params![sid.as_str()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, String>(5)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| CompassError::Storage(format!("query session: {e}")))?
                .ok_or_else(|| CompassError::NotFound(format!("session {sid}")))?;

            Ok(SessionRecord {
                session_id: SessionId::from(row.0),
                org_id: OrgId::from(row.1),
                title: row.2,
                active_grant_id: row.3,
                created_at: parse_ts(&row.4)?,
                updated_at: parse_ts(&row.5)?,
            })
        })
        .await
    }

    async fn list_sessions(&self, org_id: &OrgId) -> CompassResult<Vec<SessionRecord>> {
        let oid = org_id.clone();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT session_id, org_id, title, active_grant_id, created_at, updated_at
                     FROM sessions WHERE org_id = ?1 ORDER BY updated_at DESC",
                )
                .map_err(|e| CompassError::Storage(format!("prepare list_sessions: {e}")))?;
            let rows = stmt
                .query_map(params![oid.as_str()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                })
                .map_err(|e| CompassError::Storage(format!("query list_sessions: {e}")))?;

            let mut out = Vec::new();
            for row in rows {
                let r = row.map_err(|e| CompassError::Storage(format!("read session row: {e}")))?;
                out.push(SessionRecord {
                    session_id: SessionId::from(r.0),
                    org_id: OrgId::from(r.1),
                    title: r.2,
                    active_grant_id: r.3,
                    created_at: parse_ts(&r.4)?,
                    updated_at: parse_ts(&r.5)?,
                });
            }
            Ok(out)
        })
        .await
    }

    async fn append_event(
        &self,
        session_id: &SessionId,
        kind: SessionEventKind,
        content: &str,
        metadata: Value,
    ) -> CompassResult<SessionEvent> {
        let sid = session_id.clone();
        let content_owned = content.to_owned();
        self.with_conn(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| CompassError::Storage(format!("begin event tx: {e}")))?;
            let exists: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM sessions WHERE session_id = ?1",
                    params![sid.as_str()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| CompassError::Storage(format!("check session existence: {e}")))?;
            if exists.is_none() {
                return Err(CompassError::NotFound(format!("session {sid}")));
            }
            let next_seq: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) + 1 FROM session_events WHERE session_id = ?1",
                    params![sid.as_str()],
                    |r| r.get(0),
                )
                .map_err(|e| CompassError::Storage(format!("next event seq: {e}")))?;

            let now = Utc::now();
            let event = SessionEvent {
                event_id: format!("evt-{}", Uuid::new_v4().simple()),
                session_id: sid.clone(),
                seq: next_seq,
                kind,
                content: content_owned,
                metadata,
                created_at: now,
            };
            let kind_json = serde_json::to_string(&event.kind)
                .map_err(|e| CompassError::Storage(format!("serialize event kind: {e}")))?;
            let meta_json = serde_json::to_string(&event.metadata)
                .map_err(|e| CompassError::Storage(format!("serialize event metadata: {e}")))?;

            tx.execute(
                "INSERT INTO session_events (event_id, session_id, seq, kind, content, metadata_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.event_id,
                    event.session_id.as_str(),
                    event.seq,
                    kind_json,
                    event.content,
                    meta_json,
                    event.created_at.to_rfc3339(),
                ],
            )
            .map_err(|e| CompassError::Storage(format!("insert session_event: {e}")))?;

            tx.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE session_id = ?2",
                params![now.to_rfc3339(), sid.as_str()],
            )
            .map_err(|e| CompassError::Storage(format!("touch session updated_at: {e}")))?;

            tx.commit()
                .map_err(|e| CompassError::Storage(format!("commit event tx: {e}")))?;
            Ok(event)
        })
        .await
    }

    async fn list_events(&self, session_id: &SessionId) -> CompassResult<Vec<SessionEvent>> {
        let sid = session_id.clone();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT event_id, session_id, seq, kind, content, metadata_json, created_at
                     FROM session_events WHERE session_id = ?1 ORDER BY seq ASC",
                )
                .map_err(|e| CompassError::Storage(format!("prepare list_events: {e}")))?;
            let rows = stmt
                .query_map(params![sid.as_str()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                })
                .map_err(|e| CompassError::Storage(format!("query list_events: {e}")))?;

            let mut out = Vec::new();
            for row in rows {
                let r = row.map_err(|e| CompassError::Storage(format!("read event row: {e}")))?;
                let kind: SessionEventKind = serde_json::from_str(&r.3)
                    .map_err(|e| CompassError::Storage(format!("decode event kind: {e}")))?;
                let metadata: Value = serde_json::from_str(&r.5)
                    .map_err(|e| CompassError::Storage(format!("decode event metadata: {e}")))?;
                out.push(SessionEvent {
                    event_id: r.0,
                    session_id: SessionId::from(r.1),
                    seq: r.2,
                    kind,
                    content: r.4,
                    metadata,
                    created_at: parse_ts(&r.6)?,
                });
            }
            Ok(out)
        })
        .await
    }

    async fn save_run(&self, run: &RunRecord) -> CompassResult<()> {
        let cloned = run.clone();
        self.with_conn(move |conn| {
            let json = serde_json::to_string(&cloned)
                .map_err(|e| CompassError::Storage(format!("serialize RunRecord: {e}")))?;
            conn.execute(
                "INSERT INTO runs (run_id, org_id, session_id, created_at, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(run_id) DO UPDATE SET record_json = excluded.record_json",
                params![
                    cloned.run_id.as_str(),
                    cloned.org_id.as_str(),
                    cloned.session_id.as_ref().map(SessionId::as_str),
                    cloned.created_at.to_rfc3339(),
                    json,
                ],
            )
            .map_err(|e| CompassError::Storage(format!("upsert run: {e}")))?;
            Ok(())
        })
        .await
    }

    async fn get_run(&self, run_id: &RunId) -> CompassResult<RunRecord> {
        let rid = run_id.clone();
        self.with_conn(move |conn| {
            let json: String = conn
                .query_row(
                    "SELECT record_json FROM runs WHERE run_id = ?1",
                    params![rid.as_str()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| CompassError::Storage(format!("query run: {e}")))?
                .ok_or_else(|| CompassError::NotFound(format!("run {rid}")))?;
            serde_json::from_str(&json)
                .map_err(|e| CompassError::Storage(format!("decode RunRecord: {e}")))
        })
        .await
    }

    async fn list_runs(&self, org_id: &OrgId) -> CompassResult<Vec<RunRecord>> {
        let oid = org_id.clone();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare("SELECT record_json FROM runs WHERE org_id = ?1 ORDER BY created_at DESC")
                .map_err(|e| CompassError::Storage(format!("prepare list_runs: {e}")))?;
            let rows = stmt
                .query_map(params![oid.as_str()], |r| r.get::<_, String>(0))
                .map_err(|e| CompassError::Storage(format!("query list_runs: {e}")))?;
            let mut out = Vec::new();
            for row in rows {
                let json = row.map_err(|e| CompassError::Storage(format!("read run row: {e}")))?;
                let run: RunRecord = serde_json::from_str(&json)
                    .map_err(|e| CompassError::Storage(format!("decode RunRecord: {e}")))?;
                out.push(run);
            }
            Ok(out)
        })
        .await
    }
}
