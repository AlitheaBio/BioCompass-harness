//! Module-declared edits to a release owned by this installation.
use super::*;
use crate::dataset_store::{Release, State as DatasetState};
use biocompass_plugin_spec::VersionInputKind;
use serde::Serialize;
use std::{io::Write, path::PathBuf};

pub(super) mod append_archive;
mod render;

fn conflict(detail: &str) -> CompassError {
    CompassError::IntegrityViolation(detail.to_owned())
}
fn invalid(detail: &str) -> CompassError {
    CompassError::Validation(detail.to_owned())
}
fn missing(detail: &str) -> CompassError {
    CompassError::NotFound(detail.to_owned())
}
fn digest(value: &Value) -> CompassResult<String> {
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(value).map_err(storage)?,
    )))
}
fn hex_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn physical(dataset: &str, version: u64) -> String {
    if version == 1 {
        format!("bc-private-v1/{dataset}")
    } else {
        format!("bc-private-v1/{dataset}/v{version}")
    }
}

struct Context {
    store: LocalStore,
    root: PathBuf,
    saved: DatasetState,
    dataset: String,
    version: u64,
    actor: String,
}

impl Context {
    fn open(app: &AppState, ns: &str, name: &str) -> CompassResult<Self> {
        if ns != "bc-private-v1" {
            return Err(missing("not an installed private release"));
        }
        let (dataset, version) = if let Some((dataset, suffix)) = name.split_once('/') {
            let version = suffix
                .strip_prefix('v')
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v >= 2 && suffix == format!("v{v}"))
                .ok_or_else(|| missing("unknown private release"))?;
            (dataset, version)
        } else {
            (name, 1)
        };
        let store = LocalStore::open(&app.data_dir.join(STORE)).map_err(store_error)?;
        let root = store.dataset_directory(dataset).map_err(store_error)?;
        let actor = owner(app, &store, dataset)?;
        let saved = store
            .read_state(dataset, app.default_org_id.as_str(), &actor)
            .map_err(store_error)?;
        let release = saved.version(Some(version)).map_err(store_error)?;
        if release.manifest["catalog_id"] != physical(dataset, version) {
            return Err(conflict("release does not belong to this dataset"));
        }
        verify_publication(&local(app)?.provider_id(), dataset, release)?;
        Ok(Self {
            store,
            root,
            saved,
            dataset: dataset.into(),
            version,
            actor,
        })
    }
    fn base(&self) -> CompassResult<&Release> {
        self.saved.version(Some(self.version)).map_err(store_error)
    }
    fn head(&self) -> CompassResult<u64> {
        Ok(self.saved.version(None).map_err(store_error)?.version)
    }
    fn current(&self, version: u64) -> CompassResult<()> {
        if self.version != version || self.head()? != version {
            return Err(conflict("dataset head changed; review the current release"));
        }
        Ok(())
    }
    fn validator_pin(&self) -> CompassResult<&Value> {
        Ok(&self.saved.version(Some(1)).map_err(store_error)?.manifest["validation"]["importer"])
    }
    fn plugin_id(&self) -> CompassResult<PluginId> {
        PluginId::new(
            self.validator_pin()?["plugin_id"]
                .as_str()
                .ok_or_else(|| invalid("no installed importer on this release"))?,
        )
        .map_err(CompassError::Validation)
    }
    async fn lifecycle(
        &self,
        app: &AppState,
        previous: Option<&Value>,
    ) -> CompassResult<Installed> {
        let id = self.plugin_id()?;
        let version = match previous {
            Some(pin) => pin["version"]
                .as_str()
                .ok_or_else(|| conflict("invalid reviewed importer version"))?
                .to_owned(),
            None => local(app)?.get_plugin(&id).await?.manifest.version,
        };
        let module = installed(app, &id, &version).await?;
        if previous.is_some_and(|expected| pin(&module).as_ref().ok() != Some(expected)) {
            return Err(conflict(
                "installed lifecycle importer differs from the reviewed image",
            ));
        }
        Ok(module)
    }
    async fn validator(&self, app: &AppState) -> CompassResult<Installed> {
        self.lifecycle(app, Some(self.validator_pin()?)).await
    }
    fn committed(&self, id: &str) -> Option<&Release> {
        self.saved
            .release
            .iter()
            .chain(&self.saved.history)
            .find(|r| {
                r.manifest["version_change"]["id"] == id
                    && r.manifest["version_change"]["base_version"] == self.version
            })
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    ValidatedUpload {
        upload_record_id: String,
        validated_sha256: String,
    },
    PriorRelease {
        version: u64,
        validated_sha256: String,
    },
}
impl Source {
    fn kind(&self) -> VersionInputKind {
        match self {
            Self::ValidatedUpload { .. } => VersionInputKind::ValidatedUpload,
            Self::PriorRelease { .. } => VersionInputKind::PriorRelease,
        }
    }
    fn checksum(&self) -> &str {
        match self {
            Self::ValidatedUpload {
                validated_sha256, ..
            }
            | Self::PriorRelease {
                validated_sha256, ..
            } => validated_sha256,
        }
    }
}

struct SourceFile {
    path: PathBuf,
    digest: String,
}

fn persist(path: &Path, bytes: &[u8]) -> CompassResult<()> {
    let dir = path
        .parent()
        .ok_or_else(|| invalid("invalid local object path"))?;
    fs::create_dir_all(dir).map_err(storage)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(storage)?;
    tmp.write_all(bytes).map_err(storage)?;
    tmp.as_file().sync_all().map_err(storage)?;
    match tmp.persist_noclobber(path) {
        Ok(_) => fs::File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(storage),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(storage(e)),
    }
}

fn records(root: &Path) -> CompassResult<Vec<PathBuf>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(storage(e)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(storage)?;
        if entry.file_type().map_err(storage)?.is_file()
            && entry.path().extension().is_some_and(|s| s == "json")
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

pub(super) async fn authorize_append(
    app: &AppState,
    store: &LocalStore,
    dataset: &str,
    module: &Installed,
) -> CompassResult<()> {
    let ctx = Context::open(app, "bc-private-v1", dataset)?;
    store
        .authorize_edit(dataset, &ctx.saved.org, &ctx.actor)
        .map_err(store_error)?;
    if pin(module)? != *ctx.validator_pin()? {
        return Err(conflict("append requires the original validation module"));
    }
    if !ctx
        .lifecycle(app, None)
        .await?
        .manifest
        .version_operation_specs()?
        .iter()
        .any(|s| s.secondary_input == Some(VersionInputKind::ValidatedUpload))
    {
        return Err(invalid(
            "the installed importer declares no action for a validated upload",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn save_append(
    app: &AppState,
    store: &LocalStore,
    dataset: &str,
    sha: &str,
    module: &Value,
    upload: &Path,
    rendered: &Path,
    filename: &str,
) -> CompassResult<Value> {
    let actor = owner(app, store, dataset)?;
    store
        .authorize_edit(dataset, app.default_org_id.as_str(), &actor)
        .map_err(store_error)?;
    let dir = store
        .dataset_directory(dataset)
        .map_err(store_error)?
        .join("native-append");
    append_archive::retain_validated(upload, rendered, &dir, sha).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            conflict(&error.to_string())
        } else {
            storage(error)
        }
    })?;
    let source = Source::ValidatedUpload {
        upload_record_id: format!("sha256-{sha}"),
        validated_sha256: sha.into(),
    };
    let detail = json!({"domain":"biocompass.local-append.v1","org":app.default_org_id,"dataset":dataset,"source":source,
                      "filename":filename,"validator":module});
    let signature = sign_release_change(app, detail.clone())?;
    persist(
        &dir.join(format!("{sha}.json")),
        &serde_json::to_vec(&json!({"detail":detail,"signature":signature})).map_err(storage)?,
    )?;
    Ok(json!(source))
}

fn append_record(app: &AppState, ctx: &Context, sha: &str) -> CompassResult<Value> {
    if !hex_digest(sha) {
        return Err(invalid("invalid append digest"));
    }
    let detail = read_record(&ctx.root.join("native-append").join(format!("{sha}.json")))?;
    verify_release_change(app, &detail["detail"], &detail["signature"])?;
    let value = &detail["detail"];
    let expected = Source::ValidatedUpload {
        upload_record_id: format!("sha256-{sha}"),
        validated_sha256: sha.into(),
    };
    if value["domain"] != "biocompass.local-append.v1"
        || value["org"] != ctx.saved.org
        || value["dataset"] != ctx.dataset
        || value["source"] != json!(expected)
        || &value["validator"] != ctx.validator_pin()?
    {
        return Err(conflict(
            "append input belongs to another release authority",
        ));
    }
    Ok(value.clone())
}
fn read_record(path: &Path) -> CompassResult<Value> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= 4 * 1024 * 1024 => (),
        Ok(_) => return Err(conflict("invalid local review record")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(missing("local review input is unavailable"));
        }
        Err(e) => return Err(storage(e)),
    }
    serde_json::from_reader(fs::File::open(path).map_err(storage)?).map_err(storage)
}

fn source_file(app: &AppState, ctx: &Context, src: &Source) -> CompassResult<SourceFile> {
    let sha = src.checksum();
    if !hex_digest(sha) {
        return Err(invalid("invalid selected archive digest"));
    }
    let path = match src {
        Source::ValidatedUpload {
            upload_record_id, ..
        } => {
            if upload_record_id != &format!("sha256-{sha}") {
                return Err(invalid("append upload ID and digest disagree"));
            }
            append_record(app, ctx, sha)?;
            ctx.root.join("native-append").join(sha)
        }
        Source::PriorRelease { version, .. } => {
            if *version >= ctx.version {
                return Err(invalid("select an earlier release of this dataset"));
            }
            let historical = ctx.saved.version(Some(*version)).map_err(store_error)?;
            verify_publication(&local(app)?.provider_id(), &ctx.dataset, historical)?;
            if historical.manifest["validated_sha256"] != sha {
                return Err(conflict("selected release archive changed"));
            }
            ctx.root.join(&historical.object_dir).join("validated.zip")
        }
    };
    Ok(SourceFile {
        path,
        digest: sha.into(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestBody {
    base_version: u64,
    operation: Value,
    source: Option<Source>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    domain: String,
    org: String,
    dataset: String,
    editor: String,
    base_version: u64,
    parent_digest: String,
    lifecycle: Value,
    operation: Value,
    source_pin: Option<Source>,
    effects: Value,
    validated_sha256: String,
}
impl Review {
    fn visible(&self, id: &str, published: Option<u64>) -> Value {
        json!({"change_id":id,"base_version":self.base_version,"operation":self.operation,"source_pin":self.source_pin,
               "effects":self.effects,"validated_sha256":self.validated_sha256,"published_version":published})
    }
    fn load(app: &AppState, ctx: &Context, id: &str) -> CompassResult<Self> {
        if !hex_digest(id) {
            return Err(invalid("invalid local preview ID"));
        }
        let value = read_record(&ctx.root.join("native-reviews").join(format!("{id}.json")))?;
        let payload = &value["review"];
        if digest(payload)? != id {
            return Err(conflict("preview digest differs from its ID"));
        }
        verify_release_change(app, payload, &value["signature"])?;
        let review: Self = serde_json::from_value(payload.clone()).map_err(storage)?;
        if review.domain != "biocompass.local-preview.v1"
            || review.org != ctx.saved.org
            || review.dataset != ctx.dataset
            || ctx
                .saved
                .version(Some(review.base_version))
                .map_err(store_error)?
                .manifest["dataset_digest"]
                != review.parent_digest
        {
            return Err(conflict("preview belongs to another release"));
        }
        Ok(review)
    }
}

pub(crate) async fn list(app: &AppState, ns: &str, name: &str) -> CompassResult<Value> {
    let ctx = Context::open(app, ns, name)?;
    let id = physical(&ctx.dataset, ctx.version);
    let head = ctx.head()?;
    let specs = match ctx.lifecycle(app, None).await {
        Ok(module) => module.manifest.version_operation_specs()?,
        Err(
            CompassError::NotFound(_) | CompassError::AccessDenied(_) | CompassError::Validation(_),
        ) => vec![],
        Err(e) => return Err(e),
    };
    let may_edit = head == ctx.version && ctx.saved.editors.contains(&ctx.actor);
    let may_publish = head == ctx.version && ctx.saved.publishers.contains(&ctx.actor);
    let operations: Vec<_> = specs.iter().map(|spec| &spec.id).collect();
    let mut choices = vec![];
    let mut append = None;
    if may_edit
        && specs
            .iter()
            .any(|s| s.secondary_input == Some(VersionInputKind::ValidatedUpload))
    {
        let module = ctx.validator(app).await?;
        append = Some(
            json!({"plugin_id":module.manifest.id,"version":module.manifest.version,
                           "dataset_name":ctx.dataset,"accept":module.contract.accept}),
        );
        for path in records(&ctx.root.join("native-append"))? {
            let sha = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| conflict("invalid staged upload name"))?;
            let record = append_record(app, &ctx, sha)?;
            choices.push(json!({"label":record["filename"],"source":record["source"]}));
        }
    }
    if may_edit
        && specs
            .iter()
            .any(|s| s.secondary_input == Some(VersionInputKind::PriorRelease))
    {
        for r in ctx.saved.history.iter().filter(|v| v.version < ctx.version) {
            choices.push(json!({"label":format!("Version {}",r.version),
                "source":{"kind":"prior_release","version":r.version,"validated_sha256":r.manifest["validated_sha256"]}}));
        }
    }
    let mut previews = Vec::new();
    for path in records(&ctx.root.join("native-reviews"))? {
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| conflict("invalid stored preview name"))?;
        let review = Review::load(app, &ctx, id)?;
        if review.base_version == ctx.version {
            previews.push(review.visible(id, ctx.committed(id).map(|r| r.version)));
        }
    }
    Ok(
        json!({"catalog_id":id,"base_version":ctx.version,"head_version":head,"may_edit":may_edit,
            "may_publish":may_publish,"operation_specs":specs,"operations":operations,"source_choices":choices,
            "native_append":append,"previews":previews}),
    )
}

pub(crate) async fn preview(
    app: &AppState,
    ns: &str,
    name: &str,
    body: Value,
) -> CompassResult<Value> {
    let ctx = Context::open(app, ns, name)?;
    ctx.store
        .authorize_edit(&ctx.dataset, &ctx.saved.org, &ctx.actor)
        .map_err(store_error)?;
    let req: RequestBody =
        serde_json::from_value(body).map_err(|_| invalid("invalid version preview request"))?;
    ctx.current(req.base_version)?;
    let module = ctx.lifecycle(app, None).await?;
    let lifecycle = pin(&module)?;
    let build = render::build(app, &ctx, &module, &req.operation, req.source.as_ref()).await?;
    let review = Review {
        domain: "biocompass.local-preview.v1".into(),
        dataset: ctx.dataset.clone(),
        org: ctx.saved.org.clone(),
        editor: ctx.actor.clone(),
        base_version: ctx.version,
        parent_digest: ctx.base()?.manifest["dataset_digest"]
            .as_str()
            .ok_or_else(|| conflict("missing parent digest"))?
            .into(),
        lifecycle,
        operation: req.operation,
        source_pin: req.source,
        effects: build.effects,
        validated_sha256: build.sha,
    };
    let payload = serde_json::to_value(&review).map_err(storage)?;
    let id = digest(&payload)?;
    let signature = sign_release_change(app, payload.clone())?;
    persist(
        &ctx.root.join("native-reviews").join(format!("{id}.json")),
        &serde_json::to_vec(&json!({"review":payload,"signature":signature})).map_err(storage)?,
    )?;
    // Reread the original signed record even when the same concurrent request won its creation.
    let saved = Review::load(app, &ctx, &id)?;
    Ok(json!({"preview":saved.visible(&id,None)}))
}

fn publication_payload(dataset: &str, version: u64, unsigned: &Value) -> CompassResult<Value> {
    let audit = &unsigned["version_change"];
    Ok(
        json!({"domain":"biocompass.local-publication.v1","dataset":dataset,"org":audit["org"],
        "version":version,"catalog_id":unsigned["catalog_id"],"dataset_digest":unsigned["dataset_digest"],
        "validated_sha256":unsigned["validated_sha256"],"parent_version":audit["parent_version"],
        "parent_digest":audit["parent_digest"],"operation":audit["operation"],"change_id":audit["id"],
        "release_sha256":digest(unsigned)?}),
    )
}

pub(crate) fn verify_publication(
    signer: &str,
    dataset: &str,
    release: &Release,
) -> CompassResult<()> {
    if release.version == 1 {
        return Ok(());
    }
    let mut unsigned = release.manifest.clone();
    let proof = unsigned
        .as_object_mut()
        .and_then(|obj| obj.remove("node_publication"))
        .ok_or_else(|| conflict("missing node publication signature"))?;
    if unsigned["catalog_id"] != physical(dataset, release.version)
        || unsigned["version_change"]["parent_version"]
            .as_u64()
            .and_then(|v| v.checked_add(1))
            != Some(release.version)
    {
        return Err(conflict("invalid native publication lineage"));
    }
    InProcessNodeRuntime::verify_dataset_action(
        signer,
        &publication_payload(dataset, release.version, &unsigned)?,
        &proof,
    )
}

async fn published(app: &AppState, ctx: &Context, release: &Release) -> CompassResult<Value> {
    let runtime = local(app)?;
    verify_publication(&runtime.provider_id(), &ctx.dataset, release)?;
    let dir = ctx.root.join(&release.object_dir);
    let manifest = release.manifest.clone();
    let engine = tokio::task::spawn_blocking(move || {
        if private_release::verify(&dir).map_err(store_error)? != manifest {
            return Err(conflict(
                "private version differs from its committed manifest",
            ));
        }
        DuckDbCatalogEngine::open_hospital_readonly(dir.join("ducklake"))
    })
    .await
    .map_err(storage)??;
    runtime.register_immutable_catalog(engine).await?;
    Ok(
        json!({"version":release.version,"catalog":{"manifest":{"id":release.manifest["catalog_id"],
        "version":release.version,"dataset_digest":release.manifest["dataset_digest"]}},
        "release":{"catalog_id":release.manifest["catalog_id"],"snapshot_id":release.manifest["snapshot_id"],
        "dataset_digest":release.manifest["dataset_digest"],"validated_sha256":release.manifest["validated_sha256"],
        "node_publication":release.manifest["node_publication"]}}),
    )
}

pub(crate) async fn publish(
    app: &AppState,
    ns: &str,
    name: &str,
    id: &str,
) -> CompassResult<Value> {
    let ctx = Context::open(app, ns, name)?;
    ctx.store
        .authorize_publish(&ctx.dataset, &ctx.saved.org, &ctx.actor)
        .map_err(store_error)?;
    if !hex_digest(id) {
        return Err(invalid("invalid local preview ID"));
    }
    if let Some(release) = ctx.committed(id) {
        return published(app, &ctx, release).await;
    }
    let review = Review::load(app, &ctx, id)?;
    ctx.current(review.base_version)?;
    let module = ctx.lifecycle(app, Some(&review.lifecycle)).await?;
    let build = render::build(
        app,
        &ctx,
        &module,
        &review.operation,
        review.source_pin.as_ref(),
    )
    .await?;
    if build.sha != review.validated_sha256 || build.effects != review.effects {
        return Err(conflict(
            "successor differs from reviewed archive or effects",
        ));
    }
    let app_for_commit = app.clone();
    let dataset = ctx.dataset.clone();
    let org = ctx.saved.org.clone();
    let actor = ctx.actor.clone();
    let store = ctx.store.clone();
    let root = ctx.root.clone();
    let id = id.to_owned();
    let release=tokio::task::spawn_blocking(move || {
        store.publish_change(&dataset,&org,&actor,&id,|candidate,state,parent,version|{
            if parent.version!=review.base_version || parent.manifest["dataset_digest"]!=review.parent_digest {
                return Err("conflict: dataset head changed; review the current release".into());
            }
            let genesis=state.version(Some(1))?;
            let source=root.join(&genesis.object_dir).join("source.zip");
            let audit=json!({"id":id,"org":org,"editor":review.editor,"publisher":actor,"base_version":review.base_version,
                     "parent_version":parent.version,"parent_digest":review.parent_digest,"operation":review.operation,
                     "lifecycle":review.lifecycle,"effects":review.effects,"source_pin":review.source_pin});
            let mut manifest=private_release::seal_version(candidate,&build._stage.path().join("validate/out/rendered/tables"),
                &physical(&dataset,version),&build.report,&source,json!(state.audit),version,Some(audit),&||{})?;
            let payload=publication_payload(&dataset,version,&manifest).map_err(|e|e.to_string())?;
            manifest["node_publication"]=sign_release_change(&app_for_commit,payload).map_err(|e|e.to_string())?;
            let mut file=fs::File::create(candidate.join("release.json")).map_err(|e|e.to_string())?;
            serde_json::to_writer_pretty(&mut file,&manifest).map_err(|e|e.to_string())?;
            file.write_all(b"\n").map_err(|e|e.to_string())?;
            Ok(manifest)
        }).map_err(store_error)
    }).await.map_err(storage)??;
    published(app, &ctx, &release).await
}
