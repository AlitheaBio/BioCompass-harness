//! Local private dataset supervisor. The operator provisions ACLs. All scientist actions authenticate with the configured BioCompass hub.
#[path = "dataset_manager/runs.rs"]
mod runs;
#[path = "dataset_manager/run_queries.rs"]
mod run_queries;
#[path = "dataset_manager/generic_provider.rs"]
mod generic_provider;
#[path = "dataset_manager/query_access.rs"]
mod query_access;
#[path = "dataset_manager/seal.rs"]
mod seal;
use biocompass::dataset_store as store;
#[path = "dataset_manager/versions.rs"]
mod versions;
#[path = "dataset_manager/worker.rs"]
mod worker;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc};
use store::{LocalStore, State as DatasetState};

#[derive(Parser)]
#[command(about = "BioCompass private dataset workspace")]
struct Cli {
    #[arg(long, env = "BC_DATASET_STORE")]
    store: PathBuf,
    #[arg(long, env = "BC_DRAFT_WORKSPACE")]
    workspace: PathBuf,
    #[arg(long, env = "BC_IDENTITY_HUB")]
    identity_hub: String,
    #[arg(long, env = "BC_CONTAINER_ENGINE", default_value = "docker")]
    engine: String,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Run the authenticated local dataset service.
    Serve {
        #[arg(long, default_value_t = 18961)]
        port: u16,
        /// Directory containing the operator-approved, digest-pinned Target Validation release.
        #[arg(long, env = "BC_TARGET_VALIDATION_MODULE")]
        target_validation_module: Option<PathBuf>,
        /// Directory containing the operator-approved, digest-pinned Off-Target Toxicity release.
        #[arg(long, env = "BC_OFF_TARGET_TOXICITY_MODULE")]
        off_target_toxicity_module: Option<PathBuf>,
        /// Directory containing the operator-approved, digest-pinned Toxicity Screening release.
        #[arg(long, env = "BC_TOXICITY_SCREENING_MODULE")]
        toxicity_screening_module: Option<PathBuf>,
        #[arg(long, env = "BC_VERSION_WORKER_IMAGE")]
        version_worker: Option<String>,
        /// Operator-reviewed query modules for sealed tabular datasets.
        #[arg(long = "scoped-query-module")]
        scoped_query_modules: Vec<PathBuf>,
        #[arg(long, env = "BC_PARTNER_IDENTITY_HUB")]
        partner_identity_hub: Option<String>,
    },
    /// Publish an existing sealed tabular catalog and its provider-defined scope relation.
    AdmitSealed {
        #[arg(long)]
        dataset: String,
        #[arg(long)]
        org: String,
        #[arg(long)]
        catalog: PathBuf,
        #[arg(long)]
        relation: PathBuf,
        #[arg(long = "publisher", required = true)]
        publishers: Vec<String>,
    },
    /// Operator-only adoption of an existing draft. The signed-in users receive separate dataset permissions.
    Provision {
        #[arg(long)]
        dataset: String,
        #[arg(long)]
        org: String,
        #[arg(long)]
        draft: String,
        #[arg(long = "editor")]
        editors: Vec<String>,
        #[arg(long = "publisher")]
        publishers: Vec<String>,
        #[arg(long, env = "BC_VALIDATOR_IMAGE")]
        validator_image: String,
        #[arg(long, env = "BC_OVERLAY_IMAGE")]
        overlay_image: String,
    },
}

#[derive(Clone)]
struct App {
    root: PathBuf,
    workspace: PathBuf,
    store: LocalStore,
    issuer: String,
    partner_issuer: Option<String>,
    engine: String,
    http: reqwest::Client,
    slots: Arc<tokio::sync::Semaphore>,
    runs: Option<runs::PrivateRuns>,
    version_worker: Option<String>,
}
#[derive(Clone, Deserialize)]
struct User {
    #[serde(skip)]
    issuer: String,
    org_id: String,
    user_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    expected_revision: u64,
    patch: Value,
    #[serde(default)]
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Publish {
    expected_revision: u64,
}
struct Error(String);
impl From<String> for Error {
    fn from(s: String) -> Self {
        Self(s)
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (code, message) = self
            .0
            .split_once(": ")
            .unwrap_or(("storage", "dataset operation failed"));
        let status = match code {
            "unauthorized" => StatusCode::UNAUTHORIZED,
            "forbidden" => StatusCode::FORBIDDEN,
            "not_found" => StatusCode::NOT_FOUND,
            "conflict" => StatusCode::CONFLICT,
            "invalid" => StatusCode::UNPROCESSABLE_ENTITY,
            "issuer" => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            eprintln!("Private dataset operation: {}", self.0);
        }
        let public = if status == StatusCode::INTERNAL_SERVER_ERROR {
            "dataset operation failed"
        } else {
            message
        };
        (status, Json(json!({"error":code,"message":public}))).into_response()
    }
}

impl App {
    async fn user(&self, headers: &HeaderMap) -> Result<User, Error> {
        self.user_at(headers, &self.issuer).await
    }

    async fn query_user(&self, headers: &HeaderMap) -> Result<User,Error> {
        let issuer = match headers.get("x-biocompass-issuer") {
            None => self.issuer.as_str(),
            Some(header) => header.to_str().ok().filter(|value|
                *value == self.issuer || self.partner_issuer.as_deref() == Some(*value))
                .ok_or_else(|| Error("unauthorized: unconfigured identity issuer".into()))?,
        };
        self.user_at(headers, issuer).await
    }

    async fn user_at(&self, headers: &HeaderMap, issuer: &str) -> Result<User, Error> {
        let auth = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error("unauthorized: user session required".into()))?;
        let res = self
            .http
            .get(format!("{issuer}/api/v1/auth/whoami"))
            .bearer_auth(auth)
            .send()
            .await
            .map_err(|_| Error("issuer: identity hub unavailable".into()))?;
        if matches!(res.status().as_u16(), 401 | 403) {
            return Err(Error("unauthorized: invalid session".into()));
        }
        if !res.status().is_success() {
            return Err(Error("issuer: identity hub unavailable".into()));
        }
        let mut user = res
            .json::<User>()
            .await
            .map_err(|_| Error("unauthorized: authenticated user identity required".into()))?;
        if user.org_id.is_empty() || user.user_id.is_empty() {
            return Err(Error("unauthorized: missing user identity".into()));
        }
        user.issuer = issuer.to_owned();
        Ok(user)
    }
}

fn overlay_pin(state: &DatasetState) -> Result<&str, String> {
    state
        .validation
        .as_ref()
        .and_then(|v| v["overlay_image"].as_str())
        .ok_or_else(|| "conflict: missing materializer pin".into())
}
fn patches(state: &DatasetState) -> Vec<Value> {
    state.audit.iter().map(|e| e.patch.clone()).collect()
}

async fn state(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DatasetState>, Error> {
    let u = app.user(&headers).await?;
    Ok(Json(app.store.read_state(&id, &u.org_id, &u.user_id)?))
}

async fn edit(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(e): Json<Edit>,
) -> Result<Json<DatasetState>, Error> {
    let u = app.user(&headers).await?;
    let s = app.store.authorize_edit(&id, &u.org_id, &u.user_id)?;
    if e.expected_revision != s.revision {
        if s.audit.iter().any(|a| {
            a.revision.checked_sub(1) == Some(e.expected_revision)
                && a.actor == u.user_id
                && a.patch == e.patch
                && a.reason == e.reason
        }) {
            return Ok(Json(s));
        }
        return Err(Error("conflict: draft revision has changed".into()));
    }
    if s.release.is_some() {
        return Err(Error("conflict: draft already published".into()));
    }
    let permit = app
        .slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| Error("storage: worker admission closed".into()))?;
    blocking(move || {
        let _permit = permit;
        let source = worker::source(&app.workspace, &s.draft)?;
        let mut edits = patches(&s);
        edits.push(e.patch.clone());
        let output = worker::render(
            &app.engine,
            &s.importer,
            overlay_pin(&s)?,
            &source,
            &edits,
            None,
        )?;
        app.store.edit(
            &id,
            &u.org_id,
            &u.user_id,
            e.expected_revision,
            e.patch,
            &e.reason,
            output.validation,
        )
    })
    .await
    .map(Json)
}

async fn publish(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(p): Json<Publish>,
) -> Result<Json<DatasetState>, Error> {
    let u = app.user(&headers).await?;
    let current = app.store.authorize_publish(&id, &u.org_id, &u.user_id)?;
    if current.revision != p.expected_revision {
        return Err(Error("conflict: draft revision has changed".into()));
    }
    if current.release.is_some() {
        return Ok(Json(current));
    }
    if current
        .validation
        .as_ref()
        .and_then(|v| v["valid"].as_bool())
        != Some(true)
    {
        return Err(Error("conflict: draft has not passed validation".into()));
    }
    let permit = app
        .slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| Error("storage: worker admission closed".into()))?;
    blocking(move || {
        let _permit = permit;
        let state =
            app.store
                .publish(&id, &u.org_id, &u.user_id, p.expected_revision, |dir, s| {
                    let source = worker::source(&app.workspace, &s.draft)?;
                    let output = worker::render(
                        &app.engine,
                        &s.importer,
                        overlay_pin(s)?,
                        &source,
                        &patches(s),
                        Some(dir),
                    )?;
                    if Some(&output.validation) != s.validation.as_ref() {
                        return Err(
                            "conflict: staged result differs from the validated draft".into()
                        );
                    }
                    seal::seal(
                        dir,
                        &output.rendered.join("tables"),
                        &format!("bc-private-v1/{id}"),
                        &output.validation,
                        &source,
                        json!(s.audit),
                    )
                })?;
        checkpoint("committed_before_reply");
        Ok(state)
    })
    .await
    .map(Json)
}

async fn verify(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, Error> {
    let u = app.user(&headers).await?;
    let s = app.store.read_state(&id, &u.org_id, &u.user_id)?;
    let release = s
        .release
        .ok_or_else(|| Error("not_found: no published version".into()))?;
    let relation = runs::release_relation(&release)?;
    blocking(move || {
        let object = app.root.join(id).join(release.object_dir);
        if let Some(relation) = relation {
            return generic_provider::verify(&object, &release.manifest, &relation);
        }
        let verified = seal::verify(&object)?;
        if verified != release.manifest {
            return Err("conflict: persisted manifest differs from committed version".into());
        }
        Ok(verified)
    })
    .await
    .map(Json)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Error("storage: failed worker task".into()))?
        .map_err(Error)
}

fn checkpoint(_name: &str) {
    #[cfg(debug_assertions)]
    if std::env::var("BC_DATASET_TEST_CHECKPOINT").as_deref() == Ok(_name)
        && let Some(marker) = std::env::var_os("BC_DATASET_TEST_MARKER")
    {
        fs::write(marker, _name).expect("debug fault checkpoint marker");
        loop {
            std::thread::park();
        }
    }
}

fn canonical_issuer(issuer: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(issuer).map_err(|_| "invalid identity hub URL")?;
    let loopback = url
        .host_str()
        .is_some_and(|h| matches!(h, "localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("identity hub must be an HTTPS origin or a loopback HTTP origin".into());
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn bind_identity(root: &std::path::Path, issuer: &str, provision: bool) -> Result<String, String> {
    use std::io::Write;
    let issuer = canonical_issuer(issuer)?;
    let file = root.join("identity-hub");
    if provision && !file.exists() {
        let mut tmp = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
        tmp.write_all(issuer.as_bytes())
            .and_then(|()| tmp.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        if let Err(e) = tmp.persist_noclobber(&file)
            && e.error.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(e.to_string());
        }
    }
    if fs::read_to_string(file).map_err(|e| e.to_string())? != issuer {
        return Err("identity hub differs from provisioned workspace issuer".into());
    }
    Ok(issuer)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let store = LocalStore::open(&cli.store)?;
    let root = fs::canonicalize(&cli.store)?;
    let issuer = bind_identity(
        &root,
        &cli.identity_hub,
        matches!(&cli.command, Action::Provision { .. } | Action::AdmitSealed { .. }),
    )?;
    match cli.command {
        Action::Serve {
            port,
            target_validation_module,
            off_target_toxicity_module,
            toxicity_screening_module,
            version_worker,
            scoped_query_modules,
            partner_identity_hub,
        } => {
            if let Some(image) = version_worker.as_deref() {
                worker::image_id(image)?;
            }
            let modules = [
                ("alithea/bc-target-validation", target_validation_module),
                ("alithea/bc-off-target-toxicity", off_target_toxicity_module),
                ("alithea/bc-toxicity-screening", toxicity_screening_module),
            ];
            let runs = if !scoped_query_modules.is_empty() || modules.iter().any(|(_, module)| module.is_some()) {
                Some(runs::PrivateRuns::load(&root.join(".runtime"), modules, scoped_query_modules).await?)
            } else {
                None
            };
            let app = App {
                root,
                workspace: cli.workspace,
                store,
                issuer,
                partner_issuer: partner_identity_hub.as_deref().map(canonical_issuer).transpose()?,
                engine: cli.engine,
                http: reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(std::time::Duration::from_secs(10))
                    .build()?,
                slots: Arc::new(tokio::sync::Semaphore::new(2)),
                runs,
                version_worker,
            };
            let router = Router::new()
                .route("/health", get(|| async { "ok" }))
                .route("/v1/datasets/:id", get(state))
                .route("/v1/datasets/:id/query", post(query_access::query))
                .route("/v1/datasets/:id/query-grants", post(query_access::set))
                .route("/v1/datasets/:id/edit", post(edit))
                .route("/v1/datasets/:id/publish", post(publish))
                .route("/v1/datasets/:id/verify", get(verify))
                .route("/v1/datasets/:id/versions/preview", post(versions::preview))
                .route(
                    "/v1/datasets/:id/versions/:change_id/publish",
                    post(versions::publish),
                )
                .route("/v1/datasets/:id/runs/prepare", post(runs::prepare))
                .route("/v1/datasets/:id/runs/:run_id/confirm", post(runs::confirm))
                .route(
                    "/v1/datasets/:id/runs/:run_id/artifacts/:name",
                    get(runs::export),
                )
                .layer(DefaultBodyLimit::max(256 * 1024))
                .with_state(app);
            axum::serve(
                tokio::net::TcpListener::bind(("127.0.0.1", port)).await?,
                router,
            )
            .await?;
        }
        Action::AdmitSealed { dataset, org, catalog, relation, publishers } => {
            let state = generic_provider::admit(&store, &dataset, &org, publishers, &catalog, &relation)?;
            println!("{}", state.release.as_ref().expect("sealed catalog published").manifest);
        }
        Action::Provision {
            dataset,
            org,
            draft,
            editors,
            publishers,
            validator_image,
            overlay_image,
        } => {
            let source = worker::source(&cli.workspace, &draft)?;
            let checked = worker::attest(&cli.engine, &validator_image, &overlay_image, &source)?;
            store.provision_validated(
                &dataset,
                &org,
                &draft,
                draft.trim_start_matches("sha256-"),
                &validator_image,
                editors,
                publishers,
                checked,
            )?;
            println!(
                "{}",
                json!({"dataset":dataset,"draft":draft,"org":org,"visibility":"private"})
            );
        }
    }
    Ok(())
}
