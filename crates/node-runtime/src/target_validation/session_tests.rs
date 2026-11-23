use super::{query_error, warm};
use biocompass_core_traits::CompassResult;
use duckdb::Connection;
use std::sync::mpsc;
use std::time::Duration;

fn populate(conn: &Connection, sample: &str) -> CompassResult<()> {
    conn.execute_batch("CREATE TEMP TABLE allowed_samples(id VARCHAR PRIMARY KEY)")
        .map_err(query_error)?;
    conn.execute("INSERT INTO allowed_samples VALUES (?)", [sample])
        .map_err(query_error)?;
    Ok(())
}

fn selected(conn: &Connection) -> CompassResult<String> {
    conn.query_row("SELECT id FROM allowed_samples", [], |row| row.get(0))
        .map_err(query_error)
}

#[test]
fn concurrent_requests_have_private_sample_tables() {
    let (entered, first_ready) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let (finished, second_ready) = mpsc::channel();
    std::thread::scope(|scope| {
        let first = scope.spawn(move || {
            warm(|conn| {
                populate(conn, "org-a-sample")?;
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(30)).unwrap();
                selected(conn)
            })
        });
        first_ready.recv_timeout(Duration::from_secs(10)).unwrap();
        scope.spawn(move || {
            let result = warm(|conn| {
                populate(conn, "org-b-sample")?;
                selected(conn)
            });
            let _ = finished.send(result);
        });
        // The second transaction must finish while the first still holds its own scope.
        let second = second_ready.recv_timeout(Duration::from_secs(10));
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap().unwrap(), "org-a-sample");
        assert_eq!(
            second
                .expect("second query blocked on the first transaction")
                .unwrap(),
            "org-b-sample"
        );
    });
}
