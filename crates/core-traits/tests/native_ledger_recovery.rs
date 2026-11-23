#![cfg(feature = "local-sqlite")]

use biocompass_core_traits::{
    CreditAmountAct, OrgId, PaymentGateway, ReservationState, RunId, RunReservation,
    SqliteActLedger,
};
use tempfile::tempdir;

#[tokio::test]
async fn reserve_replay_preserves_terms_and_actual_transaction() {
    let ledger = SqliteActLedger::in_memory(CreditAmountAct(100)).unwrap();
    let org = OrgId::from("test-buyer");
    let run = RunId::from("run-measurements");
    let original = ledger
        .reserve_for_run(&org, &run, CreditAmountAct(15), "request")
        .await
        .unwrap();
    ledger
        .credit_topup(&org, CreditAmountAct(3), "test invoice", false)
        .await
        .unwrap();
    let replay = ledger
        .reserve_for_run(&org, &run, CreditAmountAct(15), "request")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(replay).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    assert!(
        ledger
            .reserve_for_run(&org, &run, CreditAmountAct(17), "request")
            .await
            .is_err()
    );
    assert!(
        ledger
            .reserve_for_run(
                &OrgId::from("other-org"),
                &run,
                CreditAmountAct(15),
                "request"
            )
            .await
            .is_err()
    );
    assert!(
        ledger
            .reserve_for_run(&org, &run, CreditAmountAct(15), "another")
            .await
            .is_err()
    );
    assert_eq!(
        ledger.available_balance(&org).await.unwrap(),
        CreditAmountAct(88)
    );
}

#[tokio::test]
async fn terminal_replay_after_reopen_keeps_exact_receipt_and_refund() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("ledger.sqlite");
    let org = OrgId::from("buyer");
    let success = RunId::from("run-success");
    let failed = RunId::from("run-failed");
    let initial = SqliteActLedger::open(&db, CreditAmountAct(100)).unwrap();
    initial
        .reserve_for_run(&org, &success, CreditAmountAct(17), "ok")
        .await
        .unwrap();
    let settle = initial
        .settle_run(&org, &success, "receipt-hash")
        .await
        .unwrap();
    initial
        .reserve_for_run(&org, &failed, CreditAmountAct(15), "fail")
        .await
        .unwrap();
    let refund = initial
        .refund_run(&org, &failed, "execution failed")
        .await
        .unwrap();
    drop(initial);
    let reopened = SqliteActLedger::open(&db, CreditAmountAct(100)).unwrap();
    let paid = reopened
        .settle_run(&org, &success, "receipt-hash")
        .await
        .unwrap();
    let returned = reopened
        .refund_run(&org, &failed, "interrupted recovery")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(paid).unwrap(),
        serde_json::to_value(settle).unwrap()
    );
    assert_eq!(
        serde_json::to_value(returned).unwrap(),
        serde_json::to_value(refund).unwrap()
    );
    assert!(
        reopened
            .settle_run(&org, &success, "conflicting")
            .await
            .is_err()
    );
    assert!(
        reopened
            .refund_run(&org, &success, "late error")
            .await
            .is_err()
    );
    assert!(
        reopened
            .settle_run(&org, &failed, "receipt-hash")
            .await
            .is_err()
    );
    assert!(
        reopened
            .reserve_for_run(&org, &success, CreditAmountAct(17), "ok")
            .await
            .is_err()
    );
    assert!(
        reopened
            .reserve_for_run(&org, &failed, CreditAmountAct(15), "fail")
            .await
            .is_err()
    );
    assert_eq!(
        reopened.available_balance(&org).await.unwrap(),
        CreditAmountAct(83)
    );
    assert_eq!(reopened.list_transactions(&org).await.unwrap().len(), 5);
}

#[tokio::test]
async fn separately_opened_ledgers_contend_for_one_terminal_outcome() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("ledger.sqlite");
    let left = SqliteActLedger::open(&db, CreditAmountAct(100)).unwrap();
    let right = SqliteActLedger::open(&db, CreditAmountAct(100)).unwrap();
    let org = OrgId::from("buyer");
    let run = RunId::from("run-race");
    let terms = RunReservation {
        idempotency_key: "same-request".to_owned(),
        amount: CreditAmountAct(7),
        state: ReservationState::Reserved,
    };
    assert_eq!(left.run_reservation(&org, &run).await.unwrap(), None);
    let (a, b) = tokio::join!(
        left.reserve_for_run(&org, &run, terms.amount, &terms.idempotency_key),
        right.reserve_for_run(&org, &run, terms.amount, &terms.idempotency_key)
    );
    assert_eq!(a.unwrap().tx_id, b.unwrap().tx_id);
    assert_eq!(
        right.run_reservation(&org, &run).await.unwrap(),
        Some(terms.clone())
    );
    assert_eq!(
        right
            .run_reservation(&OrgId::from("other"), &run)
            .await
            .unwrap(),
        None
    );
    let (settled, refunded) = tokio::join!(
        left.settle_run(&org, &run, "verified-receipt"),
        right.refund_run(&org, &run, "interrupted execution"),
    );
    let (state, available) = match (settled, refunded) {
        (Ok(entry), Err(_)) => {
            assert_eq!(
                right
                    .settle_run(&org, &run, "verified-receipt")
                    .await
                    .unwrap()
                    .tx_id,
                entry.tx_id
            );
            (ReservationState::Settled, 93)
        }
        (Err(_), Ok(entry)) => {
            assert_eq!(
                left.refund_run(&org, &run, "reopening")
                    .await
                    .unwrap()
                    .tx_id,
                entry.tx_id
            );
            (ReservationState::Refunded, 100)
        }
        other => panic!("exactly one terminal outcome must succeed: {other:?}"),
    };
    let reopened = SqliteActLedger::open(&db, CreditAmountAct(100)).unwrap();
    assert_eq!(
        reopened.run_reservation(&org, &run).await.unwrap(),
        Some(RunReservation { state, ..terms })
    );
    assert_eq!(
        reopened.available_balance(&org).await.unwrap(),
        CreditAmountAct(available)
    );
    assert_eq!(reopened.list_transactions(&org).await.unwrap().len(), 3);
}

#[tokio::test]
async fn legacy_settlement_replays_only_with_its_original_immutable_receipt_reference() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("legacy.sqlite");
    let org = OrgId::from("buyer");
    let run = RunId::from("run-free");
    let first = SqliteActLedger::open(&db, CreditAmountAct::ZERO).unwrap();
    first
        .reserve_for_run(&org, &run, CreditAmountAct::ZERO, "original-free")
        .await
        .unwrap();
    let tx = first
        .settle_run(&org, &run, "legacy-signed-hash")
        .await
        .unwrap();
    drop(first);
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "ALTER TABLE ledger_txs DROP COLUMN receipt_hash;
         ALTER TABLE run_reservations DROP COLUMN receipt_hash;",
        )
        .unwrap();
    let upgraded = SqliteActLedger::open(&db, CreditAmountAct::ZERO).unwrap();
    assert_eq!(
        upgraded
            .run_reservation(&org, &run)
            .await
            .unwrap()
            .unwrap()
            .state,
        ReservationState::Settled
    );
    let replay = upgraded
        .settle_run(&org, &run, "legacy-signed-hash")
        .await
        .unwrap();
    assert_eq!(replay.tx_id, tx.tx_id);
    assert_eq!(replay.receipt_hash, None);
    assert!(
        upgraded
            .settle_run(&org, &run, "different-hash")
            .await
            .is_err()
    );
    assert!(upgraded.refund_run(&org, &run, "late").await.is_err());
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE ledger_txs SET reference='historical receipt unknown' WHERE tx_id=?1",
            [&tx.tx_id],
        )
        .unwrap();
    assert!(
        upgraded
            .settle_run(&org, &run, "legacy-signed-hash")
            .await
            .is_err()
    );
    assert_eq!(
        upgraded.available_balance(&org).await.unwrap(),
        CreditAmountAct::ZERO
    );
    assert_eq!(upgraded.list_transactions(&org).await.unwrap().len(), 2);
}
