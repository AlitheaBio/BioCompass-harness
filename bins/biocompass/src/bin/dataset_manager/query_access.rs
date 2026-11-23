//! Dataset-scoped query grants for the authenticated local provider.
use super::{App, Error, User, blocking};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use biocompass_core_traits::{DatasetGrant, FilterExpr};
use biocompass_data_engine::{CatalogSummary, scoped_join::ScopeKey};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path as FsPath, PathBuf},
};

#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Rows,
    ComputeOnly,
    Revoked,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Grant {
    pub access: Access,
    pub samples: Vec<ScopeKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_classes: Option<Vec<String>>,
    pub epoch: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetGrant {
    issuer: String,
    org_id: String,
    access: Access,
    #[serde(default, alias = "keys")]
    samples: Vec<ScopeKey>,
    sample_classes: Option<Vec<String>>,
}

fn storage(error: impl std::fmt::Display) -> String {
    format!("storage: {error}")
}

fn key(issuer: &str, org: &str) -> String {
    // Bind both the verified issuer origin and authenticated organization.
    hex::encode(Sha256::digest(json!([issuer, org]).to_string()))
}

fn path(root: &FsPath, issuer: &str, org: &str) -> PathBuf {
    root.join("query-grants").join(key(issuer, org))
}

pub fn load(root: &FsPath, user: &User) -> Result<Option<Grant>, String> {
    read(&path(root, &user.issuer, &user.org_id))
}
fn read(path: &FsPath) -> Result<Option<Grant>, String> {
    match fs::File::open(path) {
        Ok(file) => serde_json::from_reader(file).map(Some).map_err(storage),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

pub fn supports(current: &DatasetGrant, pinned: &DatasetGrant) -> bool {
    if current.grant_id != pinned.grant_id
        || current.org_id != pinned.org_id
        || current.catalog_id != pinned.catalog_id
    {
        return false;
    }
    match (
        current.row_scope.as_ref(),
        pinned.row_scope.as_ref(),
    ) {
        (None, None) => true,
        (
            Some(FilterExpr::In { column: current_column, values: current }),
            Some(FilterExpr::In { column: pinned_column, values: pinned }),
        ) if current_column == pinned_column => {
            let allowed: Option<std::collections::HashSet<_>> = current.iter().map(ScopeKey::from_scalar).collect();
            allowed.is_some_and(|allowed| pinned.iter().all(|key| {
                ScopeKey::from_scalar(key).is_some_and(|key| allowed.contains(&key))
            }))
        }
        _ => false,
    }
}

pub fn filter(grant: &Grant, column: &str) -> FilterExpr {
    if let Some(classes) = &grant.sample_classes {
        return FilterExpr::in_values("sample_class", classes.iter().cloned());
    }
    FilterExpr::In {
        column: column.to_owned(),
        values: grant.samples.iter().map(Into::into).collect(),
    }
}

pub async fn set(
    State(app): State<App>,
    Path(dataset): Path<String>,
    headers: HeaderMap,
    Json(mut req): Json<SetGrant>,
) -> Result<Json<Grant>, Error> {
    let actor = app.user(&headers).await?;
    let state = app.store.authorize_publish(&dataset, &actor.org_id, &actor.user_id)?;
    let scoped_release = state.release.as_ref().map(|release| {
        super::runs::release_relation(release)
            .map(|relation| relation.map(|relation| (release.object_dir.clone(), relation)))
    }).transpose()?.flatten();
    if scoped_release.is_some() && req.access == Access::Rows {
        return Err(Error("invalid: this dataset template supports compute-only queries".into()));
    }
    if req.issuer != app.issuer && app.partner_issuer.as_deref() != Some(&req.issuer) {
        return Err(Error("invalid: grant issuer is not configured".into()));
    }
    if let Some(classes) = &mut req.sample_classes {
        if scoped_release.is_some() || req.access == Access::Revoked || !req.samples.is_empty()
            || classes.is_empty() || classes.len() > 10000
            || classes.iter().any(|class| class.trim().is_empty() || class.len() > 256)
        {
            return Err(Error("invalid: sample_classes requires a private HLA rows or compute-only grant with 1–10000 text classes and no sample IDs".into()));
        }
        classes.sort();
        classes.dedup();
    }
    if req.org_id.is_empty()
        || req.org_id.len() > 200
        || req.samples.len() > 10000
        || req.samples.iter().any(|key| {
            !key.is_wire_safe() || (scoped_release.is_none() && !matches!(key, ScopeKey::Text(_)))
        })
        || (req.access == Access::Revoked && !req.samples.is_empty())
    {
        return Err(Error(
            "invalid: invalid grant organization, scope or access level".into(),
        ));
    }
    req.samples.sort();
    req.samples.dedup();
    blocking(move || {
        let directory = app.root.join(&dataset);
        if let Some((object, relation)) = scoped_release
            && !req.samples.is_empty()
        {
            let manifest = fs::File::open(directory.join(object).join("ducklake/catalog.json"))
                .map_err(storage)?;
            let catalog: CatalogSummary = serde_json::from_reader(manifest).map_err(storage)?;
            relation.validate_keys(&catalog.latest_sealed, &req.samples)
                .map_err(|e| format!("invalid: {e}"))?;
        }
        let file = path(&directory, &req.issuer, &req.org_id);
        let dir = file.parent().expect("grant parent");
        fs::create_dir_all(dir).map_err(storage)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("writer.lock"))
            .map_err(storage)?;
        lock.lock().map_err(storage)?;
        let previous = read(&file)?;
        let epoch = match previous {
            Some(old)
                if old.access == req.access
                    && match (&old.sample_classes, &req.sample_classes) {
                        (None, None) => true,
                        (Some(old), Some(new)) => old.iter().all(|c| new.binary_search(c).is_ok()),
                        _ => false,
                    }
                    && old
                        .samples
                        .iter()
                        .all(|x| req.samples.binary_search(x).is_ok()) =>
            {
                old.epoch
            }
            Some(old) => old
                .epoch
                .checked_add(1)
                .ok_or("conflict: grant epoch exhausted")?,
            None => 1,
        };
        let grant = Grant {
            access: req.access,
            samples: req.samples,
            sample_classes: req.sample_classes,
            epoch,
        };
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(storage)?;
        serde_json::to_writer(&mut tmp, &grant).map_err(storage)?;
        tmp.flush().map_err(storage)?;
        tmp.as_file().sync_all().map_err(storage)?;
        tmp.persist(&file).map_err(storage)?;
        fs::File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(storage)?;
        Ok(grant)
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Read {
    version: u64,
    query: String,
    sample_class: Option<String>,
    page_size: Option<usize>,
    after_sample_id: Option<String>,
}

pub async fn query(
    State(app): State<App>,
    Path(dataset): Path<String>,
    headers: HeaderMap,
    Json(req): Json<Read>,
) -> Result<Response, Error> {
    let user = app.query_user(&headers).await?;
    let root = app.root.join(&dataset);
    let (release, grant, object) = super::runs::access(&app, &user, &dataset, req.version)?;
    if super::runs::release_relation(&release)?.is_some() {
        return Err(Error("forbidden: this dataset template supports reviewed compute-only queries".into()));
    }
    let require_rows = |policy: Option<Grant>| -> Result<(), Error> {
        if policy.is_some_and(|p| p.access == Access::Rows) {
            Ok(())
        } else {
            Err(Error("forbidden: named reads require a rows grant".into()))
        }
    };
    require_rows(load(&root, &user)?)?;
    let snapshot = release.manifest["snapshot_id"]
        .as_u64()
        .ok_or("storage: missing snapshot")
        .map_err(str::to_owned)?;
    let digest = release.manifest["dataset_digest"]
        .as_str()
        .ok_or("storage: missing digest")
        .map_err(str::to_owned)?
        .to_string();
    let pin = digest.clone();
    let scope = grant.row_scope.clone();
    let name = req.query.clone();
    let parameters = biocompass_node_runtime::RuntimeReadParams {
        sample_class: req.sample_class,
        page_size: req.page_size,
        after_sample_id: req.after_sample_id,
        ..Default::default()
    };
    let page = blocking(move || {
        biocompass_node_runtime::direct_named_read_page(
            &object.join("ducklake"),
            snapshot,
            &pin,
            scope.as_ref(),
            &name,
            &parameters,
        )
        .map_err(|e| super::runs::run_error(e).0)
    })
    .await?;
    // Check the current policy again before emitting a result.
    require_rows(load(&root, &user)?)?;
    let (_, current, _) = super::runs::access(&app, &user, &dataset, req.version)?;
    if !supports(&current, &grant) {
        return Err(Error(
            "forbidden: dataset access changed while query was running".into(),
        ));
    }
    let mut response = json!(page);
    response["query"] = json!(req.query);
    response["version"] = json!(req.version);
    response["snapshot"] = json!(snapshot);
    response["dataset_digest"] = json!(digest);
    Ok(([(header::CACHE_CONTROL, "private, no-store")], Json(response)).into_response())
}
