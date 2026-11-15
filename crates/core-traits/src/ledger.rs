//! Atomic double-entry ACT credit ledger implementing `PaymentGateway` (`reserve -> settle -> refund`).
//!
//! Supports zero-vendor B2B invoice top-ups, webhook top-ups, idempotency keys, and initial local grants.

use crate::ids::{CreditAmountAct, OrgId, RunId};
use crate::traits::PaymentGateway;
use crate::types::{CompassError, CompassResult, LedgerTx, LedgerTxKind};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// SQLite-backed atomic double-entry ACT ledger for local & B2B deployments.
#[derive(Clone)]
pub struct SqliteActLedger {
    conn: Arc<Mutex<Connection>>,
    default_genesis_act: CreditAmountAct,
}

impl SqliteActLedger {
    /// Open or create a persistent SQLite ACT ledger at `db_path`.
    pub fn open(
        db_path: impl AsRef<Path>,
        default_genesis_act: CreditAmountAct,
    ) -> CompassResult<Self> {
        let conn = Connection::open(db_path.as_ref())
            .map_err(|e| CompassError::Storage(format!("open ledger sqlite: {e}")))?;
        Self::init(conn, default_genesis_act)
    }

    /// Open an in-memory SQLite ACT ledger for unit tests.
    pub fn in_memory(default_genesis_act: CreditAmountAct) -> CompassResult<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| CompassError::Storage(format!("open memory ledger: {e}")))?;
        Self::init(conn, default_genesis_act)
    }

    fn init(conn: Connection, default_genesis_act: CreditAmountAct) -> CompassResult<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS org_balances (
                 org_id TEXT PRIMARY KEY,
                 available_milli_act INTEGER NOT NULL,
                 reserved_milli_act INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS run_reservations (
                 run_id TEXT PRIMARY KEY,
                 org_id TEXT NOT NULL,
                 amount_milli_act INTEGER NOT NULL,
                 state TEXT NOT NULL,
                 idempotency_key TEXT NOT NULL UNIQUE
             );
             CREATE TABLE IF NOT EXISTS ledger_txs (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 tx_id TEXT NOT NULL UNIQUE,
                 org_id TEXT NOT NULL,
                 run_id TEXT,
                 kind TEXT NOT NULL,
                 delta_milli_act INTEGER NOT NULL,
                 balance_after_milli_act INTEGER NOT NULL,
                 reference TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );",
        )
        .map_err(|e| CompassError::Storage(format!("init ledger schema: {e}")))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            default_genesis_act,
        })
    }

    fn ensure_org(
        tx: &rusqlite::Transaction<'_>,
        org_id: &OrgId,
        genesis: CreditAmountAct,
    ) -> CompassResult<(i64, i64)> {
        let existing: Option<(i64, i64)> = tx
            .query_row(
                "SELECT available_milli_act, reserved_milli_act FROM org_balances WHERE org_id = ?1",
                params![org_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| CompassError::Payment(format!("query org_balances: {e}")))?;

        if let Some(bal) = existing {
            return Ok(bal);
        }

        let start = genesis.milli_act();
        tx.execute(
            "INSERT INTO org_balances (org_id, available_milli_act, reserved_milli_act) VALUES (?1, ?2, 0)",
            params![org_id.as_str(), start],
        )
        .map_err(|e| CompassError::Payment(format!("insert genesis balance: {e}")))?;

        if start > 0 {
            let kind_str = serde_json::to_string(&LedgerTxKind::GenesisGrant)
                .map_err(|e| CompassError::Payment(e.to_string()))?;
            tx.execute(
                "INSERT INTO ledger_txs (tx_id, org_id, run_id, kind, delta_milli_act, balance_after_milli_act, reference, created_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7)",
                params![
                    format!("tx-{}", Uuid::new_v4().simple()),
                    org_id.as_str(),
                    kind_str,
                    start,
                    start,
                    "Initial Workspace ACT Allocation",
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|e| CompassError::Payment(format!("insert genesis tx: {e}")))?;
        }
        Ok((start, 0))
    }

    async fn with_conn<F, T>(&self, f: F) -> CompassResult<T>
    where
        F: FnOnce(&mut Connection, CreditAmountAct) -> CompassResult<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        let genesis = self.default_genesis_act;
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| CompassError::Internal("ledger mutex poisoned".into()))?;
            f(&mut guard, genesis)
        })
        .await
        .map_err(|e| CompassError::Internal(format!("ledger join error: {e}")))?
    }
}

#[async_trait]
impl PaymentGateway for SqliteActLedger {
    async fn available_balance(&self, org_id: &OrgId) -> CompassResult<CreditAmountAct> {
        let oid = org_id.clone();
        self.with_conn(move |conn, genesis| {
            let tx = conn
                .transaction()
                .map_err(|e| CompassError::Payment(format!("begin balance tx: {e}")))?;
            let (avail, _) = Self::ensure_org(&tx, &oid, genesis)?;
            tx.commit()
                .map_err(|e| CompassError::Payment(format!("commit balance tx: {e}")))?;
            Ok(CreditAmountAct::from_milli_act(avail))
        })
        .await
    }

    async fn reserve_for_run(
        &self,
        org_id: &OrgId,
        run_id: &RunId,
        amount: CreditAmountAct,
        idempotency_key: &str,
    ) -> CompassResult<LedgerTx> {
        if amount.milli_act() < 0 {
            return Err(CompassError::Validation(
                "reserve amount cannot be negative".into(),
            ));
        }
        let oid = org_id.clone();
        let rid = run_id.clone();
        let idem = idempotency_key.trim().to_owned();
        if idem.is_empty() {
            return Err(CompassError::Validation(
                "idempotency_key is required for ACT reservation".into(),
            ));
        }

        self.with_conn(move |conn, genesis| {
            let tx = conn
                .transaction()
                .map_err(|e| CompassError::Payment(format!("begin reserve tx: {e}")))?;

            // Idempotent replay check
            if let Some(existing_run) = tx
                .query_row(
                    "SELECT run_id FROM run_reservations WHERE idempotency_key = ?1",
                    params![idem],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| CompassError::Payment(format!("check idempotency_key: {e}")))?
            {
                if existing_run != rid.as_str() {
                    return Err(CompassError::Payment(format!(
                        "idempotency_key '{idem}' already bound to run '{existing_run}'"
                    )));
                }
                let (avail, _) = Self::ensure_org(&tx, &oid, genesis)?;
                return Ok(LedgerTx {
                    tx_id: format!("tx-idem-{}", rid.as_str()),
                    org_id: oid,
                    run_id: Some(rid),
                    kind: LedgerTxKind::RunReserve,
                    delta_act: CreditAmountAct::from_milli_act(-amount.milli_act()),
                    balance_after_act: CreditAmountAct::from_milli_act(avail),
                    reference: format!("Idempotent replay ({idem})"),
                    created_at: Utc::now(),
                });
            }

            let (avail, reserved) = Self::ensure_org(&tx, &oid, genesis)?;
            let needed = amount.milli_act();
            if avail < needed {
                return Err(CompassError::Payment(format!(
                    "insufficient ACT balance: need {}, available {}",
                    amount.display_act(),
                    CreditAmountAct::from_milli_act(avail).display_act()
                )));
            }

            let new_avail = avail - needed;
            let new_reserved = reserved + needed;
            tx.execute(
                "UPDATE org_balances SET available_milli_act = ?1, reserved_milli_act = ?2 WHERE org_id = ?3",
                params![new_avail, new_reserved, oid.as_str()],
            )
            .map_err(|e| CompassError::Payment(format!("update reserve balance: {e}")))?;

            tx.execute(
                "INSERT INTO run_reservations (run_id, org_id, amount_milli_act, state, idempotency_key)
                 VALUES (?1, ?2, ?3, 'reserved', ?4)",
                params![rid.as_str(), oid.as_str(), needed, idem],
            )
            .map_err(|e| CompassError::Payment(format!("insert run_reservation: {e}")))?;

            let entry = LedgerTx {
                tx_id: format!("tx-{}", Uuid::new_v4().simple()),
                org_id: oid.clone(),
                run_id: Some(rid.clone()),
                kind: LedgerTxKind::RunReserve,
                delta_act: CreditAmountAct::from_milli_act(-needed),
                balance_after_act: CreditAmountAct::from_milli_act(new_avail),
                reference: format!("Reserved for run {}", rid.as_str()),
                created_at: Utc::now(),
            };
            let kind_str = serde_json::to_string(&entry.kind)
                .map_err(|e| CompassError::Payment(e.to_string()))?;
            tx.execute(
                "INSERT INTO ledger_txs (tx_id, org_id, run_id, kind, delta_milli_act, balance_after_milli_act, reference, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    entry.tx_id,
                    oid.as_str(),
                    rid.as_str(),
                    kind_str,
                    entry.delta_act.milli_act(),
                    new_avail,
                    entry.reference,
                    entry.created_at.to_rfc3339(),
                ],
            )
            .map_err(|e| CompassError::Payment(format!("insert reserve ledger_tx: {e}")))?;

            tx.commit()
                .map_err(|e| CompassError::Payment(format!("commit reserve tx: {e}")))?;
            Ok(entry)
        })
        .await
    }

    async fn settle_run(
        &self,
        org_id: &OrgId,
        run_id: &RunId,
        receipt_hash: &str,
    ) -> CompassResult<LedgerTx> {
        let oid = org_id.clone();
        let rid = run_id.clone();
        let r_hash = receipt_hash.to_owned();
        self.with_conn(move |conn, genesis| {
            let tx = conn
                .transaction()
                .map_err(|e| CompassError::Payment(format!("begin settle tx: {e}")))?;
            let (amount, state): (i64, String) = tx
                .query_row(
                    "SELECT amount_milli_act, state FROM run_reservations WHERE run_id = ?1 AND org_id = ?2",
                    params![rid.as_str(), oid.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| CompassError::Payment(format!("query reservation: {e}")))?
                .ok_or_else(|| CompassError::NotFound(format!("reservation for run {rid}")))?;

            let (avail, reserved) = Self::ensure_org(&tx, &oid, genesis)?;
            if state != "reserved" {
                return Err(CompassError::Payment(format!(
                    "cannot settle run {rid} in reservation state '{state}'"
                )));
            }
            let new_reserved = (reserved - amount).max(0);
            tx.execute(
                "UPDATE org_balances SET reserved_milli_act = ?1 WHERE org_id = ?2",
                params![new_reserved, oid.as_str()],
            )
            .map_err(|e| CompassError::Payment(format!("update settle balance: {e}")))?;
            tx.execute(
                "UPDATE run_reservations SET state = 'settled' WHERE run_id = ?1",
                params![rid.as_str()],
            )
            .map_err(|e| CompassError::Payment(format!("mark reservation settled: {e}")))?;

            let entry = LedgerTx {
                tx_id: format!("tx-{}", Uuid::new_v4().simple()),
                org_id: oid.clone(),
                run_id: Some(rid.clone()),
                kind: LedgerTxKind::RunSettle,
                delta_act: CreditAmountAct::ZERO,
                balance_after_act: CreditAmountAct::from_milli_act(avail),
                reference: format!("Settled on verified Froglet Receipt {r_hash}"),
                created_at: Utc::now(),
            };
            let kind_str = serde_json::to_string(&entry.kind)
                .map_err(|e| CompassError::Payment(e.to_string()))?;
            tx.execute(
                "INSERT INTO ledger_txs (tx_id, org_id, run_id, kind, delta_milli_act, balance_after_milli_act, reference, created_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)",
                params![
                    entry.tx_id,
                    oid.as_str(),
                    rid.as_str(),
                    kind_str,
                    avail,
                    entry.reference,
                    entry.created_at.to_rfc3339(),
                ],
            )
            .map_err(|e| CompassError::Payment(format!("insert settle ledger_tx: {e}")))?;

            tx.commit()
                .map_err(|e| CompassError::Payment(format!("commit settle tx: {e}")))?;
            Ok(entry)
        })
        .await
    }

    async fn refund_run(
        &self,
        org_id: &OrgId,
        run_id: &RunId,
        reason: &str,
    ) -> CompassResult<LedgerTx> {
        let oid = org_id.clone();
        let rid = run_id.clone();
        let why = reason.to_owned();
        self.with_conn(move |conn, genesis| {
            let tx = conn
                .transaction()
                .map_err(|e| CompassError::Payment(format!("begin refund tx: {e}")))?;
            let (amount, state): (i64, String) = tx
                .query_row(
                    "SELECT amount_milli_act, state FROM run_reservations WHERE run_id = ?1 AND org_id = ?2",
                    params![rid.as_str(), oid.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| CompassError::Payment(format!("query reservation: {e}")))?
                .ok_or_else(|| CompassError::NotFound(format!("reservation for run {rid}")))?;

            if state != "reserved" {
                return Err(CompassError::Payment(format!(
                    "cannot refund run {rid} in reservation state '{state}'"
                )));
            }
            let (avail, reserved) = Self::ensure_org(&tx, &oid, genesis)?;
            let new_avail = avail + amount;
            let new_reserved = (reserved - amount).max(0);
            tx.execute(
                "UPDATE org_balances SET available_milli_act = ?1, reserved_milli_act = ?2 WHERE org_id = ?3",
                params![new_avail, new_reserved, oid.as_str()],
            )
            .map_err(|e| CompassError::Payment(format!("update refund balance: {e}")))?;
            tx.execute(
                "UPDATE run_reservations SET state = 'refunded' WHERE run_id = ?1",
                params![rid.as_str()],
            )
            .map_err(|e| CompassError::Payment(format!("mark reservation refunded: {e}")))?;

            let entry = LedgerTx {
                tx_id: format!("tx-{}", Uuid::new_v4().simple()),
                org_id: oid.clone(),
                run_id: Some(rid.clone()),
                kind: LedgerTxKind::RunRefund,
                delta_act: CreditAmountAct::from_milli_act(amount),
                balance_after_act: CreditAmountAct::from_milli_act(new_avail),
                reference: format!("Refunded failed/canceled run: {why}"),
                created_at: Utc::now(),
            };
            let kind_str = serde_json::to_string(&entry.kind)
                .map_err(|e| CompassError::Payment(e.to_string()))?;
            tx.execute(
                "INSERT INTO ledger_txs (tx_id, org_id, run_id, kind, delta_milli_act, balance_after_milli_act, reference, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    entry.tx_id,
                    oid.as_str(),
                    rid.as_str(),
                    kind_str,
                    amount,
                    new_avail,
                    entry.reference,
                    entry.created_at.to_rfc3339(),
                ],
            )
            .map_err(|e| CompassError::Payment(format!("insert refund ledger_tx: {e}")))?;

            tx.commit()
                .map_err(|e| CompassError::Payment(format!("commit refund tx: {e}")))?;
            Ok(entry)
        })
        .await
    }

    async fn credit_topup(
        &self,
        org_id: &OrgId,
        amount: CreditAmountAct,
        reference: &str,
        via_webhook: bool,
    ) -> CompassResult<LedgerTx> {
        if amount.milli_act() <= 0 {
            return Err(CompassError::Validation("top-up amount must be positive".into()));
        }
        let oid = org_id.clone();
        let ref_text = reference.trim().to_owned();
        self.with_conn(move |conn, genesis| {
            let tx = conn
                .transaction()
                .map_err(|e| CompassError::Payment(format!("begin topup tx: {e}")))?;
            let (avail, _) = Self::ensure_org(&tx, &oid, genesis)?;
            let new_avail = avail + amount.milli_act();
            tx.execute(
                "UPDATE org_balances SET available_milli_act = ?1 WHERE org_id = ?2",
                params![new_avail, oid.as_str()],
            )
            .map_err(|e| CompassError::Payment(format!("update topup balance: {e}")))?;

            let kind = if via_webhook {
                LedgerTxKind::WebhookTopUp
            } else {
                LedgerTxKind::InvoiceTopUp
            };
            let entry = LedgerTx {
                tx_id: format!("tx-{}", Uuid::new_v4().simple()),
                org_id: oid.clone(),
                run_id: None,
                kind,
                delta_act: amount,
                balance_after_act: CreditAmountAct::from_milli_act(new_avail),
                reference: ref_text,
                created_at: Utc::now(),
            };
            let kind_str = serde_json::to_string(&entry.kind)
                .map_err(|e| CompassError::Payment(e.to_string()))?;
            tx.execute(
                "INSERT INTO ledger_txs (tx_id, org_id, run_id, kind, delta_milli_act, balance_after_milli_act, reference, created_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7)",
                params![
                    entry.tx_id,
                    oid.as_str(),
                    kind_str,
                    amount.milli_act(),
                    new_avail,
                    entry.reference,
                    entry.created_at.to_rfc3339(),
                ],
            )
            .map_err(|e| CompassError::Payment(format!("insert topup ledger_tx: {e}")))?;

            tx.commit()
                .map_err(|e| CompassError::Payment(format!("commit topup tx: {e}")))?;
            Ok(entry)
        })
        .await
    }

    async fn list_transactions(&self, org_id: &OrgId) -> CompassResult<Vec<LedgerTx>> {
        let oid = org_id.clone();
        self.with_conn(move |conn, genesis| {
            let tx = conn
                .transaction()
                .map_err(|e| CompassError::Payment(format!("begin list_txs: {e}")))?;
            let _ = Self::ensure_org(&tx, &oid, genesis)?;
            tx.commit()
                .map_err(|e| CompassError::Payment(format!("commit ensure_org: {e}")))?;

            let mut stmt = conn
                .prepare(
                    "SELECT tx_id, org_id, run_id, kind, delta_milli_act, balance_after_milli_act, reference, created_at
                     FROM ledger_txs WHERE org_id = ?1 ORDER BY seq DESC",
                )
                .map_err(|e| CompassError::Payment(format!("prepare list_txs: {e}")))?;
            let rows = stmt
                .query_map(params![oid.as_str()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                    ))
                })
                .map_err(|e| CompassError::Payment(format!("query list_txs: {e}")))?;

            let mut out = Vec::new();
            for row in rows {
                let r = row.map_err(|e| CompassError::Payment(format!("read tx row: {e}")))?;
                let kind: LedgerTxKind = serde_json::from_str(&r.3)
                    .map_err(|e| CompassError::Payment(format!("decode tx kind: {e}")))?;
                let created_at = DateTime::parse_from_rfc3339(&r.7)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|e| CompassError::Payment(format!("parse tx ts: {e}")))?;
                out.push(LedgerTx {
                    tx_id: r.0,
                    org_id: OrgId::from(r.1),
                    run_id: r.2.map(RunId::from),
                    kind,
                    delta_act: CreditAmountAct::from_milli_act(r.4),
                    balance_after_act: CreditAmountAct::from_milli_act(r.5),
                    reference: r.6,
                    created_at,
                });
            }
            Ok(out)
        })
        .await
    }
}
