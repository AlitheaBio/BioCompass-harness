use biocompass_core_traits::{CatalogId, RunRecord};
use biocompass_node_runtime::{
    verify_released_artifact, verify_released_result, verify_released_result_commitment,
};
use froglet_protocol::crypto::sha256_hex;
use serde_json::Value;

fn released_run() -> RunRecord {
    // A signed, synthetic operator_ledger.v1 delivery with one catalog and two attachments.
    // Reconstruct the duplicate convenience fields from the five original signed documents.
    let mut value: Value =
        serde_json::from_str(include_str!("fixtures/released_run.json")).unwrap();
    value["quote"]["froglet_quote"] = value["froglet_chain"][2].clone();
    value["froglet_receipt"] = value["froglet_chain"][4].clone();
    serde_json::from_value(value).unwrap()
}

#[test]
fn verifies_original_result_and_each_indexed_attachment() {
    let run = released_run();
    let catalog = CatalogId::from("fixture/table");
    let digest = "22".repeat(32);
    let source = Some((&catalog, 2, digest.as_str()));
    verify_released_result(&run, b" null\n", source).unwrap();
    for (asset, bytes) in run
        .artifacts
        .iter()
        .zip([b"x,y\n1,2\n".as_slice(), b"{\"attachment\":true}\n"])
    {
        verify_released_artifact(asset, &sha256_hex(bytes), bytes.len() as u64).unwrap();
        assert!(
            verify_released_artifact(asset, &sha256_hex(b"changed"), bytes.len() as u64).is_err()
        );
        assert!(verify_released_artifact(asset, &asset.sha256, asset.size_bytes + 1).is_err());
    }
    assert_eq!(run.artifacts[1].name, "result.json");
    assert!(verify_released_result(&run, b"null", source).is_err());
}

#[test]
fn verifies_a_streamed_companion_digest_and_rejects_changed_artifact_metadata() {
    let run = released_run();
    let catalog = CatalogId::from("fixture/table");
    let digest = "22".repeat(32);
    let source = Some((&catalog, 2, digest.as_str()));
    let result_sha = sha256_hex(b" null\n");
    verify_released_result_commitment(&run, &result_sha, source).unwrap();
    assert!(verify_released_result_commitment(&run, &sha256_hex(b"null"), source).is_err());
    assert!(verify_released_result_commitment(&run, &result_sha, None).is_err());

    let mut changed = run.clone();
    changed.artifacts[0].sha256 = sha256_hex(b"tampered artifact");
    assert!(verify_released_result_commitment(&changed, &result_sha, source).is_err());

    let mut changed = run;
    changed.result_json = Some(Value::Bool(true));
    // Digest-only verification authorizes reading a sibling, never this convenience value.
    verify_released_result_commitment(&changed, &result_sha, source).unwrap();
    assert!(verify_released_result(&changed, b" null\n", source).is_err());
}

#[test]
fn rejects_swapped_manifest_incomplete_chain_and_wrong_origin_inputs() {
    let run = released_run();
    let catalog = CatalogId::from("fixture/table");
    let digest = "22".repeat(32);
    let source = Some((&catalog, 2, digest.as_str()));
    assert!(verify_released_result(&run, b" null\n", None).is_err());
    assert!(verify_released_result(&run, b" null\n", Some((&catalog, 3, &digest))).is_err());
    assert!(
        verify_released_result(&run, b" null\n", Some((&catalog, 2, &"33".repeat(32)))).is_err()
    );
    let mut changed = run.clone();
    changed.artifacts.swap(0, 1);
    assert!(verify_released_result(&changed, b" null\n", source).is_err());
    let mut changed = run.clone();
    changed.artifacts[0].blob_key = "runs/other/artifacts/nested/file.csv".into();
    assert!(verify_released_result(&changed, b" null\n", source).is_err());
    let mut changed = run.clone();
    changed.plugin_version = "2.0".into();
    changed.quote.plugin_version = changed.plugin_version.clone();
    assert!(verify_released_result(&changed, b" null\n", source).is_err());
    let mut changed = run;
    changed.froglet_chain.pop();
    assert!(verify_released_result(&changed, b" null\n", source).is_err());
}
