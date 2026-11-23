//! Discover a maintainer-configured static update feed and serve only verified release files.
use crate::{server::ApiResult, state::AppState};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use biocompass_core_traits::CompassError;
use futures::stream;
use minisign_verify::{PublicKey, Signature};
use reqwest::{Client, Url};
use semver::Version;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, env, net::IpAddr, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

static DOWNLOAD_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
const MAX_ARTIFACT: u64 = 1024 * 1024 * 1024;
const TARGETS: &[&str] = &[
    "darwin-aarch64",
    "darwin-x86_64",
    "linux-aarch64",
    "linux-x86_64",
    "windows-aarch64",
    "windows-x86_64",
];

#[derive(Deserialize)]
pub(crate) struct Selection {
    target: Option<String>,
    pin: Option<String>,
    product: Option<Product>,
}
#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Product {
    App,
    FrogletNode,
}
#[derive(Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    platforms: BTreeMap<String, Artifact>,
}
#[derive(Deserialize)]
struct Artifact {
    version: Option<String>,
    #[serde(default)]
    notes: String,
    url: String,
    signature: String,
}
struct Channel {
    client: Client,
    manifest: Url,
    key: PublicKey,
}

fn unavailable(message: &str) -> CompassError {
    CompassError::Storage(message.into())
}
fn invalid(message: &str) -> CompassError {
    CompassError::IntegrityViolation(message.into())
}

fn configured() -> Result<Option<Channel>, CompassError> {
    let Ok(value) = env::var("BIOCOMPASS_RELEASE_CHANNEL") else {
        return Ok(None);
    };
    let manifest =
        Url::parse(&value).map_err(|_| invalid("Invalid local app release channel URL"))?;
    let loopback = manifest
        .host_str()
        .and_then(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    if !(manifest.scheme() == "https" || manifest.scheme() == "http" && loopback)
        || !manifest.username().is_empty()
        || manifest.password().is_some()
        || manifest.query().is_some()
        || manifest.fragment().is_some()
        || manifest.path().ends_with('/')
    {
        return Err(invalid(
            "The release channel must be an HTTPS manifest URL or a loopback test URL",
        ));
    }
    let public = env::var("BIOCOMPASS_RELEASE_PUBLIC_KEY")
        .map_err(|_| invalid("Release signing key has not been configured"))?;
    let key = PublicKey::from_base64(public.trim())
        .map_err(|_| invalid("Invalid release signing key"))?;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| unavailable("Cannot initialize release channel"))?;
    Ok(Some(Channel {
        client,
        manifest,
        key,
    }))
}

async fn bounded(client: &Client, url: Url, max: usize) -> Result<Vec<u8>, CompassError> {
    let mut response = client
        .get(url)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|_| unavailable("Release channel is unavailable"))?;
    if !response.status().is_success() {
        return Err(unavailable("Release channel did not return a file"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| unavailable("Release metadata download was interrupted"))?
    {
        if chunk.len() > max - body.len() {
            return Err(invalid("Release metadata is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl Channel {
    async fn release(&self) -> Result<(Manifest, Version, String), CompassError> {
        let mut signature_url = self.manifest.clone();
        signature_url.set_path(&format!("{}.minisig", signature_url.path()));
        let (body, sig) = tokio::try_join!(
            bounded(&self.client, self.manifest.clone(), 256 * 1024),
            bounded(&self.client, signature_url, 4096)
        )?;
        let sig = std::str::from_utf8(&sig)
            .ok()
            .and_then(|v| Signature::decode(v).ok())
            .ok_or_else(|| invalid("Invalid release metadata signature"))?;
        self.key
            .verify(&body, &sig, false)
            .map_err(|_| invalid("Release metadata signature verification failed"))?;
        let release: Manifest =
            serde_json::from_slice(&body).map_err(|_| invalid("Invalid release metadata"))?;
        let version = Version::parse(release.version.trim_start_matches('v'))
            .map_err(|_| invalid("Invalid release version"))?;
        if !version.pre.is_empty() {
            return Err(invalid("A stable channel must publish a stable version"));
        }
        Ok((release, version, hex::encode(Sha256::digest(body))))
    }
    fn artifact(&self, item: &Artifact) -> Result<(Url, String, Signature), CompassError> {
        let url = self
            .manifest
            .join(&item.url)
            .map_err(|_| invalid("Invalid release file URL"))?;
        if url.origin() != self.manifest.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid(
                "Release files must share the configured channel origin",
            ));
        }
        let name = url
            .path_segments()
            .and_then(|mut p| p.next_back())
            .unwrap_or("");
        if name.is_empty()
            || name.len() > 180
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(invalid("Invalid release file name"));
        }
        let file = name.to_owned();
        let signature = decode_artifact_signature(&item.signature)?;
        Ok((url, file, signature))
    }
}

fn decode_artifact_signature(value: &str) -> Result<Signature, CompassError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let decoded;
    let text = if value.starts_with("untrusted comment:") {
        value
    } else {
        decoded = STANDARD
            .decode(value.trim())
            .map_err(|_| invalid("Invalid release file signature"))?;
        std::str::from_utf8(&decoded).map_err(|_| invalid("Invalid release file signature"))?
    };
    Signature::decode(text).map_err(|_| invalid("Invalid release file signature"))
}

fn native_target() -> String {
    let os = match env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{os}-{}", env::consts::ARCH)
}
fn target(state: &AppState, q: &Selection) -> Result<Option<String>, CompassError> {
    let t = if state.hub_client.is_none() {
        Some(native_target())
    } else {
        q.target.clone().filter(|value| !value.is_empty())
    };
    if t.as_ref().is_some_and(|s| !TARGETS.contains(&s.as_str())) {
        return Err(CompassError::Validation("Unsupported platform".into()));
    }
    Ok(t)
}
/// The home hub supplies the role, including when the workbench relays a browser session.
async fn node_download_allowed(
    state: &AppState,
    headers: &HeaderMap,
) -> ApiResult<Result<bool, Response>> {
    let Some(hub) = &state.hub_client else {
        return Ok(Ok(false));
    };
    let response = hub
        .proxy_raw(
            reqwest::Method::GET,
            "/api/v1/auth/whoami",
            headers,
            Vec::new(),
        )
        .await?;
    if !response.status().is_success() {
        return Ok(Err(response));
    }
    #[derive(Deserialize)]
    struct Caller {
        role: String,
        is_platform_admin: bool,
    }
    let bytes = axum::body::to_bytes(response.into_body(), 16384)
        .await
        .map_err(|_| unavailable("Cannot read the signed-in identity"))?;
    let caller: Caller = serde_json::from_slice(&bytes)
        .map_err(|_| unavailable("Cannot read the signed-in role"))?;
    Ok(Ok(
        caller.is_platform_admin || matches!(caller.role.as_str(), "admin" | "owner")
    ))
}

impl Channel {
    async fn published_name(&self, artifact: &Artifact) -> Result<String, CompassError> {
        let (url, name, signature) = self.artifact(artifact)?;
        self.key
            .verify_stream(&signature)
            .map_err(|_| invalid("Release signature uses an unexpected signing key"))?;
        let published = self
            .client
            .head(url)
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|_| unavailable("Cannot check the published release file"))?;
        if !published.status().is_success() {
            return Err(unavailable("The signed release file is unavailable"));
        }
        if published.content_length().is_some_and(|n| n > MAX_ARTIFACT) {
            return Err(invalid("Release file is too large"));
        }
        Ok(name)
    }
}
fn node_version(artifact: &Artifact) -> Result<Version, CompassError> {
    let version = artifact
        .version
        .as_deref()
        .and_then(|v| Version::parse(v).ok())
        .filter(|v| v.pre.is_empty());
    version.ok_or_else(|| invalid("A customer node artifact needs its own stable version"))
}

pub(crate) async fn discover(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<Selection>,
) -> ApiResult<Response> {
    let can_download_node = match node_download_allowed(&state, &headers).await? {
        Ok(allowed) => allowed,
        Err(response) => return Ok(response),
    };
    let installed = state
        .hub_client
        .is_none()
        .then_some(env!("CARGO_PKG_VERSION"));
    let requested = target(&state, &q)?;
    let mut out = json!({"state":"unconfigured","current_version":installed,"target":requested});
    let Some(channel) = configured()? else {
        return Ok(Json(out).into_response());
    };
    let Some(target) = requested else {
        out["state"] = json!("choose_target");
        return Ok(Json(out).into_response());
    };
    let (release, version, pin) = channel.release().await?;
    out["pin"] = json!(pin);
    if can_download_node
        && let Some(node) = release.platforms.get(&format!("froglet-node-{target}"))
    {
        match (node_version(node), channel.published_name(node).await) {
            (Ok(version), Ok(name)) => {
                out["node"] =
                    json!({"version":version.to_string(),"filename":name,"notes":node.notes})
            }
            _ => tracing::warn!(%target,"configured customer node release is unavailable"),
        }
    }
    let Some(artifact) = release.platforms.get(&target) else {
        out["state"] = json!("unavailable");
        return Ok(([(header::CACHE_CONTROL, "private, no-store")], Json(out)).into_response());
    };
    let name = channel.published_name(artifact).await?;
    let current =
        Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo package version is semantic");
    out["state"] = json!(if installed.is_none() {
        "download"
    } else if version.cmp_precedence(&current).is_gt() {
        "update"
    } else {
        "current"
    });
    out["version"] = json!(version.to_string());
    out["notes"] = json!(release.notes);
    out["filename"] = json!(name);
    out["pin"] = json!(pin);
    Ok(([(header::CACHE_CONTROL, "private, no-store")], Json(out)).into_response())
}

pub(crate) async fn download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<Selection>,
) -> ApiResult<Response> {
    let can_download_node = match node_download_allowed(&state, &headers).await? {
        Ok(allowed) => allowed,
        Err(response) => return Ok(response),
    };
    let node = q.product == Some(Product::FrogletNode);
    if node && !can_download_node {
        return Err(CompassError::AccessDenied(
            "Customer node downloads require an organization administrator".into(),
        )
        .into());
    }
    let channel = configured()?
        .ok_or_else(|| CompassError::NotFound("Release channel is not configured".into()))?;
    let target =
        target(&state, &q)?.ok_or_else(|| CompassError::Validation("Choose a platform".into()))?;
    let (release, version, pin) = channel.release().await?;
    if q.pin.as_deref() != Some(pin.as_str()) {
        return Ok((
            StatusCode::CONFLICT,
            "The release changed; check for updates again.",
        )
            .into_response());
    }
    if !node
        && state.hub_client.is_none()
        && !version
            .cmp_precedence(
                &Version::parse(env!("CARGO_PKG_VERSION"))
                    .expect("Cargo package version is semantic"),
            )
            .is_gt()
    {
        return Err(CompassError::Validation("A newer release is required".into()).into());
    }
    let key = if node {
        format!("froglet-node-{target}")
    } else {
        target
    };
    let artifact = release
        .platforms
        .get(&key)
        .ok_or_else(|| CompassError::NotFound("Release unavailable for this platform".into()))?;
    if node {
        node_version(artifact)?;
    }
    let (url, name, signature) = channel.artifact(artifact)?;
    // Keep disk use bounded while both verifying and serving completed temporary files.
    let Ok(slot) = DOWNLOAD_SLOTS.try_acquire() else {
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            "Other release downloads are active. Please retry shortly.",
        )
            .into_response());
    };
    let mut verifier = channel
        .key
        .verify_stream(&signature)
        .map_err(|_| invalid("Invalid release signature"))?;
    let mut response = channel
        .client
        .get(url)
        .send()
        .await
        .map_err(|_| unavailable("Release file download failed"))?;
    if !response.status().is_success()
        || response.content_length().is_some_and(|n| n > MAX_ARTIFACT)
    {
        return Err(unavailable("Release file unavailable or too large").into());
    }
    // The anonymous temporary file is discarded on failure, client cancellation, or completed response.
    let mut file = tokio::fs::File::from_std(
        tempfile::tempfile().map_err(|_| unavailable("Cannot open temporary release file"))?,
    );
    let mut length = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| unavailable("Release download interrupted; retry the download"))?
    {
        length += chunk.len() as u64;
        if length > MAX_ARTIFACT {
            return Err(invalid("Release file is too large").into());
        }
        verifier.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|_| unavailable("Insufficient space to verify release"))?;
    }
    verifier
        .finalize()
        .map_err(|_| invalid("Release file signature verification failed"))?;
    file.rewind()
        .await
        .map_err(|_| unavailable("Cannot read verified release"))?;
    let body = stream::try_unfold((file, slot), |(mut file, slot)| async move {
        let mut buf = vec![0; 128 * 1024];
        let count = file.read(&mut buf).await?;
        buf.truncate(count);
        Ok::<_, std::io::Error>((count > 0).then_some((Bytes::from(buf), (file, slot))))
    });
    Ok((
        [
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CONTENT_TYPE, "application/octet-stream".into()),
            (header::CONTENT_LENGTH, length.to_string()),
            (header::CACHE_CONTROL, "private, no-store".into()),
        ],
        Body::from_stream(body),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};

    #[test]
    fn release_file_verifies_in_both_manifest_encodings_and_rejects_changed_bytes() {
        // Signed with Minisign 0.12, using a disposable publisher key.
        let public = PublicKey::from_base64(PUB).unwrap();
        for value in [SIG.to_owned(), STANDARD.encode(SIG)] {
            let signature = decode_artifact_signature(&value).unwrap();
            let mut stream = public.verify_stream(&signature).unwrap();
            stream.update(b"BioCompass release ");
            stream.update(b"verification fixture\n");
            stream.finalize().unwrap();
            let mut altered = public.verify_stream(&signature).unwrap();
            altered.update(b"different release file");
            assert!(altered.finalize().is_err());
        }
    }
    const PUB: &str = "RWR28Z/9Kq9gFZzOHTkpOH/PV3lIKWvqJir/UDjP8aJDCUd92u6TAAL2";
    const SIG: &str = "untrusted comment: signature from minisign secret key\nRUR28Z/9Kq9gFdnduMwzVFeS6S7Xt1gPMErS7QSd1cUgPJXHJl9QFEO0ZEms3H02o0OZfZOG5yBiSopDScYvpDDZ8DozNmVImA4=\ntrusted comment: BioCompass offline unit fixture\nVGEm/vQxdUHBpXFs0i0prBKQnGoj9Zx/CVTEgl1+MVdyvTsEg6LYrS3MJ7fehwhAroIuYl/ZPGkFQMIh/p2ZCA==\n";
}
