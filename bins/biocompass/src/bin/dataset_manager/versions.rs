//! Authenticated previews and atomic publication of private dataset changes.
use super::{
    App, Error, blocking, seal,
    store::{Release, State as DatasetState},
    worker,
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path as FsPath, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Append { draft_id: String },
    RemoveSample { sample_id: String },
    CorrectRow { patch: Value },
    Revert { restore_version: u64 },
}

#[derive(Deserialize)]
pub struct ChangeRequest {
    base_version: u64,
    #[serde(flatten)]
    operation: Operation,
}

#[derive(Clone, Serialize, Deserialize)]
struct Preview {
    id: String,
    base_version: u64,
    editor: String,
    operation: Operation,
    worker_image: String,
    effects: Value,
}

fn storage(e: impl std::fmt::Display) -> String {
    format!("storage: {e}")
}

fn object(root: &FsPath, release: &Release, name: &str) -> PathBuf {
    root.join(&release.object_dir).join(name)
}

fn validated_archive(root: &FsPath, release: &Release) -> Result<PathBuf, String> {
    let archive = object(root, release, "validated.zip");
    if release.manifest["validated_sha256"] != worker::archive_hash(&archive)? {
        return Err("conflict: published version archive failed integrity verification".into());
    }
    Ok(archive)
}

fn incoming(
    root: &FsPath,
    state: &DatasetState,
    op: &Operation,
) -> Result<Option<PathBuf>, String> {
    match op {
        Operation::Append { draft_id } => {
            worker::source(&root.join("append-sources"), draft_id).map(Some)
        }
        Operation::Revert { restore_version } => {
            validated_archive(root, state.version(Some(*restore_version))?).map(Some)
        }
        _ => Ok(None),
    }
}

fn render(
    engine: &str,
    root: &FsPath,
    state: &DatasetState,
    base: &Release,
    op: &Operation,
    image: &str,
    candidate: Option<&FsPath>,
) -> Result<(worker::Build, Value), String> {
    let original = object(root, state.version(Some(1))?, "source.zip");
    if worker::archive_hash(&original)? != state.source_sha {
        return Err("conflict: original upload failed integrity verification".into());
    }
    let input = incoming(root, state, op)?;
    let parent = validated_archive(root, base)?;
    let (build, effects) = worker::render_version(
        engine,
        &state.importer,
        image,
        &original,
        &parent,
        input.as_deref(),
        &serde_json::to_value(op).map_err(storage)?,
        candidate,
    )?;
    if build.validation["valid"] != true {
        return Err(format!(
            "invalid: complete dataset failed validation: {}",
            build.validation["diagnostics"]
        ));
    }
    Ok((build, effects))
}

fn save_preview(root: &FsPath, preview: &Preview) -> Result<(), String> {
    let dir = root.join("changes");
    fs::create_dir_all(&dir).map_err(storage)?;
    let mut file = tempfile::NamedTempFile::new_in(&dir).map_err(storage)?;
    serde_json::to_writer(&mut file, preview).map_err(storage)?;
    file.flush().map_err(storage)?;
    file.as_file().sync_all().map_err(storage)?;
    match file.persist_noclobber(dir.join(&preview.id)) {
        Ok(_) => fs::File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(storage),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(storage(e)),
    }
}

pub async fn preview(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<ChangeRequest>,
) -> Result<Json<Value>, Error> {
    let user = app.user(&headers).await?;
    let state = app.store.authorize_edit(&id, &user.org_id, &user.user_id)?;
    state.version(Some(req.base_version))?;
    if let Operation::RemoveSample { sample_id } = &req.operation
        && (sample_id.is_empty() || sample_id.len() > 256)
    {
        return Err(Error("invalid: provide a valid sample ID".into()));
    }
    let image = app
        .version_worker
        .clone()
        .ok_or_else(|| Error("issuer: version worker is not configured".into()))?;
    let root = app.root.join(&id);
    let permit = app
        .slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| Error("storage: worker admission closed".into()))?;
    blocking(move || {
        let _permit = permit;
        let base = state.version(Some(req.base_version))?;
        let (_, effects) = render(
            &app.engine,
            &root,
            &state,
            base,
            &req.operation,
            &image,
            None,
        )?;
        let digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&(&user.user_id, req.base_version, &req.operation, &image))
                .map_err(storage)?,
        ));
        let preview = Preview {
            id: digest,
            base_version: req.base_version,
            editor: user.user_id,
            operation: req.operation,
            worker_image: image,
            effects,
        };
        save_preview(&root, &preview)?;
        serde_json::to_value(preview).map_err(storage)
    })
    .await
    .map(Json)
}

pub async fn publish(
    State(app): State<App>,
    Path((id, change_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Release>, Error> {
    let user = app.user(&headers).await?;
    app.store
        .authorize_publish(&id, &user.org_id, &user.user_id)?;
    if change_id.len() != 64 || !change_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error("invalid: malformed change ID".into()));
    }
    let root = app.root.join(&id);
    let file = fs::File::open(root.join("changes").join(&change_id)).map_err(|e| {
        Error(if e.kind() == std::io::ErrorKind::NotFound {
            "not_found: unknown change preview".into()
        } else {
            storage(e)
        })
    })?;
    let preview: Preview = serde_json::from_reader(file).map_err(storage)?;
    if preview.id != change_id {
        return Err(Error("conflict: change preview integrity mismatch".into()));
    }
    let permit = app
        .slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| Error("storage: worker admission closed".into()))?;
    blocking(move || {
        let _permit = permit;
        app.store.publish_change(
            &id,
            &user.org_id,
            &user.user_id,
            &preview.id,
            |candidate, state, parent, version| {
                if parent.version != preview.base_version
                    && matches!(preview.operation, Operation::Revert { .. })
                {
                    return Err("conflict: head changed; review a new revert preview".into());
                }
                let (build, effects) = render(
                    &app.engine,
                    &root,
                    state,
                    parent,
                    &preview.operation,
                    &preview.worker_image,
                    Some(candidate),
                )?;
                if effects != preview.effects
                    && !matches!(preview.operation, Operation::Append { .. })
                {
                    return Err("conflict: affected rows changed; review a new preview".into());
                }
                let mut audit = json!({"id":preview.id,"base_version":preview.base_version,
                "parent_version":parent.version,"parent_digest":parent.manifest["dataset_digest"],
                "operation":preview.operation,"editor":preview.editor,"effects":effects});
                if let Operation::Append { draft_id } = &preview.operation {
                    let archive = worker::source(&root.join("append-sources"), draft_id)?;
                    let path = candidate.join("append.zip");
                    fs::copy(archive, &path).map_err(storage)?;
                    let checksum = worker::archive_hash(&path)?;
                    if format!("sha256-{checksum}") != *draft_id {
                        return Err("conflict: append archive changed".into());
                    }
                    audit["append_sha256"] = json!(checksum);
                }
                seal::seal_version(
                    candidate,
                    &build.rendered.join("tables"),
                    &format!("bc-private-v1/{id}/v{version}"),
                    &build.validation,
                    &object(&root, state.version(Some(1))?, "source.zip"),
                    json!(state.audit),
                    version,
                    Some(audit),
                )
            },
        )
    })
    .await
    .map(Json)
}
