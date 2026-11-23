//! Download and install an authorized, exact marketplace release into a personal workspace.
pub(crate) mod removal;
use crate::state::AppState;
use biocompass_core_traits::{CompassError, CompassResult, OrgId, PluginId};
use biocompass_node_runtime::InProcessNodeRuntime;
use biocompass_plugin_spec::{LoadedPlugin, PluginExecutionPlane, PluginManifest, SkillDoc};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

type Inventory = BTreeMap<String, (String, u64)>;

#[derive(Deserialize)]
struct PackageFile {
    path: String,
    size_bytes: u64,
    sha256: String,
    data_hex: String,
}

#[derive(Deserialize)]
struct Package {
    plugin_id: PluginId,
    plugin_version: String,
    manifest: PluginManifest,
    skill: Option<SkillDoc>,
    image_digest: String,
    ui_sha256: Option<String>,
    ui_files: Vec<PackageFile>,
    #[serde(default)]
    icon_file: Option<PackageFile>,
}

struct StagedAssets {
    ui_files: BTreeMap<String, Vec<u8>>,
    icon: Option<Vec<u8>>,
}

// Keep the published JSON representation. Converting a manifest through TOML would lose
// JSON-schema values (e.g. null); a normalized SkillDoc cannot recover original Markdown bytes.
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    format: u8,
    origin: String,
    acquired_by_org: OrgId,
    installed_at: chrono::DateTime<chrono::Utc>,
    manifest: PluginManifest,
    skill: Option<SkillDoc>,
    image_digest: String,
    ui_sha256: Option<String>,
    files: Inventory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon_sha256: Option<(String, u64)>,
}

fn io(e: std::io::Error) -> CompassError {
    CompassError::Storage(format!("native module installation: {e}"))
}
fn invalid(reason: impl Into<String>) -> CompassError {
    CompassError::IntegrityViolation(reason.into())
}
fn json_error(e: serde_json::Error) -> CompassError {
    invalid(format!("invalid module package metadata: {e}"))
}
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn path_ok(s: &str) -> bool {
    !s.is_empty()
        && s.trim() == s
        && !s.contains('\\')
        && !s.chars().any(char::is_control)
        && !s.split('/').any(|part| matches!(part, "" | "." | ".."))
        && Path::new(s)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn stored_dir(root: &Path, id: &PluginId, version: &str) -> CompassResult<PathBuf> {
    Ok(root.join(sha(&serde_json::to_vec(&(id, version)).map_err(json_error)?)))
}

impl Receipt {
    fn check(&self) -> CompassResult<()> {
        let image = self.manifest.validate(false)?;
        if self.format != 1
            || self.manifest.execution == PluginExecutionPlane::CloudOnly
            || image.sha256_digest.as_deref() != self.image_digest.strip_prefix("sha256:")
            || !self.image_digest.starts_with("sha256:")
        {
            return Err(invalid(
                "invalid native release execution policy or OCI digest",
            ));
        }
        if self
            .manifest
            .icon
            .as_deref()
            .is_some_and(|icon| !path_ok(icon) || !icon.ends_with(".svg"))
        {
            return Err(invalid("invalid module icon path"));
        }
        if let Some((icon_hash, icon_size)) = &self.icon_sha256 {
            if self.manifest.icon.is_none()
                || *icon_size == 0
                || *icon_size > 256 * 1024
                || icon_hash.len() != 64
                || !icon_hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(invalid("invalid module icon metadata"));
            }
        }
        if self.files.keys().any(|p| !path_ok(p)) {
            return Err(invalid("invalid relative UI asset path"));
        }
        if self.files.keys().any(|p| {
            p.match_indices('/')
                .any(|(i, _)| self.files.contains_key(&p[..i]))
        }) {
            return Err(invalid("a module UI file conflicts with a directory"));
        }
        let Some(ui) = &self.manifest.ui else {
            if self.files.is_empty() && self.ui_sha256.is_none() {
                return Ok(());
            }
            return Err(invalid("unexpected UI files in module package"));
        };
        if !path_ok(&ui.entry)
            || !self
                .files
                .contains_key(ui.entry.rsplit('/').next().unwrap_or(""))
        {
            return Err(invalid(
                "module package does not supply its declared UI entry",
            ));
        }
        if self.ui_sha256.as_ref()
            != Some(&sha(&serde_json::to_vec(&self.files).map_err(json_error)?))
        {
            return Err(invalid("module UI inventory checksum mismatch"));
        }
        Ok(())
    }

    fn ui_root(&self, directory: &Path) -> PathBuf {
        let root = directory.join("content");
        self.manifest
            .ui
            .as_ref()
            .and_then(|ui| Path::new(&ui.entry).parent())
            .map_or_else(|| root.clone(), |rel| root.join(rel))
    }

    fn plugin(&self, directory: &Path) -> LoadedPlugin {
        LoadedPlugin {
            manifest: self.manifest.clone(),
            skill: self.skill.clone(),
            root_dir: Some(directory.join("content")),
        }
    }

    fn same_publication(&self, other: &Self) -> CompassResult<bool> {
        Ok(self.origin == other.origin
            && self.image_digest == other.image_digest
            && self.ui_sha256 == other.ui_sha256
            && self.files == other.files
            && self.icon_sha256 == other.icon_sha256
            && serde_json::to_value((&self.manifest, &self.skill)).map_err(json_error)?
                == serde_json::to_value((&other.manifest, &other.skill)).map_err(json_error)?)
    }
}

fn validate_payload(
    package: Package,
    id: &PluginId,
    version: &str,
    origin: String,
    org: OrgId,
) -> CompassResult<(Receipt, StagedAssets)> {
    if &package.plugin_id != id
        || &package.manifest.id != id
        || package.plugin_version != version
        || package.manifest.version != version
    {
        return Err(invalid("hub returned a different module ID or version"));
    }
    let mut files = Inventory::new();
    let mut ui_files = BTreeMap::new();
    for file in package.ui_files {
        if !path_ok(&file.path) || files.contains_key(&file.path) {
            return Err(invalid("duplicate or invalid UI path in module package"));
        }
        let data = hex::decode(file.data_hex).map_err(|_| invalid("invalid UI file encoding"))?;
        if data.len() as u64 != file.size_bytes || sha(&data) != file.sha256 {
            return Err(invalid(format!(
                "module asset failed integrity check: {}",
                file.path
            )));
        }
        files.insert(file.path.clone(), (file.sha256, file.size_bytes));
        ui_files.insert(file.path, data);
    }
    let (icon_sha256, icon) = if let Some(icon_file) = package.icon_file {
        if package.manifest.icon.as_deref() != Some(icon_file.path.as_str())
            || !path_ok(&icon_file.path)
            || !icon_file.path.ends_with(".svg")
        {
            return Err(invalid("module icon_file does not match manifest.icon"));
        }
        let data =
            hex::decode(icon_file.data_hex).map_err(|_| invalid("invalid icon file encoding"))?;
        if data.is_empty()
            || data.len() > 256 * 1024
            || data.len() as u64 != icon_file.size_bytes
            || sha(&data) != icon_file.sha256
        {
            return Err(invalid("module icon failed integrity check"));
        }
        (Some((icon_file.sha256, icon_file.size_bytes)), Some(data))
    } else {
        (None, None)
    };
    let receipt = Receipt {
        format: 1,
        origin,
        acquired_by_org: org,
        installed_at: chrono::Utc::now(),
        manifest: package.manifest,
        skill: package.skill,
        image_digest: package.image_digest,
        ui_sha256: package.ui_sha256,
        files,
        icon_sha256,
    };
    receipt.check()?;
    Ok((receipt, StagedAssets { ui_files, icon }))
}

// Inventory every file, including unexpected additions. Never follow directory or file symlinks.
fn inventory(root: &Path, path: &Path, found: &mut Inventory) -> CompassResult<()> {
    for entry in fs::read_dir(path).map_err(io)? {
        let entry = entry.map_err(io)?;
        let ty = entry.file_type().map_err(io)?;
        let file = entry.path();
        if ty.is_dir() {
            inventory(root, &file, found)?;
        } else if ty.is_file() {
            let relative = file
                .strip_prefix(root)
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| invalid("invalid installed module filename"))?;
            let mut input = File::open(&file).map_err(io)?;
            let mut digest = Sha256::new();
            let size = std::io::copy(&mut input, &mut digest).map_err(io)?;
            found.insert(relative.to_owned(), (hex::encode(digest.finalize()), size));
        } else {
            return Err(invalid("invalid file type in installed module"));
        }
    }
    Ok(())
}

fn load(directory: &Path) -> CompassResult<Receipt> {
    let receipt: Receipt =
        serde_json::from_slice(&fs::read(directory.join("release.json")).map_err(io)?)
            .map_err(json_error)?;
    receipt.check()?;
    let content = directory.join("content");
    let mut actual = Inventory::new();
    if !fs::symlink_metadata(&content).map_err(io)?.is_dir() {
        return Err(invalid("invalid installed module content directory"));
    }
    inventory(&content, &content, &mut actual)?;
    let mut expected = receipt
        .files
        .iter()
        .map(|(k, v)| {
            let path = receipt.ui_root(directory).join(k);
            (
                path.strip_prefix(&content)
                    .expect("validated relative UI path")
                    .to_string_lossy()
                    .into_owned(),
                v.clone(),
            )
        })
        .collect::<Inventory>();
    if let (Some(icon_rel), Some(icon_entry)) = (&receipt.manifest.icon, &receipt.icon_sha256)
        && expected.insert(icon_rel.clone(), icon_entry.clone()).is_some()
    {
        return Err(invalid("module icon path conflicts with a UI asset"));
    }
    if actual != expected {
        return Err(invalid(
            "installed module files differ from their published inventory",
        ));
    }
    Ok(receipt)
}

fn write_new(path: &Path, data: &[u8]) -> CompassResult<()> {
    let mut f = File::create_new(path).map_err(io)?;
    f.write_all(data).map_err(io)?;
    f.sync_all().map_err(io)
}

fn sync_dirs(path: &Path) -> CompassResult<()> {
    for entry in fs::read_dir(path).map_err(io)? {
        let entry = entry.map_err(io)?;
        if entry.file_type().map_err(io)?.is_dir() {
            sync_dirs(&entry.path())?;
        }
    }
    File::open(path).and_then(|f| f.sync_all()).map_err(io)
}

// Serialize short disk commits across processes; network and image acquisition happen outside the lock.
fn lock(root: &Path) -> CompassResult<File> {
    fs::create_dir_all(root).map_err(io)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".install.lock"))
        .map_err(io)?;
    file.lock().map_err(io)?;
    Ok(file)
}

fn verify_existing(directory: &Path, receipt: &Receipt) -> CompassResult<bool> {
    if directory.try_exists().map_err(io)? {
        let existing = load(directory)?;
        if !receipt.same_publication(&existing)? {
            return Err(invalid(format!(
                "{}@{} is already installed from {}; incoming release from {} differs",
                receipt.manifest.id, receipt.manifest.version, existing.origin, receipt.origin
            )));
        }
        return Ok(true);
    }
    Ok(false)
}

// Called under the install lock for a new release only. Use the prior committed maximum
// to keep restoration order deterministic even if the wall clock is adjusted or timestamps tie.
fn next_commit_time(root: &Path) -> CompassResult<chrono::DateTime<chrono::Utc>> {
    #[derive(Deserialize)]
    struct InstalledAt {
        installed_at: chrono::DateTime<chrono::Utc>,
    }
    let mut stamp = chrono::Utc::now();
    for entry in fs::read_dir(root).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() != 64
            || !name.bytes().all(|b| b.is_ascii_hexdigit())
            || !entry.file_type().map_err(io)?.is_dir()
        {
            continue;
        }
        let metadata = File::open(entry.path().join("release.json")).map_err(io)?;
        let last: InstalledAt = serde_json::from_reader(metadata).map_err(json_error)?;
        if last.installed_at >= stamp {
            stamp = last
                .installed_at
                .checked_add_signed(chrono::Duration::nanoseconds(1))
                .ok_or_else(|| invalid("installed release timestamp cannot be advanced"))?;
        }
    }
    Ok(stamp)
}

fn commit(
    root: &Path,
    directory: &Path,
    receipt: &Receipt,
    bytes: &StagedAssets,
) -> CompassResult<()> {
    let _lock = lock(root)?;
    if verify_existing(directory, receipt)? {
        return Ok(());
    }
    let stage = tempfile::Builder::new()
        .prefix(".pending-")
        .tempdir_in(root)
        .map_err(io)?;
    fs::create_dir(stage.path().join("content")).map_err(io)?;
    let ui_root = receipt.ui_root(stage.path());
    for (name, data) in &bytes.ui_files {
        let path = ui_root.join(name);
        fs::create_dir_all(path.parent().expect("file within staging directory")).map_err(io)?;
        write_new(&path, data)?;
    }
    if let (Some(icon_rel), Some(icon_data)) = (&receipt.manifest.icon, &bytes.icon) {
        let icon_path = stage.path().join("content").join(icon_rel);
        fs::create_dir_all(icon_path.parent().expect("icon within staging directory"))
            .map_err(io)?;
        write_new(&icon_path, icon_data)?;
    }
    let mut committed = receipt.clone();
    committed.installed_at = next_commit_time(root)?;
    write_new(
        &stage.path().join("release.json"),
        &serde_json::to_vec(&committed).map_err(json_error)?,
    )?;
    load(stage.path())?;
    sync_dirs(stage.path())?;
    fs::rename(stage.path(), directory).map_err(io)?;
    File::open(root).and_then(|f| f.sync_all()).map_err(io)?;
    Ok(())
}

fn check_source(
    previous: Option<&LoadedPlugin>,
    directory: &Path,
    incoming: &Receipt,
) -> CompassResult<()> {
    if let Some(old) = previous
        && old.root_dir.as_deref() != Some(directory.join("content").as_path())
    {
        return Err(invalid(format!(
            "{}@{} is already registered from {}; refusing installation from {}",
            incoming.manifest.id,
            incoming.manifest.version,
            old.root_dir.as_ref().map_or_else(
                || "another registry".to_owned(),
                |p| p.display().to_string()
            ),
            incoming.origin
        )));
    }
    Ok(())
}

pub(crate) async fn install(
    state: &AppState,
    id: &PluginId,
    version: &str,
) -> CompassResult<LoadedPlugin> {
    install_checked(state, id, version, None).await
}

#[derive(Deserialize)]
struct DownloadList {
    plugins: Vec<Download>,
}

#[derive(Deserialize)]
struct Download {
    plugin_id: PluginId,
    version: String,
    image_digest: String,
}

/// The hub orders eligible releases by publication time, newest first within each ID.
/// Binding its first matching descriptor also checks that the package serves the advertised digest.
pub(crate) async fn install_latest(state: &AppState, id: &PluginId) -> CompassResult<LoadedPlugin> {
    let workspace = state.installed_workspace.as_ref().ok_or_else(|| {
        CompassError::AccessDenied(
            "a paired native workspace is required to discover downloads".into(),
        )
    })?;
    let releases = workspace
        .send(workspace.request(Method::GET, &["plugins", "downloads"]))
        .await?
        .json::<DownloadList>()
        .await
        .map_err(|e| invalid(format!("invalid marketplace release list: {e}")))?;
    let release = releases
        .plugins
        .into_iter()
        .find(|r| &r.plugin_id == id)
        .ok_or_else(|| {
            CompassError::NotFound(format!("module '{id}' has no available download"))
        })?;
    biocompass_core_traits::validate_requested_plugin_version(Some(&release.version))?;
    install_checked(state, id, &release.version, Some(&release.image_digest)).await
}

async fn install_checked(
    state: &AppState,
    id: &PluginId,
    version: &str,
    expected_digest: Option<&str>,
) -> CompassResult<LoadedPlugin> {
    let workspace = state.installed_workspace.as_ref().ok_or_else(|| {
        CompassError::AccessDenied(
            "a paired, loopback native workspace is required to install a published module".into(),
        )
    })?;
    let runtime = state
        .local_runtime
        .as_ref()
        .ok_or_else(|| CompassError::AccessDenied("local runtime required".into()))?;
    let runner = state
        .oci_runner
        .as_ref()
        .ok_or_else(|| CompassError::AccessDenied("native container engine required".into()))?;
    let (ns, slug) = id
        .as_str()
        .split_once('/')
        .ok_or_else(|| invalid("module ID requires a namespace"))?;
    let scope = workspace.scope().await?;
    // Hub accepts 32 MiB of UI files. The JSON transport hex-encodes them and also carries the manifest.
    let package = workspace
        .bounded_bytes(
            &["plugins", ns, slug, "versions", version, "package"],
            72 * 1024 * 1024,
        )
        .await?;
    let (receipt, bytes) = validate_payload(
        serde_json::from_slice(&package).map_err(json_error)?,
        id,
        version,
        scope.origin,
        scope.org_id,
    )?;
    if expected_digest.is_some_and(|digest| digest != receipt.image_digest) {
        return Err(invalid(
            "module package digest differs from marketplace discovery",
        ));
    }
    let root = state.data_dir.join("module-releases");
    let directory = stored_dir(&root, id, version)?;
    let previous = match runtime.get_plugin_version(id, Some(version)).await {
        Ok(p) => Some(p),
        Err(CompassError::NotFound(_)) => None,
        Err(e) => return Err(e),
    };
    check_source(previous.as_ref(), &directory, &receipt)?;
    verify_existing(&directory, &receipt)?;
    if !runner
        .has_verified_local_digest(&receipt.manifest.image)
        .await
    {
        use tokio::io::AsyncWriteExt;
        let mut image = workspace
            .send(
                workspace
                    .request(
                        Method::GET,
                        &["plugins", ns, slug, "versions", version, "image"],
                    )
                    .timeout(std::time::Duration::from_secs(3600)),
            )
            .await?;
        let archive = tempfile::NamedTempFile::new().map_err(io)?;
        let mut file = tokio::fs::File::from_std(archive.reopen().map_err(io)?);
        while let Some(chunk) = image.chunk().await.map_err(|e| {
            CompassError::Storage(format!("interrupted OCI download: {}", e.without_url()))
        })? {
            file.write_all(&chunk).await.map_err(io)?;
        }
        file.flush().await.map_err(io)?;
        drop(file);
        runner
            .import_release_image(&receipt.manifest.image, archive.path())
            .await?;
    }
    let plugin = receipt.plugin(&directory);
    runtime
        .register_installed_plugin(plugin.clone(), |prev| {
            check_source(prev, &directory, &receipt)?;
            commit(&root, &directory, &receipt, &bytes)
        })
        .await?;
    Ok(plugin)
}

pub(crate) async fn restore(data_dir: &Path, runtime: &InProcessNodeRuntime) -> CompassResult<()> {
    let root = data_dir.join("module-releases");
    if !root.exists() {
        return Ok(());
    }
    let mut releases = Vec::new();
    {
        let _lock = lock(&root)?;
        for entry in fs::read_dir(&root).map_err(io)? {
            let entry = entry.map_err(io)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !entry.file_type().map_err(io)?.is_dir() {
                continue;
            }
            if name.starts_with(".pending-") {
                fs::remove_dir_all(entry.path()).map_err(io)?;
            } else if name.len() == 64 && name.bytes().all(|c| c.is_ascii_hexdigit()) {
                let dir = entry.path();
                match load(&dir) {
                    Ok(receipt)
                        if stored_dir(&root, &receipt.manifest.id, &receipt.manifest.version)?
                            == dir =>
                    {
                        releases.push((receipt, dir))
                    }
                    _ => tracing::warn!(path=%dir.display(),"skipping invalid installed release"),
                }
            }
        }
    }
    releases.sort_by(|a, b| a.0.installed_at.cmp(&b.0.installed_at).then(a.1.cmp(&b.1)));
    for (receipt, dir) in releases {
        runtime
            .register_installed_plugin(receipt.plugin(&dir), |old| {
                check_source(old, &dir, &receipt)
            })
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn package() -> Value {
        let files = BTreeMap::from([
            ("assets/main.js", (sha(b"const n=1;"), 10)),
            ("main.html", (sha(b"<p>1</p>"), 8)),
        ]);
        let image = format!("sha256:{}", "a".repeat(64));
        json!({"plugin_id":"fixture/math","plugin_version":"1","image_digest":image,
            "manifest":{"id":"fixture/math","name":"Math","version":"1","image":format!("registry:5000/fixture/math@{image}"),
                "ui":{"entry":"web/dist/main.html"},"inputs":{"schema":{"type":"object","properties":{"note":{"default":null}}}}},
            "skill":{"name":"Math","description":"Fixture","body_markdown":"# Usage"},
            "ui_sha256":sha(&serde_json::to_vec(&files).unwrap()),
            "ui_files":[{"path":"main.html","data_hex":hex::encode(b"<p>1</p>"),"sha256":sha(b"<p>1</p>"),"size_bytes":8},
                        {"path":"assets/main.js","data_hex":hex::encode(b"const n=1;"),"sha256":sha(b"const n=1;"),"size_bytes":10}]})
    }

    fn read(v: Value) -> CompassResult<(Receipt, StagedAssets)> {
        validate_payload(
            serde_json::from_value(v).unwrap(),
            &PluginId::new("fixture/math").unwrap(),
            "1",
            "https://hub.example".into(),
            OrgId::new("org-test").unwrap(),
        )
    }

    #[test]
    fn exact_nested_ui_json_schema_and_skill_survive_idempotent_commit() {
        let root = tempfile::tempdir().unwrap();
        let (receipt, bytes) = read(package()).unwrap();
        let dst = stored_dir(root.path(), &receipt.manifest.id, "1").unwrap();
        commit(root.path(), &dst, &receipt, &bytes).unwrap();
        let original = fs::read(dst.join("release.json")).unwrap();
        let (mut replay, bytes) = read(package()).unwrap();
        replay.acquired_by_org = OrgId::new("another-permitted-org").unwrap();
        commit(root.path(), &dst, &replay, &bytes).unwrap();
        assert_eq!(original, fs::read(dst.join("release.json")).unwrap());
        let plugin = load(&dst).unwrap().plugin(&dst);
        assert!(plugin.manifest.inputs.schema["properties"]["note"]["default"].is_null());
        assert_eq!(plugin.skill.unwrap().body_markdown, "# Usage");
        assert_eq!(
            fs::read(dst.join("content/web/dist/main.html")).unwrap(),
            b"<p>1</p>"
        );
        assert_eq!(
            fs::read(dst.join("content/web/dist/assets/main.js")).unwrap(),
            b"const n=1;"
        );
    }

    #[test]
    fn conflict_preserves_the_earlier_release_and_origin() {
        let root = tempfile::tempdir().unwrap();
        let (a, bytes) = read(package()).unwrap();
        let dst = stored_dir(root.path(), &a.manifest.id, "1").unwrap();
        commit(root.path(), &dst, &a, &bytes).unwrap();
        let (mut b, newbytes) = read(package()).unwrap();
        b.origin = "https://other.example".into();
        let e = commit(root.path(), &dst, &b, &newbytes)
            .unwrap_err()
            .to_string();
        assert!(e.contains("https://hub.example") && e.contains("https://other.example"));
        b.origin = a.origin.clone();
        b.skill.as_mut().unwrap().body_markdown = "mutated".into();
        assert!(commit(root.path(), &dst, &b, &newbytes).is_err());
        assert_eq!(load(&dst).unwrap().skill.unwrap().body_markdown, "# Usage");
        fs::write(dst.join("content/unpublished"), b"x").unwrap();
        assert!(load(&dst).is_err());
    }

    #[test]
    fn rejects_incomplete_or_substituted_payload_before_disk_writes() {
        for (path, value) in [
            ("/plugin_id", json!("other/math")),
            ("/manifest/version", json!("2")),
            ("/plugin_version", json!("2")),
            ("/manifest/execution", json!("cloud_only")),
            ("/image_digest", json!(format!("sha256:{}", "b".repeat(64)))),
            ("/ui_files/0/data_hex", json!("00")),
            ("/ui_files/1/size_bytes", json!(99)),
            ("/ui_files/1/path", json!("main.html")),
            ("/ui_files/0/path", json!("../main.html")),
            ("/manifest/ui/entry", json!("../main.html")),
            ("/manifest/ui/entry", json!("web/other.html")),
            ("/ui_sha256", json!("different")),
        ] {
            let mut v = package();
            if path == "/manifest/execution" {
                v["manifest"]["execution"] = value;
            } else {
                *v.pointer_mut(path).unwrap() = value;
            }
            assert!(read(v).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn competing_different_versions_restore_by_completion_order() {
        let root = tempfile::tempdir().unwrap();
        let (mut slow, bytes_a) = read(package()).unwrap();
        let (mut fast, bytes_b) = read(package()).unwrap();
        fast.manifest.version = "2".into();
        // Both acquisitions are in flight. Version 2 started later, then finishes first.
        slow.installed_at = chrono::Utc::now() - chrono::Duration::hours(1);
        fast.installed_at = chrono::Utc::now() + chrono::Duration::hours(1);
        let a = stored_dir(root.path(), &slow.manifest.id, "1").unwrap();
        let b = stored_dir(root.path(), &fast.manifest.id, "2").unwrap();
        commit(root.path(), &b, &fast, &bytes_b).unwrap();
        commit(root.path(), &a, &slow, &bytes_a).unwrap();
        let first = load(&b).unwrap().installed_at;
        let last = load(&a).unwrap().installed_at;
        assert!(
            last > first,
            "restoration must select the last committed distinct release"
        );
        commit(root.path(), &b, &fast, &bytes_b).unwrap();
        assert_eq!(load(&b).unwrap().installed_at, first);
    }

    #[test]
    fn concurrent_first_install_exposes_only_one_complete_release() {
        let root = tempfile::tempdir().unwrap();
        let (result_a, result_b) = std::thread::scope(|s| {
            let try_install = |note: &str| {
                let (mut r, bytes) = read(package()).unwrap();
                r.skill.as_mut().unwrap().description = note.into();
                commit(
                    root.path(),
                    &stored_dir(root.path(), &r.manifest.id, "1").unwrap(),
                    &r,
                    &bytes,
                )
                .is_ok()
            };
            let a = s.spawn(move || try_install("a"));
            let b = s.spawn(move || try_install("b"));
            (a.join().unwrap(), b.join().unwrap())
        });
        assert_ne!(result_a, result_b);
        let (a, _) = read(package()).unwrap();
        let actual = load(&stored_dir(root.path(), &a.manifest.id, "1").unwrap()).unwrap();
        assert_eq!(
            actual.skill.unwrap().description,
            if result_a { "a" } else { "b" }
        );
    }

    #[test]
    fn dev_05_installs_and_verifies_optional_icon_svg_and_rejects_tampered_icon() {
        let root = tempfile::tempdir().unwrap();
        let icon_bytes = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect width=\"8\" height=\"8\"/></svg>";
        let mut pkg = package();
        pkg["manifest"]["icon"] = json!("icon.svg");
        pkg["icon_file"] = json!({
            "path": "icon.svg",
            "size_bytes": icon_bytes.len() as u64,
            "sha256": sha(icon_bytes),
            "data_hex": hex::encode(icon_bytes),
        });

        let (receipt, bytes) = read(pkg).unwrap();
        let dst = stored_dir(root.path(), &receipt.manifest.id, "1").unwrap();
        commit(root.path(), &dst, &receipt, &bytes).unwrap();
        assert_eq!(
            fs::read(dst.join("content/icon.svg")).expect("installed icon.svg on disk"),
            icon_bytes
        );
        let loaded = load(&dst).unwrap();
        assert_eq!(loaded.manifest.icon.as_deref(), Some("icon.svg"));

        // Tampering with installed icon.svg must fail load() inventory check.
        fs::write(dst.join("content/icon.svg"), b"<svg>tampered</svg>").unwrap();
        assert!(load(&dst).is_err());
    }
}
