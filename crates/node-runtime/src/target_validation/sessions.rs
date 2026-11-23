use super::query_error;
use biocompass_core_traits::{CompassError, CompassResult};
use biocompass_data_engine::open_duckdb_with_extensions;
use duckdb::Connection;
use std::sync::{Condvar, Mutex};

// One bounded group per process. All connections share the engine's scheduler and memory.
static SESSIONS: ReadSessions = ReadSessions::new(4);

struct ReadSessions {
    limit: usize,
    database: Mutex<Option<Connection>>,
    active: Mutex<usize>,
    available: Condvar,
}

struct Slot<'a>(&'a ReadSessions);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut active = self.0.active.lock().unwrap_or_else(|err| err.into_inner());
        *active -= 1;
        self.0.available.notify_one();
    }
}

impl ReadSessions {
    const fn new(limit: usize) -> Self {
        Self {
            limit,
            database: Mutex::new(None),
            active: Mutex::new(0),
            available: Condvar::new(),
        }
    }

    fn open(&self) -> CompassResult<(Slot<'_>, Connection)> {
        let unavailable = |_| CompassError::Storage("warm query provider unavailable".into());
        let active = self.active.lock().map_err(unavailable)?;
        let mut active = self
            .available
            .wait_while(active, |n| *n >= self.limit)
            .map_err(unavailable)?;
        *active += 1;
        drop(active);
        let slot = Slot(self);
        // Reserving before initialization also releases a slot if opening DuckDB fails.
        let mut database = self
            .database
            .lock()
            .map_err(|_| CompassError::Storage("warm query database unavailable".into()))?;
        let database = match database.as_ref() {
            Some(existing) => existing,
            None => database.insert(open_duckdb_with_extensions(&[])?),
        };
        let connection = database.try_clone().map_err(query_error)?;
        Ok((slot, connection))
    }
}

pub(super) fn warm<T>(action: impl FnOnce(&Connection) -> CompassResult<T>) -> CompassResult<T> {
    let (_slot, mut connection) = SESSIONS.open()?;
    // Grants, selectors, temporary views and transactions belong to this request's connection.
    let txn = connection.transaction().map_err(query_error)?;
    let result = action(&txn);
    txn.rollback().map_err(query_error)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn occupied_sessions_wait_for_one_slot_to_return() {
        let sessions = ReadSessions::new(2);
        let first = sessions.open().unwrap();
        let second = sessions.open().unwrap();
        // Both sessions share the same in-memory database, while TEMP tables remain private.
        first
            .1
            .execute_batch("CREATE TABLE provider_marker AS SELECT 42 AS value")
            .unwrap();
        let marker: i32 = second
            .1
            .query_row("SELECT value FROM provider_marker", [], |r| r.get(0))
            .unwrap();
        assert_eq!(marker, 42);
        let (trying, ready) = mpsc::channel();
        let (admitted, next) = mpsc::channel();
        thread::scope(|scope| {
            scope.spawn(|| {
                trying.send(()).unwrap();
                let _third = sessions.open().unwrap();
                admitted.send(()).unwrap();
            });
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            let waiting = next.recv_timeout(Duration::from_millis(100));
            drop(first);
            assert_eq!(waiting, Err(mpsc::RecvTimeoutError::Timeout));
            next.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        drop(second);
        assert_eq!(*sessions.active.lock().unwrap(), 0);
    }

    #[test]
    fn failed_request_discards_its_private_tables() {
        let result = warm(|conn| {
            conn.execute_batch("CREATE TEMP TABLE interrupted_grant(sample VARCHAR)")
                .map_err(query_error)?;
            Err::<(), _>(CompassError::AccessDenied("simulated revocation".into()))
        });
        assert!(matches!(result, Err(CompassError::AccessDenied(_))));
        let count: i64 = warm(|conn| {
            conn.query_row(
                "SELECT count(*) FROM duckdb_tables() WHERE table_name='interrupted_grant'",
                [],
                |r| r.get(0),
            )
            .map_err(query_error)
        })
        .unwrap();
        assert_eq!(count, 0);
    }
}
