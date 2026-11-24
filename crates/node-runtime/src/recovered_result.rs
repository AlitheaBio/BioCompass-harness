use super::*;
use biocompass_core_traits::result_manifest_hash;
use froglet_verify::DocumentStatus;

fn inconsistent(reason: impl std::fmt::Display) -> CompassError {
    CompassError::IntegrityViolation(format!("native signed result recovery: {reason}"))
}

/// Return a terminal candidate only when its existing local receipt and outputs verify.
/// The caller owns the monetary transaction and the final SQLite state transition.
pub(super) async fn load_signed_outcome(
    runtime: &InProcessNodeRuntime,
    run: &RunRecord,
    key: &str,
) -> CompassResult<Option<RunRecord>> {
    if !matches!(
        (run.status, run.froglet_chain.len()),
        (RunStatus::Prepared, 3) | (RunStatus::Running, 3 | 4)
    ) {
        return Err(inconsistent(
            "expected a pending record with its original signed quote or deal",
        ));
    }
    if run.receipt_verified
        || run.froglet_receipt.is_some()
        || run.result_json.is_some()
        || !run.artifacts.is_empty()
        || run.completed_at.is_some()
    {
        return Err(inconsistent("pending row has unexpected terminal metadata"));
    }
    let store = runtime.blob_store.as_ref();
    let path = format!("runs/{}/froglet_chain.json", run.run_id);
    let docs: Vec<Value> = match store.get_bytes(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| inconsistent(format!("invalid chain mirror: {error}")))?,
        Err(CompassError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    if !docs.starts_with(&run.froglet_chain) {
        return Err(inconsistent(
            "chain mirror differs from the original signed quote or deal",
        ));
    }
    let links = local_brokered_links(runtime, run).await?;
    let receipt = verify_native_chain(runtime, run, key, &docs, &links)?;
    let mut recovered = run.clone();
    let payload = receipt.payload;
    match (
        payload.deal_state.as_str(),
        payload.execution_state.as_str(),
    ) {
        ("succeeded", "succeeded") => {
            let result_path = format!("runs/{}/result.json", run.run_id);
            if payload.result_ref != Some(format!("blob://{result_path}"))
                || payload.failure_code.is_some()
                || payload.failure_message.is_some()
            {
                return Err(inconsistent(
                    "successful receipt has inconsistent output or failure metadata",
                ));
            }
            let bytes = store.get_bytes(&result_path).await?;
            let result: Value = serde_json::from_slice(&bytes)
                .map_err(|error| inconsistent(format!("invalid result JSON: {error}")))?;
            let artifacts = recover_legacy_artifacts(store, &run.run_id).await?;
            let digest = sha256_hex(&bytes);
            let manifest = result_manifest_hash(&digest, &artifacts)?;
            if payload.result_hash.as_deref() != Some(&manifest) {
                return Err(inconsistent(
                    "stored output bytes differ from the signed result manifest",
                ));
            }
            recovered.status = RunStatus::Succeeded;
            recovered.result_json = Some(result);
            recovered.artifacts = artifacts;
            recovered.error_message = None;
        }
        ("failed", "failed") => {
            if payload.result_ref.is_some()
                || payload.result_hash.is_some()
                || payload.failure_code.is_none()
            {
                return Err(inconsistent(
                    "failed receipt has inconsistent failure or result metadata",
                ));
            }
            recovered.status = RunStatus::Failed;
            recovered.error_message = payload.failure_message;
        }
        _ => {
            return Err(inconsistent(
                "unsupported or inconsistent signed terminal state",
            ));
        }
    }
    let finished = Utc
        .timestamp_opt(payload.finished_at, 0)
        .single()
        .ok_or_else(|| inconsistent("invalid signed completion time"))?;
    recovered.completed_at = Some(finished);
    recovered.froglet_receipt = docs.last().cloned();
    recovered.froglet_chain = docs;
    recovered.receipt_verified = true;
    Ok(Some(recovered))
}

/// Parent/child links as this workspace's own session store recorded them when it brokered the
/// runs (`W6-2`), so recovery never compares a receipt's links with themselves.
pub(super) async fn local_brokered_links(
    runtime: &InProcessNodeRuntime,
    run: &RunRecord,
) -> CompassResult<released_result::BrokeredRunLinks> {
    let parent_deal_hash = match &run.parent_run_id {
        None => None,
        Some(parent_id) => {
            let parent_id = RunId::new(parent_id.clone()).map_err(CompassError::Validation)?;
            let parent = runtime.session_store.get_run(&parent_id).await?;
            if parent.org_id != run.org_id {
                return Err(inconsistent("parent run belongs to another organization"));
            }
            Some(
                parent
                    .froglet_chain
                    .get(3)
                    .and_then(|deal| deal.get("hash"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| inconsistent("brokered parent run has no signed deal"))?
                    .to_owned(),
            )
        }
    };
    let mut child_receipt_hashes: Vec<String> = runtime
        .session_store
        .list_runs(&run.org_id)
        .await?
        .into_iter()
        .filter(|child| child.parent_run_id.as_deref() == Some(run.run_id.as_str()))
        .filter_map(|child| {
            child
                .froglet_receipt
                .as_ref()
                .and_then(|r| r.get("hash"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    child_receipt_hashes.sort();
    child_receipt_hashes.dedup();
    Ok(released_result::BrokeredRunLinks {
        parent_deal_hash,
        child_receipt_hashes,
    })
}

fn verify_native_chain(
    runtime: &InProcessNodeRuntime,
    run: &RunRecord,
    key: &str,
    docs: &[Value],
    links: &released_result::BrokeredRunLinks,
) -> CompassResult<SignedArtifact<ReceiptPayload>> {
    let kinds = [
        ARTIFACT_TYPE_DESCRIPTOR,
        ARTIFACT_TYPE_OFFER,
        ARTIFACT_TYPE_QUOTE,
        ARTIFACT_TYPE_DEAL,
        ARTIFACT_TYPE_RECEIPT,
    ];
    let report = verify_froglet_chain_offline(docs, None);
    if !report.valid
        || docs.len() != kinds.len()
        || report.artifacts.len() != kinds.len()
        || report
            .artifacts
            .iter()
            .zip(kinds)
            .any(|(doc, kind)| doc.status != DocumentStatus::Verified || doc.artifact_type != kind)
    {
        return Err(inconsistent(
            "invalid Froglet signatures, document types or links",
        ));
    }
    let quote = &docs[2];
    let deal = &docs[3];
    let provider = runtime.keys.provider_id();
    let requester = runtime.keys.requester_id();
    let signed_offer = &docs[1]["payload"]["offer_id"];
    let offer_matches = signed_offer == &format!("{}:{}", run.plugin_id, run.quote.offer_id)
        // Original native offers signed only the plugin ID, before named offers existed.
        || (run.quote.offer_id == "default" && signed_offer.as_str() == Some(run.plugin_id.as_str()));
    if run.quote.run_id != run.run_id
        || run.quote.plugin_id != run.plugin_id
        || run.quote.plugin_version != run.plugin_version
        || run.quote.target_node_id != runtime.node_id
        || run.quote.froglet_quote != *quote
        || !offer_matches
        || quote["payload"]["workload_hash"] != run.quote.workload_hash
        || quote["signer"] != provider
        || quote["payload"]["provider_id"] != provider
        || quote["payload"]["requester_id"] != requester
        || deal["signer"] != requester
        || deal["payload"]["client_nonce"].as_str().map(str::trim) != Some(key)
        || run
            .grant
            .as_ref()
            .is_some_and(|grant| grant.org_id != run.org_id)
    {
        return Err(inconsistent(
            "local identity, run, offer, approved quote or confirmation key differs",
        ));
    }
    let signed_quote: SignedArtifact<QuotePayload> = serde_json::from_value(docs[2].clone())
        .map_err(|error| inconsistent(format!("decode Froglet quote: {error}")))?;
    let signed_deal: SignedArtifact<DealPayload> = serde_json::from_value(docs[3].clone())
        .map_err(|error| inconsistent(format!("decode Froglet deal: {error}")))?;
    let receipt: SignedArtifact<ReceiptPayload> = serde_json::from_value(docs[4].clone())
        .map_err(|error| inconsistent(format!("decode Froglet receipt: {error}")))?;
    released_result::verify_signed_chain_extension_refs(
        &signed_quote,
        &signed_deal,
        &receipt,
        &run.inputs,
        run.parent_run_id.is_some(),
        links.parent_deal_hash.as_deref(),
        Some(released_result::ExpectedChildren::Brokered(
            &links.child_receipt_hashes,
        )),
    )
    .map_err(|error| inconsistent(error.to_string()))?;
    let image = OciImageRef::parse(&run.quote.image_ref, runtime.allow_local_dev_tags)?
        .canonical_module_hash();
    if receipt.signer != provider
        || receipt.payload.provider_id != provider
        || receipt.payload.requester_id != requester
        || receipt.payload.settlement_state != SETTLEMENT_METHOD_NONE
        || receipt.payload.executor.module_hash.as_deref() != Some(&image)
    {
        return Err(inconsistent(
            "receipt does not match this native executor and quoted module",
        ));
    }
    Ok(receipt)
}

fn valid_artifact_name(name: &str) -> bool {
    !name.contains(['\\', '\0']) && !name.split('/').any(|part| matches!(part, "" | "." | ".."))
}

async fn recover_legacy_artifacts(
    store: &dyn BlobStore,
    run_id: &RunId,
) -> CompassResult<Vec<ArtifactRef>> {
    let directory = format!("runs/{run_id}/artifacts");
    let prefix = format!("{directory}/");
    let mut keys = store.list_keys_under(&directory).await?;
    keys.sort();
    let mut result = Vec::with_capacity(keys.len());
    for blob_key in keys {
        let name = blob_key
            .strip_prefix(&prefix)
            .filter(|name| valid_artifact_name(name))
            .ok_or_else(|| inconsistent("invalid artifact key in historical result directory"))?
            .to_owned();
        if result
            .last()
            .is_some_and(|artifact: &ArtifactRef| artifact.name == name)
        {
            return Err(inconsistent("duplicate historical artifact name"));
        }
        let (sha256, size_bytes) = store.sha256_and_size(&blob_key).await?;
        let media_type = mime_guess::from_path(&name)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        result.push(ArtifactRef {
            name,
            blob_key,
            sha256,
            size_bytes,
            media_type,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use biocompass_core_traits::{
        LocalFsBlobStore, OrgId, SqliteSessionStore, StandardOciPolicyVerifier,
    };
    use biocompass_oci_runner::{DetectedEngine, EngineKind};
    use tempfile::tempdir;

    #[tokio::test]
    async fn original_quote_and_deal_recover_only_the_signed_bytes() -> CompassResult<()> {
        let temp = tempdir().unwrap();
        let blobs = Arc::new(LocalFsBlobStore::new(temp.path().join("blobs"))?);
        let runtime = InProcessNodeRuntime::new(NodeRuntimeDeps {
            keys: NodeKeypairSet::load_or_generate(temp.path().join("keys"))?,
            session_store: Arc::new(SqliteSessionStore::open(temp.path().join("sessions"))?),
            blob_store: blobs.clone(),
            oci_registry: Arc::new(StandardOciPolicyVerifier::strict_pinned()),
            oci_runner: Arc::new(OciContainerRunner::from_engine(DetectedEngine {
                kind: EngineKind::Podman,
                binary_path: PathBuf::new(),
                server_version: "offline".into(),
                rootless: true,
            })),
            stage_root_dir: temp.path().join("stage"),
            allow_local_dev_tags: false,
        })?;
        let plugin_dir = temp.path().join("fixture");
        std::fs::create_dir(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            format!(
                r#"
id="test/echo"
name="Echo"
version="1.0.0"
description="Return a test object"
image="registry.example/test/echo@sha256:{}"
network="none"
[inputs.schema]
type="object"
"#,
                "11".repeat(32)
            ),
        )
        .unwrap();
        std::fs::write(plugin_dir.join("SKILL.md"), "Echo").unwrap();
        let plugin = LoadedPlugin::load_from_dir(&plugin_dir, false)?;
        let quote = runtime
            .prepare_run({
                let plugin_id = plugin.manifest.id.clone();
                runtime.register_plugin(plugin).await?;
                PrepareRunRequest {
                    org_id: OrgId::from("test-org"),
                    session_id: None,
                    plugin_id,
                    plugin_version: None,
                    offer_id: None,
                    params: json!({}),
                    dataset_grant: None,
                    inputs: Vec::new(),
                    parent_run_id: None,
                    agent_credential_id: None,
                    agent_tool_call_id: None,
                    doc_id: None,
                    doc_version: None,
                }
            })
            .await?;
        let mut run = runtime.session_store.get_run(&quote.run_id).await?;
        let chain_key = format!("runs/{}/froglet_chain.json", run.run_id);
        let result_key = format!("runs/{}/result.json", run.run_id);
        assert!(load_signed_outcome(&runtime, &run, "k").await?.is_none());
        let signed_quote: SignedArtifact<QuotePayload> =
            serde_json::from_value(quote.froglet_quote.clone()).unwrap();
        let now = signed_quote.created_at;
        let deal = runtime.sign_requester(
            ARTIFACT_TYPE_DEAL,
            now,
            DealPayload {
                requester_id: runtime.keys.requester_id(),
                provider_id: runtime.keys.provider_id(),
                quote_hash: signed_quote.hash.clone(),
                workload_hash: quote.workload_hash.clone(),
                client_nonce: Some(" k ".into()),
                success_payment_hash: "00".repeat(32),
                confidential_session_hash: None,
                extension_refs: vec![],
                authority_ref: None,
                supersedes_deal_hash: None,
                admission_deadline: now + 5,
                completion_deadline: now + 50,
                acceptance_deadline: now + 60,
            },
        )?;
        run.status = RunStatus::Running;
        let bytes = b" null\n";
        let result_digest = blobs.put_bytes(&result_key, bytes).await?;
        let artifact_key = format!("runs/{}/artifacts/nested/data.csv", run.run_id);
        blobs.put_bytes(&artifact_key, b"value\n1\n").await?;
        let artifacts = recover_legacy_artifacts(blobs.as_ref(), &run.run_id).await?;
        assert_eq!(artifacts.len(), 1);
        let receipt_payload = ReceiptPayload {
            provider_id: runtime.keys.provider_id(),
            requester_id: runtime.keys.requester_id(),
            quote_hash: signed_quote.hash.clone(),
            deal_hash: deal.hash.clone(),
            started_at: Some(now),
            finished_at: now + 1,
            deal_state: "succeeded".into(),
            execution_state: "succeeded".into(),
            settlement_state: SETTLEMENT_METHOD_NONE.into(),
            result_hash: Some(result_manifest_hash(&result_digest, &artifacts)?),
            result_format: Some("application/json".into()),
            result_ref: Some(format!("blob://{result_key}")),
            confidential_session_hash: None,
            result_envelope_hash: None,
            acceptance_ref: None,
            extension_refs: vec![],
            failure_code: None,
            failure_message: None,
            limits_applied: signed_quote.payload.execution_limits.clone(),
            settlement_refs: empty_free_settlement_refs(),
            executor: ReceiptExecutor {
                runtime: "container".into(),
                runtime_version: "test".into(),
                execution_mode: Some("rootless".into()),
                attestation_platform: None,
                measurement: None,
                abi_version: Some(CONTAINER_ABI_VERSION.into()),
                module_hash: Some("11".repeat(32)),
                capabilities_granted: signed_quote.payload.capabilities_granted.clone(),
            },
        };
        let receipt = runtime.sign_provider(ARTIFACT_TYPE_RECEIPT, now + 1, receipt_payload)?;
        let mut chain = run.froglet_chain.clone();
        chain.push(serde_json::to_value(&deal).unwrap());
        chain.push(serde_json::to_value(&receipt).unwrap());
        let serialized = serde_json::to_vec(&chain).unwrap();
        blobs.put_bytes(&chain_key, &serialized).await?;
        let terminal = load_signed_outcome(&runtime, &run, "k").await?.unwrap();
        assert_eq!(terminal.status, RunStatus::Succeeded);
        assert_eq!(terminal.result_json, Some(Value::Null));
        assert_eq!(terminal.artifacts[0].blob_key, artifact_key);

        // W6-2: a receipt that signs a bc.child.v1 the local store never brokered is refused
        // (before the fix the receipt was compared with itself and this was accepted).
        let mut forged_payload = receipt.payload.clone();
        forged_payload.extension_refs = vec![format!("bc.child.v1:{}", "ab".repeat(32))];
        let forged = runtime.sign_provider(ARTIFACT_TYPE_RECEIPT, now + 1, forged_payload)?;
        let mut forged_chain = chain.clone();
        forged_chain[4] = serde_json::to_value(&forged).unwrap();
        blobs
            .put_bytes(&chain_key, &serde_json::to_vec(&forged_chain).unwrap())
            .await?;
        let err = load_signed_outcome(&runtime, &run, "k").await.unwrap_err();
        assert!(err.to_string().contains("never started"), "{err}");
        blobs.put_bytes(&chain_key, &serialized).await?;

        run.froglet_chain.push(chain[3].clone());
        assert_eq!(
            load_signed_outcome(&runtime, &run, "k")
                .await?
                .unwrap()
                .froglet_chain,
            chain
        );
        assert!(
            load_signed_outcome(&runtime, &run, "other-key")
                .await
                .is_err()
        );
        blobs.put_bytes(&artifact_key, b"value\n2\n").await?;
        assert!(load_signed_outcome(&runtime, &run, "k").await.is_err());
        blobs.put_bytes(&artifact_key, b"value\n1\n").await?;
        blobs.put_bytes(&result_key, b"null").await?;
        assert!(load_signed_outcome(&runtime, &run, "k").await.is_err());
        blobs.put_bytes(&chain_key, b"[").await?;
        assert!(load_signed_outcome(&runtime, &run, "k").await.is_err());

        // The original stage-3 signer had no named offers. Re-sign its historical
        // bare plugin ID and reconstruct the old JSON shape from before offer fields existed.
        let mut old_offer: SignedArtifact<OfferPayload> =
            serde_json::from_value(chain[1].clone()).unwrap();
        old_offer.payload.offer_id = run.plugin_id.to_string();
        let old_offer = runtime.sign_provider(ARTIFACT_TYPE_OFFER, now, old_offer.payload)?;
        let mut quote_payload = signed_quote.payload;
        quote_payload.offer_hash = old_offer.hash.clone();
        let old_quote = runtime.sign_provider(ARTIFACT_TYPE_QUOTE, now, quote_payload)?;
        let mut deal_payload = deal.payload;
        deal_payload.quote_hash = old_quote.hash.clone();
        let old_deal = runtime.sign_requester(ARTIFACT_TYPE_DEAL, now, deal_payload)?;
        let mut receipt_payload = receipt.payload;
        receipt_payload.quote_hash = old_quote.hash.clone();
        receipt_payload.deal_hash = old_deal.hash.clone();
        // Historical results with no attachments still commit to the canonical empty manifest.
        std::fs::remove_file(temp.path().join("blobs").join(&artifact_key)).unwrap();
        receipt_payload.result_hash = Some(result_manifest_hash(&sha256_hex(b"null"), &[])?);
        let old_receipt = runtime.sign_provider(ARTIFACT_TYPE_RECEIPT, now + 1, receipt_payload)?;
        let old_chain = vec![
            chain[0].clone(),
            json!(old_offer),
            json!(old_quote),
            json!(old_deal),
            json!(old_receipt),
        ];
        let mut old_record = serde_json::to_value(&run.quote).unwrap();
        let fields = old_record.as_object_mut().unwrap();
        for name in [
            "offer_id",
            "offer_name",
            "base_fee_milli_eur",
            "max_input_bytes",
            "max_output_bytes",
            "max_runtime_sec",
            "max_memory_mb",
        ] {
            fields.remove(name);
        }
        fields.insert("froglet_quote".into(), old_chain[2].clone());
        run.quote = serde_json::from_value(old_record).unwrap();
        run.froglet_chain = old_chain[..3].to_vec();
        blobs
            .put_bytes(&chain_key, &serde_json::to_vec(&old_chain).unwrap())
            .await?;
        let old = load_signed_outcome(&runtime, &run, "k").await?.unwrap();
        assert_eq!(old.quote.offer_id, "default");
        assert_eq!(old.result_json, Some(Value::Null));
        assert!(old.artifacts.is_empty());
        run.quote.offer_id = "tier-premium".into();
        assert!(load_signed_outcome(&runtime, &run, "k").await.is_err());
        Ok(())
    }
}
