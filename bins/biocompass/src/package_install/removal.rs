//! Remove a managed release or detach an authored workspace registration while retaining its files.
use super::{invalid, io, json_error, lock, path_ok, stored_dir};
use crate::state::AppState;
use biocompass_core_traits::{
    CompassError, CompassResult, OrgId, PluginId, local_delivery::DeliveryScope,
};
use biocompass_plugin_spec::{LoadedPlugin, PluginExecutionPlane};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
struct Detachment {
    id: PluginId,
    version: String,
    directory: String,
    terms_hash: String,
    origin: String,
    org: OrgId,
    detached_at: chrono::DateTime<chrono::Utc>,
}

enum Source {
    Managed,
    Workspace(String),
    Protected,
}

fn source(state: &AppState, plugin: &LoadedPlugin) -> CompassResult<Source> {
    let Some(dir) = plugin.root_dir.as_deref() else {
        return Ok(Source::Protected);
    };
    let managed = stored_dir(
        &state.data_dir.join("module-releases"),
        &plugin.manifest.id,
        &plugin.manifest.version,
    )?;
    if dir == managed.join("content") {
        return Ok(Source::Managed);
    }
    if dir.parent() == Some(state.user_plugins_dir.as_path())
        && let Some(name) = dir.file_name().and_then(|s| s.to_str())
        && path_ok(name)
        && !name.contains('/')
    {
        return Ok(Source::Workspace(name.into()));
    }
    Ok(Source::Protected)
}

pub(crate) fn removable(state: &AppState, p: &LoadedPlugin) -> bool {
    state.installed_workspace.is_some()
        && matches!(source(state, p), Ok(Source::Managed | Source::Workspace(_)))
}

fn marker(root: &Path, id: &PluginId, version: &str) -> CompassResult<PathBuf> {
    stored_dir(&root.join("detached-modules"), id, version)
}

pub(crate) fn is_detached(root: &Path, id: &PluginId, version: &str) -> CompassResult<bool> {
    marker(root, id, version)?.try_exists().map_err(io)
}

fn read_marker(path: &Path) -> CompassResult<Detachment> {
    let d: Detachment = serde_json::from_slice(&fs::read(path).map_err(io)?).map_err(json_error)?;
    if !path_ok(&d.directory)
        || d.directory.contains('/')
        || stored_dir(path.parent().unwrap(), &d.id, &d.version)? != path
    {
        return Err(invalid("invalid detached module record"));
    }
    Ok(d)
}

fn owned(scope: &DeliveryScope, origin: &str, org: &OrgId) -> CompassResult<()> {
    if scope.origin != origin || &scope.org_id != org {
        return Err(CompassError::AccessDenied(
            "this workspace release belongs to another signed-in organization or hub".into(),
        ));
    }
    Ok(())
}

fn load_source(state: &AppState, d: &Detachment) -> CompassResult<LoadedPlugin> {
    let p = LoadedPlugin::load_from_dir(
        state.user_plugins_dir.join(&d.directory),
        state.allow_local_dev_tags,
    )?;
    if p.manifest.id != d.id
        || p.manifest.version != d.version
        || biocompass_node_runtime::release_terms_hash(&p)? != d.terms_hash
    {
        return Err(invalid(
            "detached workspace source now declares a different release",
        ));
    }
    Ok(p)
}

/// Recovers source cards from their original roots; only validity and informational fields are exposed.
pub(crate) fn detached(state: &AppState) -> CompassResult<Vec<LoadedPlugin>> {
    if state.installed_workspace.is_none() {
        return Ok(Vec::new());
    }
    let root = state.data_dir.join("detached-modules");
    if !root.try_exists().map_err(io)? {
        return Ok(Vec::new());
    }
    let _lock = lock(&root)?;
    let mut out = Vec::new();
    for entry in fs::read_dir(&root).map_err(io)? {
        let e = entry.map_err(io)?;
        let name = e.file_name();
        let key = name.to_string_lossy();
        if key.len() != 64
            || !key.bytes().all(|c| c.is_ascii_hexdigit())
            || !e.file_type().map_err(io)?.is_file()
        {
            continue;
        }
        let d = read_marker(&e.path())?;
        if let Ok(p) = load_source(state, &d) {
            out.push((d.detached_at, p));
        }
    }
    out.sort_by_key(|v| v.0);
    Ok(out.into_iter().map(|(_, p)| p).collect())
}

fn create_marker(
    state: &AppState,
    p: &LoadedPlugin,
    name: String,
    scope: &DeliveryScope,
) -> CompassResult<bool> {
    let target = marker(&state.data_dir, &p.manifest.id, &p.manifest.version)?;
    let root = target.parent().unwrap();
    let _guard = lock(root)?;
    if target.try_exists().map_err(io)? {
        let d = read_marker(&target)?;
        owned(scope, &d.origin, &d.org)?;
        return Ok(false);
    }
    let d = Detachment {
        id: p.manifest.id.clone(),
        version: p.manifest.version.clone(),
        directory: name,
        terms_hash: biocompass_node_runtime::release_terms_hash(p)?,
        origin: scope.origin.clone(),
        org: scope.org_id.clone(),
        detached_at: chrono::Utc::now(),
    };
    let mut tmp = tempfile::NamedTempFile::new_in(root).map_err(io)?;
    tmp.write_all(&serde_json::to_vec(&d).map_err(json_error)?)
        .map_err(io)?;
    tmp.as_file().sync_all().map_err(io)?;
    tmp.persist(&target).map_err(|e| io(e.error))?;
    File::open(root).and_then(|f| f.sync_all()).map_err(io)?;
    Ok(true)
}

#[derive(Deserialize)]
struct PackageOwner {
    origin: String,
    acquired_by_org: OrgId,
    manifest: biocompass_plugin_spec::PluginManifest,
}

fn read_owner(
    release: &Path,
    id: &PluginId,
    version: &str,
    scope: &DeliveryScope,
) -> CompassResult<()> {
    let file = File::open(release.join("release.json")).map_err(io)?;
    let owner: PackageOwner = serde_json::from_reader(file).map_err(json_error)?;
    if &owner.manifest.id != id || owner.manifest.version != version {
        return Err(invalid("installed release identity mismatch"));
    }
    owned(scope, &owner.origin, &owner.acquired_by_org)
}

fn remove_managed(
    data_dir: &Path,
    id: &PluginId,
    version: &str,
    scope: &DeliveryScope,
) -> CompassResult<bool> {
    let root = data_dir.join("module-releases");
    let _guard = lock(&root)?;
    let release = stored_dir(&root, id, version)?;
    if !release.try_exists().map_err(io)? {
        return Ok(false);
    }
    read_owner(&release, id, version, scope)?;
    let retired = root.join(format!(".pending-remove-{}", uuid::Uuid::new_v4()));
    fs::rename(release, &retired).map_err(io)?;
    File::open(&root).and_then(|f| f.sync_all()).map_err(io)?;
    if let Err(error) = fs::remove_dir_all(&retired) {
        tracing::warn!(%error,"detached package cleanup will resume on startup");
    }
    Ok(true)
}

pub(crate) async fn remove(state: &AppState, id: &PluginId, version: &str) -> CompassResult<bool> {
    biocompass_core_traits::validate_requested_plugin_version(Some(version))?;
    let workspace = state.installed_workspace.as_ref().ok_or_else(|| {
        CompassError::AccessDenied(
            "a paired native workspace is required to remove a release".into(),
        )
    })?;
    let scope = workspace.scope().await?;
    // Check ownership before revealing an in-use status. Check it again under the commit lock.
    let release = stored_dir(&state.data_dir.join("module-releases"), id, version)?;
    let mark = marker(&state.data_dir, id, version)?;
    if release.try_exists().map_err(io)? {
        read_owner(&release, id, version, &scope)?;
    } else if mark.try_exists().map_err(io)? {
        let d = read_marker(&mark)?;
        owned(&scope, &d.origin, &d.org)?;
    }

    let runtime = state
        .local_runtime
        .as_ref()
        .ok_or_else(|| invalid("local runtime required"))?;
    let db = state
        .local_sqlite
        .as_ref()
        .ok_or_else(|| invalid("local run store required"))?;
    runtime.remove_plugin_with(id, version, async |current| {
        let Some(plugin) = current else {
            if mark.try_exists().map_err(io)? {
                let detached = read_marker(&mark)?;
                owned(&scope, &detached.origin, &detached.org)?;
            }
            return Ok(false);
        };
        let registration = source(state, plugin)?;
        if matches!(registration, Source::Protected) {
            return Err(CompassError::AccessDenied(
                "this module is bundled with the application or registered outside this workspace".into(),
            ));
        }
        if db.module_release_in_use(id, version).await? {
            return Err(invalid("this module version has an unexpired local quote or a running job"));
        }
        match registration {
            Source::Managed => remove_managed(&state.data_dir, id, version, &scope),
            Source::Workspace(name) => create_marker(state, plugin, name, &scope),
            Source::Protected => unreachable!(),
        }
    }).await
}

pub(crate) async fn restore_source(
    state: &AppState,
    id: &PluginId,
    version: &str,
) -> CompassResult<LoadedPlugin> {
    let workspace = state.installed_workspace.as_ref().ok_or_else(|| {
        CompassError::AccessDenied(
            "a paired native workspace is required to restore a release".into(),
        )
    })?;
    let scope = workspace.scope().await?;
    let runtime = state
        .local_runtime
        .as_ref()
        .ok_or_else(|| invalid("local runtime required"))?;
    let path = marker(&state.data_dir, id, version)?;
    if !path.try_exists().map_err(io)? {
        let current = runtime.get_plugin_version(id, Some(version)).await?;
        if matches!(source(state, &current)?, Source::Workspace(_)) {
            return Ok(current);
        }
        return Err(CompassError::NotFound(
            "workspace source is not detached".into(),
        ));
    }
    let d = read_marker(&path)?;
    owned(&scope, &d.origin, &d.org)?;
    let p = load_source(state, &d)?;
    if p.manifest.execution != PluginExecutionPlane::CloudOnly {
        let runner = state
            .oci_runner
            .as_ref()
            .ok_or_else(|| invalid("native container engine required"))?;
        if !runner.has_local_image(&p.manifest.image).await {
            runner.pull_image(&p.manifest.image).await?;
        }
    }
    runtime
        .register_workspace_plugin_with(p.clone(), |current| {
            if current.is_some_and(|old| old.root_dir != p.root_dir) {
                return Err(invalid("a different source already owns this release"));
            }
            let root = path.parent().unwrap();
            let _guard = lock(root)?;
            if path.try_exists().map_err(io)? {
                let now = read_marker(&path)?;
                owned(&scope, &now.origin, &now.org)?;
                if now.directory != d.directory || now.detached_at != d.detached_at {
                    return Err(invalid(
                        "workspace source detachment changed during restoration",
                    ));
                }
                load_source(state, &now)?;
                fs::remove_file(&path).map_err(io)?;
                File::open(root).and_then(|f| f.sync_all()).map_err(io)?;
            }
            Ok(())
        })
        .await?;
    Ok(p)
}
