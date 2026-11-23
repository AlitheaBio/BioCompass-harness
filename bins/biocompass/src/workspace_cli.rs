//! Operate the installed workspace through its authenticated local HTTP endpoints.
use biocompass_core_traits::local_delivery::{DeliveryScope, LocalDeliveryStore};
use biocompass_core_traits::{CompassError, CompassResult, OrgId, PluginId, RunId, RunQuote};
use clap::{Args, Subcommand};
use reqwest::{Client, Method, Url};
use serde_json::{Value, json};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Args)]
pub(super) struct Workspace {
    /// URL of the running local BioCompass workspace.
    #[arg(
        long,
        global = true,
        env = "BIOCOMPASS_WORKSPACE_URL",
        default_value = "http://127.0.0.1:8080"
    )]
    workspace_url: Url,
    /// API key for this request. Alternatives include COMPASS_BEARER_TOKEN and COMPASS_COOKIE.
    #[arg(long, global = true, env = "COMPASS_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    /// Bearer credential for this request.
    #[arg(
        long,
        global = true,
        env = "COMPASS_BEARER_TOKEN",
        hide_env_values = true
    )]
    bearer_token: Option<String>,
    /// Cookie header containing the active hub session.
    #[arg(long, global = true, env = "COMPASS_COOKIE", hide_env_values = true)]
    cookie: Option<String>,
    /// Read previously delivered runs or result bytes from this owned local data directory offline.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Hub origin returned by `workspace identity`. Required for offline reads.
    #[arg(long, global = true)]
    hub_url: Option<String>,
    /// Organization ID returned by `workspace identity`. Required for offline reads.
    #[arg(long, global = true)]
    org: Option<String>,
    /// Subject returned by `workspace identity`. Required for offline reads.
    #[arg(long, global = true)]
    subject: Option<String>,
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Show the active hub origin, organization, subject and local workspace ID.
    Identity,
    /// Request a hosted run quote. Review the returned price before confirming.
    Prepare {
        #[arg(long)]
        plugin: String,
        /// Exact published version. Omit to request the current available version.
        #[arg(long)]
        plugin_version: Option<String>,
        #[arg(long)]
        offer_id: Option<String>,
        #[arg(long)]
        grant_id: Option<String>,
        /// Module parameters as a JSON object.
        #[arg(long, default_value = "{}")]
        params: String,
    },
    /// Accept the quoted run. Repeating this command uses the same confirmation key by default.
    Confirm {
        run_id: String,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Read the current hosted run, including a wait for worker capacity.
    Status { run_id: String },
    /// Cancel a confirmed run that is waiting for worker capacity.
    Cancel { run_id: String },
    /// Receive and acknowledge completed deliveries for this workspace.
    Sync,
    /// List delivered runs, or show one run and its artifact names.
    Runs { run_id: Option<String> },
    /// Write the exact result JSON or a named artifact to standard output.
    Result {
        run_id: String,
        #[arg(long)]
        artifact: Option<String>,
    },
}

impl Workspace {
    pub(super) async fn execute(self) -> CompassResult<ExitCode> {
        if let Some(dir) = self.data_dir.as_deref() {
            return self.read_offline(dir).await;
        }
        if self.hub_url.is_some() || self.org.is_some() || self.subject.is_some() {
            return Err(CompassError::Validation(
                "--hub-url, --org and --subject select an offline scope and require --data-dir"
                    .into(),
            ));
        }
        let mut url = self.workspace_url;
        if !matches!(url.scheme(), "http" | "https")
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CompassError::Validation(
                "workspace URL must be HTTP(S) without credentials, query or fragment".into(),
            ));
        }
        let mut path = url
            .path_segments_mut()
            .map_err(|()| CompassError::Validation("invalid workspace URL".into()))?;
        path.pop_if_empty().extend(["api", "v1", "workspace"]);
        let mut method = Method::GET;
        let mut body = None;
        let mut raw = false;
        match &self.action {
            Action::Identity => {
                path.push("identity");
            }
            Action::Prepare {
                plugin,
                plugin_version,
                params,
                offer_id,
                grant_id,
            } => {
                let params = serde_json::from_str::<Value>(params)
                    .map_err(|e| CompassError::Validation(format!("invalid --params JSON: {e}")))?;
                if !params.is_object() {
                    return Err(CompassError::Validation(
                        "--params must be a JSON object".into(),
                    ));
                }
                path.extend(["runs", "prepare"]);
                method = Method::POST;
                biocompass_core_traits::validate_requested_plugin_version(
                    plugin_version.as_deref(),
                )?;
                let mut payload = json!({
                    "plugin_id": plugin,
                    "params": params,
                    "offer_id": offer_id,
                    "grant_id": grant_id,
                });
                if let Some(version) = plugin_version {
                    payload["plugin_version"] = json!(version);
                }
                body = Some(payload);
            }
            Action::Confirm {
                run_id,
                idempotency_key,
            } => {
                path.extend(["runs", segment(run_id)?, "confirm"]);
                method = Method::POST;
                body = Some(json!({"idempotency_key":idempotency_key}));
            }
            Action::Status { run_id } => {
                path.extend(["runs", segment(run_id)?, "status"]);
            }
            Action::Cancel { run_id } => {
                path.extend(["runs", segment(run_id)?, "cancel"]);
                method = Method::POST;
            }
            Action::Sync => {
                path.push("sync");
                method = Method::POST;
            }
            Action::Runs { run_id } => {
                path.push("runs");
                if let Some(run_id) = run_id {
                    path.push(segment(run_id)?);
                }
            }
            Action::Result { run_id, artifact } => {
                path.extend(["runs", segment(run_id)?]);
                if let Some(name) = artifact {
                    path.push("artifacts");
                    for component in name.split('/') {
                        path.push(segment(component)?);
                    }
                } else {
                    path.push("result");
                }
                raw = true;
            }
        }
        drop(path);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CompassError::Internal(format!("workspace HTTP client: {e}")))?;
        let request = client.request(method, url);
        let mut request = match (self.api_key, self.bearer_token, self.cookie) {
            (Some(key), None, None) => request.header("X-API-Key", key),
            (None, Some(token), None) => request.bearer_auth(token),
            (None, None, Some(cookie)) => request.header(reqwest::header::COOKIE, cookie),
            _ => return Err(CompassError::AccessDenied("select one workspace credential using --api-key, --bearer-token, --cookie, or its environment variable".into())),
        };
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| CompassError::Protocol(format!("request workspace: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(CompassError::Protocol(format!(
                "workspace returned {status}: {detail}"
            )));
        }
        if raw {
            let mut out = io::stdout().lock();
            while let Some(bytes) = response
                .chunk()
                .await
                .map_err(|e| CompassError::Protocol(format!("read workspace output: {e}")))?
            {
                out.write_all(&bytes).map_err(output_error)?;
            }
            out.flush().map_err(output_error)?;
        } else {
            let value: Value = response
                .json()
                .await
                .map_err(|e| CompassError::Protocol(format!("invalid workspace response: {e}")))?;
            if let Action::Prepare {
                plugin,
                plugin_version,
                offer_id,
                ..
            } = &self.action
            {
                let module = PluginId::new(plugin.clone()).map_err(CompassError::Validation)?;
                let quote: RunQuote = serde_json::from_value(value.clone())
                    .map_err(|e| CompassError::Protocol(format!("invalid workspace quote: {e}")))?;
                quote.validate_selection(&module, plugin_version.as_deref(), offer_id.as_deref())?;
            }
            print_json(&value)?;
            if matches!(self.action, Action::Sync)
                && value
                    .get("errors")
                    .and_then(Value::as_array)
                    .is_some_and(|errors| !errors.is_empty())
            {
                return Ok(ExitCode::FAILURE);
            }
        }
        Ok(ExitCode::SUCCESS)
    }
    async fn read_offline(&self, dir: &Path) -> CompassResult<ExitCode> {
        if !matches!(self.action, Action::Runs { .. } | Action::Result { .. }) {
            return Err(CompassError::Validation(
                "--data-dir supports offline reads with runs or result".into(),
            ));
        }
        let (Some(origin), Some(org), Some(subject)) = (&self.hub_url, &self.org, &self.subject)
        else {
            return Err(CompassError::Validation(
                "offline reads require --hub-url, --org and --subject from workspace identity"
                    .into(),
            ));
        };
        let scope = DeliveryScope {
            origin: origin.to_owned(),
            org_id: OrgId::new(org).map_err(CompassError::Validation)?,
            subject: subject.to_owned(),
        };
        let store = LocalDeliveryStore::open_workspace_existing(dir)?;
        match &self.action {
            Action::Runs { run_id: None } => {
                let runs: Vec<_> = store
                    .list(&scope)
                    .await?
                    .into_iter()
                    .map(|delivery| delivery.run)
                    .collect();
                print_json(&json!({"runs": runs}))?;
            }
            Action::Runs { run_id: Some(id) } => {
                let id = RunId::new(id).map_err(CompassError::Validation)?;
                print_json(&store.get(&scope, &id).await?.run)?;
            }
            Action::Result { run_id, artifact } => {
                let id = RunId::new(run_id).map_err(CompassError::Validation)?;
                let bytes = match artifact {
                    Some(name) => store.artifact_bytes(&scope, &id, name).await?,
                    None => store.result_bytes(&scope, &id).await?,
                };
                io::stdout()
                    .lock()
                    .write_all(&bytes)
                    .map_err(output_error)?;
            }
            _ => unreachable!("offline command was checked above"),
        }
        Ok(ExitCode::SUCCESS)
    }
}

fn segment(name: &str) -> CompassResult<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(CompassError::Validation(
            "invalid run ID or artifact path component".into(),
        ));
    }
    Ok(name)
}

fn print_json(value: &impl serde::Serialize) -> CompassResult<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value)
        .map_err(|e| CompassError::Storage(format!("write JSON output: {e}")))?;
    writeln!(out).map_err(output_error)
}

fn output_error(error: io::Error) -> CompassError {
    CompassError::Storage(format!("write workspace output: {error}"))
}
