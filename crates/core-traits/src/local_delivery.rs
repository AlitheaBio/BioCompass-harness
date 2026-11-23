//! Durable pickup of authenticated hosted results into an OS-owned local workspace.
use super::SqliteSessionStore;
use crate::{CompassError, CompassResult, OrgId, RunId, RunQuote, RunRecord, RunStatus};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

/// The caller supplies the authoritative hub origin and authenticated principal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeliveryScope {
    pub origin: String,
    pub org_id: OrgId,
    pub subject: String,
}

/// Locally remembered authority for reading an already-delivered result.
#[derive(Clone)]
pub struct SavedReadSession {
    pub scope: DeliveryScope,
    pub fingerprint: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingDelivery {
    pub quote: RunQuote,
    pub saved: bool,
    pub acknowledged: bool,
    pub retired: bool,
}

/// An immutable result reference, including the original signed run and source snapshot metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalDelivery {
    pub workspace_id: String,
    pub origin: String,
    pub org_id: OrgId,
    pub run: RunRecord,
    pub receipt_hash: String,
    pub result_sha256: String,
    pub provenance: Value,
}

pub struct DeliveryFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct LocalDeliveryStore {
    db: SqliteSessionStore,
    read_only: bool,
    objects: PathBuf,
    workspace_id: String,
}

fn storage(error: impl std::fmt::Display) -> CompassError {
    CompassError::Storage(format!("local workspace delivery: {error}"))
}

fn missing() -> CompassError {
    CompassError::NotFound("workspace delivery".into())
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn integrity(message: &str) -> CompassError {
    CompassError::IntegrityViolation(message.into())
}

fn valid_name(name: &str) -> bool {
    !name.contains(['\\', '\0'])
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn decode<T: serde::de::DeserializeOwned>(encoded: &str) -> CompassResult<T> {
    serde_json::from_str(encoded).map_err(storage)
}

#[cfg(unix)]
fn protect_database(db_path: &Path) -> CompassResult<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(storage)?;
    }
    // SQLite inherits the database's permissions when creating new sidecars.
    let db = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(db_path)
        .map_err(storage)?;
    db.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(storage)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = db_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        match std::fs::set_permissions(Path::new(&sidecar), std::fs::Permissions::from_mode(0o600))
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
    }
    Ok(())
}

impl LocalDeliveryStore {
    pub fn open_workspace(data_dir: impl AsRef<Path>) -> CompassResult<Self> {
        let root = data_dir.as_ref();
        Self::open(root.join("sessions.sqlite"), root.join("store"))
    }

    /// Open a previously initialized local workspace read-only for the owning OS user.
    pub fn open_workspace_existing(data_dir: impl AsRef<Path>) -> CompassResult<Self> {
        let root = data_dir.as_ref();
        let path = root.join("sessions.sqlite");
        let objects = root.join("store/workspace-result-objects");
        if !path.is_file() || !objects.is_dir() {
            return Err(missing());
        }
        let conn =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(storage)?;
        let workspace_id = conn
            .query_row(
                "SELECT workspace_id FROM local_workspace_identity WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(missing)?;
        Ok(Self {
            db: SqliteSessionStore {
                conn: std::sync::Arc::new(std::sync::Mutex::new(conn)),
            },
            objects,
            workspace_id,
            read_only: true,
        })
    }

    pub fn open(db_path: impl AsRef<Path>, blob_root: impl AsRef<Path>) -> CompassResult<Self> {
        let db_path = db_path.as_ref();
        if db_path == Path::new(":memory:") {
            return Err(CompassError::Validation(
                "workspace delivery requires a durable database path".into(),
            ));
        }
        #[cfg(unix)]
        protect_database(db_path)?;
        let db = SqliteSessionStore::open(db_path)?;
        let workspace_id = {
            let conn = db.conn.lock().map_err(storage)?;
            conn.execute_batch(
                "PRAGMA synchronous=FULL;
                 CREATE TABLE IF NOT EXISTS local_workspace_identity (
                     singleton INTEGER PRIMARY KEY CHECK(singleton=1), workspace_id TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS local_workspace_read_sessions (
                     handle_hash TEXT PRIMARY KEY, fingerprint TEXT NOT NULL,
                     origin TEXT NOT NULL, org_id TEXT NOT NULL, subject TEXT NOT NULL,
                     expires_ms INTEGER NOT NULL, revoked INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE IF NOT EXISTS local_workspace_deliveries (
                     origin TEXT NOT NULL, org_id TEXT NOT NULL, run_id TEXT NOT NULL,
                     subject TEXT NOT NULL, quote_json TEXT NOT NULL, saved_json TEXT,
                     acknowledged INTEGER NOT NULL DEFAULT 0, retired INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY(origin, org_id, run_id)
                 );",
            )
            .map_err(storage)?;
            conn.execute(
                "INSERT OR IGNORE INTO local_workspace_identity VALUES (1,?1)",
                [Uuid::new_v4().to_string()],
            )
            .map_err(storage)?;
            conn.query_row(
                "SELECT workspace_id FROM local_workspace_identity WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(storage)?
        };
        // Only digests are used as filesystem names. Remote paths and blob keys never select local files.
        let root = blob_root.as_ref();
        let objects = root.join("workspace-result-objects");
        let mut dirs = std::fs::DirBuilder::new();
        dirs.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            dirs.mode(0o700);
        }
        dirs.create(&objects).map_err(storage)?;
        #[cfg(unix)]
        {
            std::fs::File::open(root)
                .and_then(|f| f.sync_all())
                .map_err(storage)?;
            if let Some(parent) = root.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::File::open(parent)
                    .and_then(|f| f.sync_all())
                    .map_err(storage)?;
            }
        }
        Ok(Self {
            db,
            read_only: false,
            objects,
            workspace_id,
        })
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Save the trusted original quote before confirming the hosted run.
    pub async fn register_pending(
        &self,
        scope: &DeliveryScope,
        quote: &RunQuote,
    ) -> CompassResult<()> {
        if scope.origin.trim().is_empty() || scope.subject.trim().is_empty() {
            return Err(CompassError::Validation(
                "missing delivery origin or subject".into(),
            ));
        }
        let scope = scope.clone();
        let quote = quote.clone();
        self.db.with_conn(move |conn| {
            let quoted = serde_json::to_string(&quote).map_err(storage)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(storage)?;
            tx.execute(
                "INSERT INTO local_workspace_deliveries(origin, org_id, run_id, subject, quote_json)
                 VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(origin, org_id, run_id) DO NOTHING",
                params![scope.origin,scope.org_id.as_str(),quote.run_id.as_str(),scope.subject,quoted]
            ).map_err(storage)?;
            let existing: String = tx.query_row(
                "SELECT quote_json FROM local_workspace_deliveries WHERE origin=?1 AND org_id=?2 AND run_id=?3 AND subject=?4",
                params![scope.origin,scope.org_id.as_str(),quote.run_id.as_str(),scope.subject], |row| row.get(0)
            ).optional().map_err(storage)?.ok_or_else(missing)?;
            if decode::<Value>(&existing)? != serde_json::to_value(&quote).map_err(storage)? {
                return Err(integrity("another quote is already bound to this workspace run"));
            }
            tx.commit().map_err(storage)
        }).await
    }

    pub async fn get_pending(
        &self,
        scope: &DeliveryScope,
        run_id: &RunId,
    ) -> CompassResult<PendingDelivery> {
        let (scope, run_id) = (scope.clone(), run_id.clone());
        self.db.with_conn(move |conn| {
            let (quote,saved,acknowledged,retired): (String,bool,bool,bool) = conn.query_row(
                "SELECT quote_json,saved_json IS NOT NULL,acknowledged,retired FROM local_workspace_deliveries
                 WHERE origin=?1 AND org_id=?2 AND subject=?3 AND run_id=?4",
                params![scope.origin,scope.org_id.as_str(),scope.subject,run_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?,r.get(2)?,r.get(3)?))
            ).optional().map_err(storage)?.ok_or_else(missing)?;
            Ok(PendingDelivery{quote:decode(&quote)?,saved,acknowledged,retired})
        }).await
    }

    /// Includes completed local writes whose acknowledgement still needs to reach the hub.
    pub async fn pending(&self, scope: &DeliveryScope) -> CompassResult<Vec<PendingDelivery>> {
        let scope = scope.clone();
        self.db
            .with_conn(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT quote_json,saved_json IS NOT NULL FROM local_workspace_deliveries
                 WHERE origin=?1 AND org_id=?2 AND subject=?3 AND acknowledged=0 AND retired=0 ORDER BY rowid",
                    )
                    .map_err(storage)?;
                let mut rows = stmt
                    .query(params![scope.origin, scope.org_id.as_str(), scope.subject])
                    .map_err(storage)?;
                let mut result = Vec::new();
                while let Some(row) = rows.next().map_err(storage)? {
                    result.push(PendingDelivery {
                        quote: decode(&row.get::<_, String>(0).map_err(storage)?)?,
                        saved: row.get(1).map_err(storage)?,
                        acknowledged: false,
                        retired: false,
                    });
                }
                Ok(result)
            })
            .await
    }

    /// Stop automatic pickup only after a definite unsuccessful terminal state or expired unused quote.
    pub async fn retire_without_result(
        &self,
        scope: &DeliveryScope,
        run_id: &RunId,
    ) -> CompassResult<()> {
        let (scope, run_id) = (scope.clone(), run_id.clone());
        self.db.with_conn(move |conn| {
            let changed = conn.execute("UPDATE local_workspace_deliveries SET retired=1 WHERE origin=?1 AND org_id=?2 AND subject=?3 AND run_id=?4 AND saved_json IS NULL",
                params![scope.origin,scope.org_id.as_str(),scope.subject,run_id.as_str()]).map_err(storage)?;
            if changed == 0 { return Err(missing()); }
            Ok(())
        }).await
    }

    pub async fn list(&self, scope: &DeliveryScope) -> CompassResult<Vec<LocalDelivery>> {
        let scope = scope.clone();
        self.db.with_conn(move |conn| {
            let mut stmt = conn.prepare("SELECT saved_json FROM local_workspace_deliveries
                WHERE origin=?1 AND org_id=?2 AND subject=?3 AND saved_json IS NOT NULL ORDER BY rowid").map_err(storage)?;
            let mut rows = stmt.query(params![scope.origin,scope.org_id.as_str(),scope.subject]).map_err(storage)?;
            let mut result = Vec::new();
            while let Some(row) = rows.next().map_err(storage)? {
                result.push(decode(&row.get::<_,String>(0).map_err(storage)?)?);
            }
            Ok(result)
        }).await
    }

    pub async fn get(&self, scope: &DeliveryScope, run_id: &RunId) -> CompassResult<LocalDelivery> {
        let (scope, run_id) = (scope.clone(), run_id.clone());
        self.db.with_conn(move |conn| {
            let json: String = conn.query_row(
                "SELECT saved_json FROM local_workspace_deliveries WHERE origin=?1 AND org_id=?2 AND subject=?3 AND run_id=?4 AND saved_json IS NOT NULL",
                params![scope.origin,scope.org_id.as_str(),scope.subject,run_id.as_str()], |r| r.get(0)
            ).optional().map_err(storage)?.ok_or_else(missing)?;
            decode(&json)
        }).await
    }

    /// The caller first verifies the signed release against the trusted pending quote.
    /// No row becomes visible until every referenced object has durable, checksum-checked bytes.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish(
        &self,
        scope: &DeliveryScope,
        run: &RunRecord,
        receipt_hash: &str,
        result_sha256: &str,
        provenance: &Value,
        result: &[u8],
        artifacts: &[DeliveryFile],
    ) -> CompassResult<LocalDelivery> {
        if self.read_only {
            return Err(CompassError::AccessDenied(
                "workspace opened read-only".into(),
            ));
        }
        let trusted = self.get_pending(scope, &run.run_id).await?;
        if trusted.retired
            || run.org_id != scope.org_id
            || run.status != RunStatus::Succeeded
            || serde_json::to_value(&trusted.quote).map_err(storage)?
                != serde_json::to_value(&run.quote).map_err(storage)?
            || !valid_hash(receipt_hash)
            || run
                .froglet_receipt
                .as_ref()
                .and_then(|r| r["hash"].as_str())
                != Some(receipt_hash)
        {
            return Err(integrity(
                "delivered result differs from the tracked organization, signed receipt or original quote",
            ));
        }
        if run.artifacts.len() != artifacts.len() {
            return Err(integrity(
                "delivered artifact count differs from the run manifest",
            ));
        }
        let mut names = HashSet::new();
        for (expected, file) in run.artifacts.iter().zip(artifacts) {
            if !valid_name(&expected.name)
                || !names.insert(&expected.name)
                || file.name != expected.name
                || expected.size_bytes != file.bytes.len() as u64
                || !valid_hash(&expected.sha256)
            {
                return Err(integrity(
                    "delivered artifact name or size differs from the run manifest",
                ));
            }
        }
        self.write_object(result_sha256, result).await?;
        for (expected, file) in run.artifacts.iter().zip(artifacts) {
            self.write_object(&expected.sha256, &file.bytes).await?;
        }
        #[cfg(unix)]
        tokio::fs::File::open(&self.objects)
            .await
            .map_err(storage)?
            .sync_all()
            .await
            .map_err(storage)?;
        let saved = LocalDelivery {
            workspace_id: self.workspace_id.clone(),
            origin: scope.origin.clone(),
            org_id: scope.org_id.clone(),
            run: run.clone(),
            receipt_hash: receipt_hash.to_owned(),
            result_sha256: result_sha256.to_owned(),
            provenance: provenance.clone(),
        };
        let subject = scope.subject.clone();
        self.db.with_conn(move |conn| {
            let json = serde_json::to_string(&saved).map_err(storage)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(storage)?;
            let key = params![saved.origin,saved.org_id.as_str(),subject,saved.run.run_id.as_str()];
            let prior:Option<String> = tx.query_row(
                "SELECT saved_json FROM local_workspace_deliveries WHERE origin=?1 AND org_id=?2 AND subject=?3 AND run_id=?4 AND retired=0",
                key, |r|r.get(0)).optional().map_err(storage)?.ok_or_else(missing)?;
            if let Some(prior) = prior {
                if decode::<Value>(&prior)? != serde_json::to_value(&saved).map_err(storage)? {
                    return Err(integrity("workspace run already contains a different released result"));
                }
            } else {
                tx.execute("UPDATE local_workspace_deliveries SET saved_json=?5 WHERE origin=?1 AND org_id=?2 AND subject=?3 AND run_id=?4",
                   params![saved.origin,saved.org_id.as_str(),subject,saved.run.run_id.as_str(),json]).map_err(storage)?;
            }
            tx.commit().map_err(storage)?;
            Ok(saved)
        }).await
    }

    async fn write_object(&self, digest: &str, bytes: &[u8]) -> CompassResult<()> {
        if !valid_hash(digest) || hex::encode(Sha256::digest(bytes)) != digest {
            return Err(integrity("downloaded bytes do not match expected SHA-256"));
        }
        match self.verify_object(digest, Some(bytes.len() as u64)).await {
            Ok(()) => return Ok(()),
            Err(CompassError::NotFound(_) | CompassError::IntegrityViolation(_)) => {}
            Err(e) => return Err(e),
        }
        let temp = self.objects.join(format!(".{}.partial", Uuid::new_v4()));
        let outcome: CompassResult<()> = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut f = options.open(&temp).await.map_err(storage)?;
            f.write_all(bytes).await.map_err(storage)?;
            f.sync_all().await.map_err(storage)?;
            drop(f);
            tokio::fs::rename(&temp, self.objects.join(digest))
                .await
                .map_err(storage)?;
            self.verify_object(digest, Some(bytes.len() as u64)).await
        }
        .await;
        if outcome.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        outcome
    }

    /// Verify durable on-disk output, including already-saved deliveries about to retry their acknowledgement.
    pub async fn verify_saved(
        &self,
        scope: &DeliveryScope,
        run_id: &RunId,
    ) -> CompassResult<LocalDelivery> {
        let saved = self.get(scope, run_id).await?;
        self.verify_object(&saved.result_sha256, None).await?;
        for artifact in &saved.run.artifacts {
            self.verify_object(&artifact.sha256, Some(artifact.size_bytes))
                .await?;
        }
        Ok(saved)
    }

    async fn verify_object(&self, digest: &str, expected_size: Option<u64>) -> CompassResult<()> {
        if !valid_hash(digest) {
            return Err(integrity("invalid stored workspace object digest"));
        }
        let mut file = tokio::fs::File::open(self.objects.join(digest))
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    missing()
                } else {
                    storage(e)
                }
            })?;
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let len = file.read(&mut buffer).await.map_err(storage)?;
            if len == 0 {
                break;
            }
            hash.update(&buffer[..len]);
            bytes += len as u64;
        }
        if hex::encode(hash.finalize()) != digest || expected_size.is_some_and(|size| bytes != size)
        {
            return Err(integrity(
                "stored workspace object differs from expected checksum or size",
            ));
        }
        Ok(())
    }

    pub async fn acknowledge(
        &self,
        scope: &DeliveryScope,
        run_id: &RunId,
        receipt_hash: &str,
    ) -> CompassResult<()> {
        let saved = self.get(scope, run_id).await?;
        if saved.receipt_hash != receipt_hash {
            return Err(integrity("acknowledgement names a different receipt"));
        }
        let (scope, run_id) = (scope.clone(), run_id.clone());
        self.db.with_conn(move |conn| {
            conn.execute("UPDATE local_workspace_deliveries SET acknowledged=1 WHERE origin=?1 AND org_id=?2 AND subject=?3 AND run_id=?4 AND saved_json IS NOT NULL",
                params![scope.origin,scope.org_id.as_str(),scope.subject,run_id.as_str()]).map_err(storage)?;
            Ok(())
        }).await
    }

    async fn read_object(&self, digest: &str) -> CompassResult<Vec<u8>> {
        if !valid_hash(digest) {
            return Err(integrity("invalid stored workspace object digest"));
        }
        let bytes = tokio::fs::read(self.objects.join(digest))
            .await
            .map_err(storage)?;
        if hex::encode(Sha256::digest(&bytes)) != digest {
            return Err(integrity("stored workspace object does not match SHA-256"));
        }
        Ok(bytes)
    }

    /// Store only hashes. Re-observing a session cannot extend it or undo sign-out.
    pub async fn remember_read_session(&self, hash: String, session: SavedReadSession) -> CompassResult<()> {
        self.db.with_conn(move |conn| {
            let now = chrono::Utc::now().timestamp_millis();
            conn.execute("DELETE FROM local_workspace_read_sessions WHERE expires_ms <= ?1", [now])
                .map_err(storage)?;
            let changed = conn.execute(
                "INSERT INTO local_workspace_read_sessions(handle_hash,fingerprint,origin,org_id,subject,expires_ms)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(handle_hash) DO UPDATE SET expires_ms=min(expires_ms,excluded.expires_ms)
                 WHERE revoked=0 AND fingerprint=excluded.fingerprint AND origin=excluded.origin
                   AND org_id=excluded.org_id AND subject=excluded.subject",
                params![hash,session.fingerprint,session.scope.origin,session.scope.org_id.as_str(),
                        session.scope.subject,session.expires_at.timestamp_millis()],
            ).map_err(storage)?;
            if changed == 0 {
                return Err(CompassError::AccessDenied("UNAUTHENTICATED: Sign in again to open saved results.".into()));
            }
            Ok(())
        }).await
    }

    pub async fn read_session(&self, hash: String, origin: String) -> CompassResult<Option<SavedReadSession>> {
        self.db.with_conn(move |conn| {
            let row: Option<(String,String,String,i64,bool)> = conn.query_row(
                "SELECT fingerprint,org_id,subject,expires_ms,revoked FROM local_workspace_read_sessions
                 WHERE handle_hash=?1 AND origin=?2", params![hash,origin],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
            ).optional().map_err(storage)?;
            let Some((fingerprint,org,subject,expires,revoked)) = row else {return Ok(None)};
            if revoked || expires <= chrono::Utc::now().timestamp_millis() {
                return Err(CompassError::AccessDenied("UNAUTHENTICATED: Sign in again to open saved results.".into()));
            }
            let expires_at = chrono::DateTime::from_timestamp_millis(expires)
                .ok_or_else(|| integrity("invalid saved session expiry"))?;
            let org_id = OrgId::new(org).map_err(CompassError::Validation)?;
            Ok(Some(SavedReadSession { fingerprint, scope: DeliveryScope {origin,org_id,subject}, expires_at }))
        }).await
    }

    pub async fn shorten_read_session(&self, hash: String, expires_at: chrono::DateTime<chrono::Utc>) -> CompassResult<()> {
        self.db.with_conn(move |conn| {
            conn.execute(
                "UPDATE local_workspace_read_sessions SET expires_ms=min(expires_ms,?1)
                 WHERE handle_hash=?2 AND revoked=0", params![expires_at.timestamp_millis(),hash],
            ).map_err(storage)?;
            Ok(())
        }).await
    }

    pub async fn revoke_read_session(&self, hash: String) -> CompassResult<()> {
        self.db.with_conn(move |conn| {
            conn.execute("UPDATE local_workspace_read_sessions SET revoked=1 WHERE handle_hash=?1", [hash])
                .map_err(storage)?;
            Ok(())
        }).await
    }

    pub async fn result_bytes(
        &self,
        scope: &DeliveryScope,
        run_id: &RunId,
    ) -> CompassResult<Vec<u8>> {
        let saved = self.get(scope, run_id).await?;
        self.read_object(&saved.result_sha256).await
    }

    pub async fn artifact_bytes(
        &self,
        scope: &DeliveryScope,
        run_id: &RunId,
        name: &str,
    ) -> CompassResult<Vec<u8>> {
        let saved = self.get(scope, run_id).await?;
        let expected = saved
            .run
            .artifacts
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(missing)?;
        let bytes = self.read_object(&expected.sha256).await?;
        if bytes.len() as u64 != expected.size_bytes {
            return Err(integrity(
                "stored artifact size differs from recorded manifest",
            ));
        }
        Ok(bytes)
    }
}
