//! Run the declared transformation, then validate its candidate with the original pinned importer.
use super::*;

pub(super) struct Build {
    pub _stage: tempfile::TempDir,
    pub report: Value,
    pub effects: Value,
    pub sha: String,
}

pub(super) fn file_digest(path: &Path) -> CompassResult<String> {
    append_archive::file_digest(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            conflict(&error.to_string())
        } else {
            storage(error)
        }
    })
}

fn stage_file(source: &SourceFile, destination: &Path) -> CompassResult<()> {
    // Hash the exact staged bytes; a concurrent change to a retained source cannot alter the input.
    let metadata = fs::symlink_metadata(&source.path).map_err(storage)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BYTES {
        return Err(conflict("invalid version input file"));
    }
    fs::copy(&source.path, destination).map_err(storage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o644)).map_err(storage)?;
    }
    if file_digest(destination)? != source.digest {
        return Err(conflict("staged archive differs from its pinned digest"));
    }
    Ok(())
}

async fn invoke(
    app: &AppState,
    module: &Installed,
    directory: PathBuf,
    input: SourceFile,
    secondary: Option<SourceFile>,
    params: Value,
) -> CompassResult<PathBuf> {
    let (inbound, outbound) = tokio::task::spawn_blocking(move || {
        fs::create_dir(&directory).map_err(storage)?;
        let inbound = directory.join("in");
        let outbound = directory.join("out");
        prepare_stage_permissions(&directory, &inbound, &outbound)?;
        let data = inbound.join("data");
        fs::create_dir(&data).map_err(storage)?;
        stage_file(&input, &data.join("bundle.zip"))?;
        if let Some(secondary) = secondary {
            stage_file(&secondary, &data.join("secondary.bin"))?;
        }
        let params_file = inbound.join("params.json");
        fs::write(&params_file, serde_json::to_vec(&params).map_err(storage)?).map_err(storage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(data, fs::Permissions::from_mode(0o755)).map_err(storage)?;
            fs::set_permissions(params_file, fs::Permissions::from_mode(0o644)).map_err(storage)?;
        }
        Ok::<_, CompassError>((inbound, outbound))
    })
    .await
    .map_err(storage)??;
    execute(app, module, &inbound, &outbound).await?;
    Ok(outbound)
}

fn effects(path: &Path) -> CompassResult<Value> {
    let value = read_report(path)?;
    let groups = value
        .as_object()
        .filter(|o| !o.is_empty() && o.len() <= 256)
        .ok_or_else(|| conflict("expected bounded aggregate changes"))?;
    let label = |s: &str| {
        (1..=96).contains(&s.len())
            && s.as_bytes()[0].is_ascii_alphabetic()
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    };
    for (group, entry) in groups {
        let metrics = entry
            .as_object()
            .filter(|o| (2..=17).contains(&o.len()))
            .ok_or_else(|| conflict("invalid change metrics"))?;
        if !label(group)
            || !entry["affected_sha256"].as_str().is_some_and(hex_digest)
            || metrics
                .iter()
                .any(|(name, v)| name != "affected_sha256" && (!label(name) || !v.is_u64()))
        {
            return Err(conflict(
                "change effects must contain aggregate counts and digests",
            ));
        }
    }
    Ok(value)
}

pub(super) async fn build(
    app: &AppState,
    ctx: &Context,
    module: &Installed,
    operation: &Value,
    source: Option<&Source>,
) -> CompassResult<Build> {
    let spec = module.manifest.validate_version_operation_spec(operation)?;
    if source.map(Source::kind) != spec.secondary_input {
        return Err(invalid("select the source declared by the version action"));
    }
    let params = operation.as_object().expect("validated operation");
    let mut fields = params.clone();
    fields.remove("kind");
    let params = biocompass_plugin_spec::version_operation_params(&spec.id, &fields)?;
    let base = ctx.base()?;
    let parent = SourceFile {
        path: ctx.root.join(&base.object_dir).join("validated.zip"),
        digest: base.manifest["validated_sha256"]
            .as_str()
            .ok_or_else(|| conflict("missing parent archive digest"))?
            .into(),
    };
    let secondary = source.map(|s| source_file(app, ctx, s)).transpose()?;
    let stage = tempfile::Builder::new()
        .prefix(".native-change-")
        .tempdir_in(&app.data_dir)
        .map_err(storage)?;
    let generated = invoke(
        app,
        module,
        stage.path().join("change"),
        parent,
        secondary,
        params,
    )
    .await?;
    let (sha, effects, candidate) = tokio::task::spawn_blocking(move || {
        let candidate = generated.join("bundle.zip");
        Ok::<_, CompassError>((
            file_digest(&candidate)?,
            effects(&generated.join("effects.json"))?,
            candidate,
        ))
    })
    .await
    .map_err(storage)??;
    if let Some(src @ Source::PriorRelease { .. }) = source
        && src.checksum() != sha
    {
        return Err(conflict(
            "restoration did not reproduce the selected archive",
        ));
    }
    let validator = ctx.validator(app).await?;
    let validated = invoke(
        app,
        &validator,
        stage.path().join("validate"),
        SourceFile {
            path: candidate,
            digest: sha.clone(),
        },
        None,
        json!({"operation":"import","bundle_filename":"bundle.zip"}),
    )
    .await?;
    let mut report = read_report(&validated.join("result.json"))?;
    if report["valid"] != true || report["source_sha256"] != sha {
        return Err(invalid(
            "successor failed validation against its pinned importer",
        ));
    }
    if let Some(Source::PriorRelease { version, .. }) = source {
        let prior = ctx.saved.version(Some(*version)).map_err(store_error)?;
        let selected = |report: &Value| {
            report["resources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|entry| entry["disposition"] == "attachment")
                .map(|entry| {
                    serde_json::to_string(&(
                        entry.get("path"),
                        entry.get("sha256"),
                        entry.get("bytes"),
                    ))
                    .unwrap_or_default()
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        if selected(&report) != selected(&prior.manifest["validation"]) {
            return Err(conflict(
                "restored version has a different attachment inventory",
            ));
        }
    }
    report["original_sha256"] = json!(ctx.saved.source_sha);
    report["importer"] = ctx.validator_pin()?.clone();
    Ok(Build {
        _stage: stage,
        effects,
        report,
        sha,
    })
}
