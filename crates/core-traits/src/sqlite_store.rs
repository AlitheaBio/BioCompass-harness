//! SQLite WAL implementation of `SessionStore` for `biocompass` local single-binary deployments.

use crate::ids::{OrgId, ProjectId, RunId, SessionId};
use crate::traits::SessionStore;
use crate::types::{
    CompassError, CompassResult, ProjectRecord, RunRecord, SavedViewRecord, SessionEvent,
    SessionEventKind, SessionRecord,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[path = "local_delivery.rs"]
pub mod local_delivery;

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
             CREATE TABLE IF NOT EXISTS projects (
                 project_id TEXT PRIMARY KEY,
                 org_id TEXT NOT NULL,
                 name TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS sessions (
                 session_id TEXT PRIMARY KEY,
                 org_id TEXT NOT NULL,
                 project_id TEXT REFERENCES projects(project_id) ON DELETE SET NULL,
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
             );
             CREATE INDEX IF NOT EXISTS runs_recovery_candidates ON runs(run_id)
                 WHERE json_extract(record_json, '$.status') IN ('prepared', 'running');
             CREATE TABLE IF NOT EXISTS saved_views (
                 view_id TEXT PRIMARY KEY,
                 org_id TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 record_json TEXT NOT NULL
             );",
        )
        .map_err(|e| CompassError::Storage(format!("init sqlite schema: {e}")))?;

        // Backwards-compatible migration for existing databases missing sessions.project_id.
        let has_project_col = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(sessions)")
                .map_err(|e| CompassError::Storage(format!("inspect sessions schema: {e}")))?;
            let cols = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| CompassError::Storage(format!("read sessions schema: {e}")))?;
            let mut found = false;
            for col in cols {
                let name =
                    col.map_err(|e| CompassError::Storage(format!("read column name: {e}")))?;
                if name == "project_id" {
                    found = true;
                    break;
                }
            }
            found
        };
        if !has_project_col {
            conn.execute(
                "ALTER TABLE sessions ADD COLUMN project_id TEXT REFERENCES projects(project_id) ON DELETE SET NULL",
                [],
            )
            .map_err(|e| CompassError::Storage(format!("migrate sessions.project_id: {e}")))?;
        }

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

fn build_session_record(
    session_id: String,
    org_id: String,
    project_id: Option<String>,
    title: String,
    active_grant_id: String,
    created_at: String,
    updated_at: String,
) -> CompassResult<SessionRecord> {
    Ok(SessionRecord {
        session_id: SessionId::from(session_id),
        org_id: OrgId::from(org_id),
        project_id: project_id.map(ProjectId::from),
        title,
        active_grant_id,
        created_at: parse_ts(&created_at)?,
        updated_at: parse_ts(&updated_at)?,
    })
}

fn build_project_record(
    project_id: String,
    org_id: String,
    name: String,
    created_at: String,
    updated_at: String,
) -> CompassResult<ProjectRecord> {
    Ok(ProjectRecord {
        project_id: ProjectId::from(project_id),
        org_id: OrgId::from(org_id),
        name,
        created_at: parse_ts(&created_at)?,
        updated_at: parse_ts(&updated_at)?,
    })
}

// A deleted transcript has no event destination. Other storage errors must roll back the transaction.
fn append_event_in(
    tx: &rusqlite::Transaction<'_>,
    sid: &SessionId,
    org_id: Option<&OrgId>,
    kind: SessionEventKind,
    content: String,
    metadata: Value,
) -> CompassResult<Option<SessionEvent>> {
    let exists: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM sessions WHERE session_id = ?1 AND (?2 IS NULL OR org_id = ?2)",
            params![sid.as_str(), org_id.map(OrgId::as_str)],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| CompassError::Storage(format!("check session existence: {e}")))?;
    if exists.is_none() {
        return Ok(None);
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
        content,
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

    Ok(Some(event))
}

// A finishing writer may transition a run once. Its affected row count also gates the event insert.
fn write_run_row(
    conn: &Connection,
    run: &RunRecord,
    preserve_terminal: bool,
) -> CompassResult<usize> {
    let json = serde_json::to_string(run)
        .map_err(|e| CompassError::Storage(format!("serialize RunRecord: {e}")))?;
    conn.execute(
        "INSERT INTO runs (run_id, org_id, session_id, created_at, record_json)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(run_id) DO UPDATE SET record_json = excluded.record_json
         WHERE NOT ?6 OR json_extract(runs.record_json, '$.status') IN ('prepared','running')",
        params![
            run.run_id.as_str(),
            run.org_id.as_str(),
            run.session_id.as_ref().map(SessionId::as_str),
            run.created_at.to_rfc3339(),
            json,
            preserve_terminal,
        ],
    )
    .map_err(|e| CompassError::Storage(format!("upsert run: {e}")))
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn create_session(
        &self,
        org_id: &OrgId,
        title: &str,
        active_grant_id: &str,
    ) -> CompassResult<SessionRecord> {
        let trimmed_title = title.trim();
        if trimmed_title.is_empty() {
            return Err(CompassError::Validation("session title cannot be empty".into()));
        }
        let record = SessionRecord {
            session_id: SessionId::from(format!("ses-{}", Uuid::new_v4().simple())),
            org_id: org_id.clone(),
            project_id: None,
            title: trimmed_title.to_owned(),
            active_grant_id: active_grant_id.trim().to_owned(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let cloned = record.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO sessions (session_id, org_id, project_id, title, active_grant_id, created_at, updated_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
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
        self.update_session(session_id, None, Some(active_grant_id), None)
            .await
    }

    async fn update_session(
        &self,
        session_id: &SessionId,
        title: Option<&str>,
        active_grant_id: Option<&str>,
        project_id: Option<Option<&ProjectId>>,
    ) -> CompassResult<SessionRecord> {
        let sid = session_id.clone();
        let next_title = match title {
            Some(t) => {
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    return Err(CompassError::Validation(
                        "session title cannot be empty".into(),
                    ));
                }
                Some(trimmed.to_owned())
            }
            None => None,
        };
        let next_grant = active_grant_id.map(|g| g.trim().to_owned());
        let next_project = project_id.map(|opt| opt.map(|p| p.as_str().to_owned()));
        let now = Utc::now().to_rfc3339();

        self.with_conn(move |conn| {
            let current = conn
                .query_row(
                    "SELECT session_id, org_id, project_id, title, active_grant_id, created_at, updated_at
                     FROM sessions WHERE session_id = ?1",
                    params![sid.as_str()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, String>(5)?,
                            r.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| CompassError::Storage(format!("query session before update: {e}")))?
                .ok_or_else(|| CompassError::NotFound(format!("session {sid}")))?;

            let target_project = match next_project {
                Some(Some(ref pid)) => {
                    let exists: Option<String> = conn
                        .query_row(
                            "SELECT project_id FROM projects WHERE project_id = ?1 AND org_id = ?2",
                            params![pid, current.1],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(|e| CompassError::Storage(format!("check project existence: {e}")))?;
                    if exists.is_none() {
                        return Err(CompassError::NotFound(format!("project {pid}")));
                    }
                    Some(pid.clone())
                }
                Some(None) => None,
                None => current.2,
            };
            let target_title = next_title.unwrap_or(current.3);
            let target_grant = next_grant.unwrap_or(current.4);

            conn.execute(
                "UPDATE sessions
                 SET project_id = ?1, title = ?2, active_grant_id = ?3, updated_at = ?4
                 WHERE session_id = ?5",
                params![target_project, target_title, target_grant, now, sid.as_str()],
            )
            .map_err(|e| CompassError::Storage(format!("update session: {e}")))?;
            Ok(())
        })
        .await?;
        self.get_session(session_id).await
    }

    async fn delete_session(&self, session_id: &SessionId) -> CompassResult<()> {
        let sid = session_id.clone();
        self.with_conn(move |conn| {
            let changed = conn
                .execute("DELETE FROM sessions WHERE session_id = ?1", params![sid.as_str()])
                .map_err(|e| CompassError::Storage(format!("delete session: {e}")))?;
            if changed == 0 {
                return Err(CompassError::NotFound(format!("session {sid}")));
            }
            Ok(())
        })
        .await
    }

    async fn get_session(&self, session_id: &SessionId) -> CompassResult<SessionRecord> {
        let sid = session_id.clone();
        self.with_conn(move |conn| {
            let row = conn
                .query_row(
                    "SELECT session_id, org_id, project_id, title, active_grant_id, created_at, updated_at
                     FROM sessions WHERE session_id = ?1",
                    params![sid.as_str()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, String>(5)?,
                            r.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| CompassError::Storage(format!("query session: {e}")))?
                .ok_or_else(|| CompassError::NotFound(format!("session {sid}")))?;

            build_session_record(row.0, row.1, row.2, row.3, row.4, row.5, row.6)
        })
        .await
    }

    async fn list_sessions(&self, org_id: &OrgId) -> CompassResult<Vec<SessionRecord>> {
        let oid = org_id.clone();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT session_id, org_id, project_id, title, active_grant_id, created_at, updated_at
                     FROM sessions WHERE org_id = ?1 ORDER BY updated_at DESC",
                )
                .map_err(|e| CompassError::Storage(format!("prepare list_sessions: {e}")))?;
            let rows = stmt
                .query_map(params![oid.as_str()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                })
                .map_err(|e| CompassError::Storage(format!("query list_sessions: {e}")))?;

            let mut out = Vec::new();
            for row in rows {
                let r = row.map_err(|e| CompassError::Storage(format!("read session row: {e}")))?;
                out.push(build_session_record(r.0, r.1, r.2, r.3, r.4, r.5, r.6)?);
            }
            Ok(out)
        })
        .await
    }

    async fn create_project(&self, org_id: &OrgId, name: &str) -> CompassResult<ProjectRecord> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(CompassError::Validation("project name cannot be empty".into()));
        }
        let record = ProjectRecord {
            project_id: ProjectId::from(format!("proj-{}", Uuid::new_v4().simple())),
            org_id: org_id.clone(),
            name: trimmed.to_owned(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let cloned = record.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO projects (project_id, org_id, name, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    cloned.project_id.as_str(),
                    cloned.org_id.as_str(),
                    cloned.name,
                    cloned.created_at.to_rfc3339(),
                    cloned.updated_at.to_rfc3339(),
                ],
            )
            .map_err(|e| CompassError::Storage(format!("insert project: {e}")))?;
            Ok(())
        })
        .await?;
        Ok(record)
    }

    async fn rename_project(
        &self,
        project_id: &ProjectId,
        name: &str,
    ) -> CompassResult<ProjectRecord> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(CompassError::Validation("project name cannot be empty".into()));
        }
        let pid = project_id.clone();
        let next_name = trimmed.to_owned();
        let now = Utc::now().to_rfc3339();
        self.with_conn(move |conn| {
            let changed = conn
                .execute(
                    "UPDATE projects SET name = ?1, updated_at = ?2 WHERE project_id = ?3",
                    params![next_name, now, pid.as_str()],
                )
                .map_err(|e| CompassError::Storage(format!("rename project: {e}")))?;
            if changed == 0 {
                return Err(CompassError::NotFound(format!("project {pid}")));
            }
            let row = conn
                .query_row(
                    "SELECT project_id, org_id, name, created_at, updated_at FROM projects WHERE project_id = ?1",
                    params![pid.as_str()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                        ))
                    },
                )
                .map_err(|e| CompassError::Storage(format!("query updated project: {e}")))?;
            build_project_record(row.0, row.1, row.2, row.3, row.4)
        })
        .await
    }

    async fn list_projects(&self, org_id: &OrgId) -> CompassResult<Vec<ProjectRecord>> {
        let oid = org_id.clone();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT project_id, org_id, name, created_at, updated_at
                     FROM projects WHERE org_id = ?1 ORDER BY updated_at DESC, name ASC",
                )
                .map_err(|e| CompassError::Storage(format!("prepare list_projects: {e}")))?;
            let rows = stmt
                .query_map(params![oid.as_str()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| CompassError::Storage(format!("query list_projects: {e}")))?;
            let mut out = Vec::new();
            for row in rows {
                let r = row.map_err(|e| CompassError::Storage(format!("read project row: {e}")))?;
                out.push(build_project_record(r.0, r.1, r.2, r.3, r.4)?);
            }
            Ok(out)
        })
        .await
    }

    async fn delete_project(&self, project_id: &ProjectId) -> CompassResult<()> {
        let pid = project_id.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE sessions SET project_id = NULL WHERE project_id = ?1",
                params![pid.as_str()],
            )
            .map_err(|e| CompassError::Storage(format!("ungroup project sessions: {e}")))?;
            let changed = conn
                .execute(
                    "DELETE FROM projects WHERE project_id = ?1",
                    params![pid.as_str()],
                )
                .map_err(|e| CompassError::Storage(format!("delete project: {e}")))?;
            if changed == 0 {
                return Err(CompassError::NotFound(format!("project {pid}")));
            }
            Ok(())
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
            let event = append_event_in(&tx, &sid, None, kind, content_owned, metadata)?
                .ok_or_else(|| CompassError::NotFound(format!("session {sid}")))?;
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
        let run = run.clone();
        self.with_conn(move |conn| write_run_row(conn, &run, false).map(|_| ()))
            .await
    }

    async fn finish_run(
        &self,
        run: &RunRecord,
        summary: &str,
        metadata: Value,
    ) -> CompassResult<()> {
        let run = run.clone();
        let summary = summary.to_owned();
        self.with_conn(move |conn| {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| CompassError::Storage(format!("begin run completion: {e}")))?;
            let changed = write_run_row(&tx, &run, true)?;
            if changed == 1
                && let Some(sid) = &run.session_id
            {
                append_event_in(
                    &tx,
                    sid,
                    Some(&run.org_id),
                    SessionEventKind::RunCompleted,
                    summary,
                    metadata,
                )?;
            }
            tx.commit()
                .map_err(|e| CompassError::Storage(format!("commit run completion: {e}")))
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

impl SqliteSessionStore {
    /// Enumerate nonterminal runs in this workstation across all organizations at startup.
    pub async fn recovery_candidate_run_ids(&self) -> CompassResult<Vec<RunId>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT run_id FROM runs
                     WHERE json_extract(record_json, '$.status') IN ('prepared', 'running')
                     ORDER BY run_id",
                )
                .map_err(|e| {
                    CompassError::Storage(format!("prepare recovery candidate scan: {e}"))
                })?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0).map(RunId::from))
                .map_err(|e| CompassError::Storage(format!("query recovery candidates: {e}")))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| CompassError::Storage(format!("read recovery candidate: {e}")))
        })
        .await
    }

    /// A release can be detached once its durable, unexpired quotes and running attempts have cleared.
    pub async fn module_release_in_use(
        &self,
        id: &crate::PluginId,
        version: &str,
    ) -> CompassResult<bool> {
        let id = id.as_str().to_owned();
        let version = version.to_owned();
        self.with_conn(move |conn| {
            conn.query_row("SELECT EXISTS (SELECT 1 FROM runs
                WHERE json_extract(record_json,'$.plugin_id') = ?1
                  AND json_extract(record_json,'$.plugin_version') = ?2
                  AND (json_extract(record_json,'$.status') = 'running' OR
                    (json_extract(record_json,'$.status') = 'prepared'
                      AND julianday(json_extract(record_json,'$.quote.expires_at')) >= julianday('now'))))",
                params![id,version], |row| row.get(0))
                .map_err(|e| CompassError::Storage(format!("check active module runs: {e}")))
        }).await
    }

    /// Persist or update a local `SavedViewRecord` in SQLite (`saved_views`, §7.1).
    pub async fn save_saved_view(&self, view: &SavedViewRecord) -> CompassResult<()> {
        let cloned = view.clone();
        self.with_conn(move |conn| {
            let json = serde_json::to_string(&cloned)
                .map_err(|e| CompassError::Storage(format!("serialize SavedViewRecord: {e}")))?;
            conn.execute(
                "INSERT INTO saved_views (view_id, org_id, created_at, record_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(view_id) DO UPDATE SET record_json = excluded.record_json",
                params![
                    cloned.view_id,
                    cloned.org_id.as_str(),
                    cloned.created_at.to_rfc3339(),
                    json,
                ],
            )
            .map_err(|e| CompassError::Storage(format!("upsert saved_view: {e}")))?;
            Ok(())
        })
        .await
    }

    /// Retrieve a local `SavedViewRecord` by `view_id` scoped to `org_id`.
    pub async fn get_saved_view(
        &self,
        org_id: &OrgId,
        view_id: &str,
    ) -> CompassResult<SavedViewRecord> {
        let oid = org_id.clone();
        let vid = view_id.trim().to_owned();
        self.with_conn(move |conn| {
            let json: String = conn
                .query_row(
                    "SELECT record_json FROM saved_views WHERE view_id = ?1 AND org_id = ?2",
                    params![vid, oid.as_str()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| CompassError::Storage(format!("query saved_view: {e}")))?
                .ok_or_else(|| CompassError::NotFound(format!("saved view {vid}")))?;
            serde_json::from_str(&json)
                .map_err(|e| CompassError::Storage(format!("decode SavedViewRecord: {e}")))
        })
        .await
    }

    /// List all local `SavedViewRecord`s for `org_id` ordered newest-first.
    pub async fn list_saved_views(&self, org_id: &OrgId) -> CompassResult<Vec<SavedViewRecord>> {
        let oid = org_id.clone();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT record_json FROM saved_views WHERE org_id = ?1 ORDER BY created_at DESC",
                )
                .map_err(|e| CompassError::Storage(format!("prepare list_saved_views: {e}")))?;
            let rows = stmt
                .query_map(params![oid.as_str()], |r| r.get::<_, String>(0))
                .map_err(|e| CompassError::Storage(format!("query list_saved_views: {e}")))?;
            let mut out = Vec::new();
            for row in rows {
                let json =
                    row.map_err(|e| CompassError::Storage(format!("read saved_view row: {e}")))?;
                let rec: SavedViewRecord = serde_json::from_str(&json)
                    .map_err(|e| CompassError::Storage(format!("decode SavedViewRecord: {e}")))?;
                out.push(rec);
            }
            Ok(out)
        })
        .await
    }

    /// Delete a local `SavedViewRecord` and return it so referenced snapshot pins can be released.
    pub async fn delete_saved_view(
        &self,
        org_id: &OrgId,
        view_id: &str,
    ) -> CompassResult<SavedViewRecord> {
        let rec = self.get_saved_view(org_id, view_id).await?;
        let oid = org_id.clone();
        let vid = view_id.trim().to_owned();
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM saved_views WHERE view_id = ?1 AND org_id = ?2",
                params![vid, oid.as_str()],
            )
            .map_err(|e| CompassError::Storage(format!("delete saved_view: {e}")))?;
            Ok(())
        })
        .await?;
        Ok(rec)
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use crate::types::RunStatus;

    #[tokio::test]
    async fn restart_finds_only_prepared_and_running_ids_across_orgs() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sessions.sqlite");
        {
            let store = SqliteSessionStore::open(&path).unwrap();
            let conn = store.conn.lock().unwrap();
            conn.execute("DROP INDEX runs_recovery_candidates", [])
                .unwrap();
            for (id, org, status) in [
                ("a1", "a", RunStatus::Prepared),
                ("b1", "b", RunStatus::Running),
                ("a2", "a", RunStatus::Succeeded),
                ("b2", "b", RunStatus::Failed),
                ("a3", "a", RunStatus::Canceled),
            ] {
                let record = serde_json::json!({ "status": status }).to_string();
                conn.execute(
                    "INSERT INTO runs(run_id, org_id, created_at, record_json) VALUES (?1, ?2, '', ?3)",
                    params![id, org, record],
                )
                .unwrap();
            }
        }
        let store = SqliteSessionStore::open(&path).unwrap();
        assert_eq!(
            store.recovery_candidate_run_ids().await.unwrap(),
            vec![RunId::from("a1"), RunId::from("b1")],
        );
        store.conn.lock().unwrap().execute(
            "UPDATE runs SET record_json = json_set(record_json, '$.status', 'succeeded') WHERE run_id = 'b1'",
            [],
        ).unwrap();
        assert_eq!(
            store.recovery_candidate_run_ids().await.unwrap(),
            vec![RunId::from("a1")]
        );
    }
}

#[cfg(test)]
mod finish_run_tests {
    use super::*;
    use crate::types::RunStatus;
    use serde_json::json;

    fn running(session_id: &SessionId) -> RunRecord {
        serde_json::from_value(json!({
            "run_id":"run-finish", "org_id":"org-fixture", "session_id": session_id,
            "plugin_id":"echo", "plugin_version":"1", "status":"running", "params":{},
            "quote":{"run_id":"run-finish","plugin_id":"echo","plugin_version":"1",
                "image_ref":"echo:1","target_node_id":"worker","estimated_cost_act":1,
                "workload_hash":"test","froglet_quote":{},"expires_at":"2026-01-01T00:00:00Z"},
            "artifacts":[], "froglet_chain":[], "receipt_verified":false, "logs":[],
            "created_at":"2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn transcript_failure_rolls_back_terminal_run_and_retry_commits_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.sqlite");
        let first = SqliteSessionStore::open(&path).unwrap();
        let sid = first
            .create_session(&OrgId::from("org-fixture"), "Recovery", "")
            .await
            .unwrap()
            .session_id;
        let mut run = running(&sid);
        first.save_run(&run).await.unwrap();
        run.status = RunStatus::Succeeded;
        run.result_json = Some(json!({"value":17}));
        let meta = json!({"run_id":run.run_id});
        let injector = Connection::open(&path).unwrap();
        injector
            .execute_batch(
                "CREATE TRIGGER fail_transcript BEFORE INSERT ON session_events
            BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;",
            )
            .unwrap();
        assert!(
            first
                .finish_run(&run, "complete", meta.clone())
                .await
                .is_err()
        );
        assert_eq!(
            first.get_run(&run.run_id).await.unwrap().status,
            RunStatus::Running
        );
        assert!(first.list_events(&sid).await.unwrap().is_empty());
        injector
            .execute_batch("DROP TRIGGER fail_transcript")
            .unwrap();
        drop(injector);
        drop(first);

        // Two independent workspace connections replay the same finalization after restart.
        let left = SqliteSessionStore::open(&path).unwrap();
        let right = SqliteSessionStore::open(&path).unwrap();
        let (a, b) = tokio::join!(
            left.finish_run(&run, "complete", meta.clone()),
            right.finish_run(&run, "complete", meta.clone())
        );
        a.unwrap();
        b.unwrap();
        drop((left, right));
        let reopened = SqliteSessionStore::open(&path).unwrap();
        assert_eq!(
            reopened.get_run(&run.run_id).await.unwrap().result_json,
            run.result_json
        );
        let transcript = reopened.list_events(&sid).await.unwrap();
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript[0].kind, SessionEventKind::RunCompleted);
        assert_eq!(transcript[0].metadata, meta);
        assert_eq!(transcript[0].content, "complete");
    }

    #[tokio::test]
    async fn deleted_transcript_keeps_terminal_workspace_run_and_original_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.sqlite");
        let store = SqliteSessionStore::open(&path).unwrap();
        let sid = store
            .create_session(&OrgId::from("org-fixture"), "Temporary chat", "")
            .await
            .unwrap()
            .session_id;
        let mut run = running(&sid);
        store.save_run(&run).await.unwrap();
        store.delete_session(&sid).await.unwrap();
        run.status = RunStatus::Succeeded;
        run.result_json = Some(json!({"value":23}));
        let metadata = json!({"run_id":run.run_id});
        store
            .finish_run(&run, "complete", metadata.clone())
            .await
            .unwrap();
        drop(store);

        let reopened = SqliteSessionStore::open(&path).unwrap();
        reopened
            .finish_run(&run, "complete", metadata)
            .await
            .unwrap();
        let saved = reopened.get_run(&run.run_id).await.unwrap();
        assert_eq!(saved.status, RunStatus::Succeeded);
        assert_eq!(saved.session_id.as_ref(), Some(&sid));
        assert_eq!(saved.result_json, run.result_json);
        assert!(matches!(
            reopened.get_session(&sid).await,
            Err(CompassError::NotFound(_))
        ));
        assert!(reopened.list_events(&sid).await.unwrap().is_empty());
        assert!(matches!(
            reopened
                .append_event(&sid, SessionEventKind::UserMessage, "hello", Value::Null)
                .await,
            Err(CompassError::NotFound(_))
        ));
    }
}
