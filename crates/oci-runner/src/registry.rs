//! Standard OCI Distribution Spec v1.1 (`/v2/`) manifest client verifying `@sha256` digests cryptographically.

use async_trait::async_trait;
use biocompass_core_traits::{CompassError, CompassResult, OciImageRef, OciRegistryClient};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::time::Duration;

const ACCEPT_OCI_MANIFESTS: &str = concat!(
    "application/vnd.oci.image.manifest.v1+json, ",
    "application/vnd.oci.image.index.v1+json, ",
    "application/vnd.docker.distribution.manifest.v2+json, ",
    "application/vnd.docker.distribution.manifest.list.v2+json"
);

/// Cloud-agnostic OCI Distribution Spec v1.1 HTTP client (works with Zot, Harbor, `registry:2`, GHCR, Scaleway, Exoscale, etc.).
#[derive(Debug, Clone)]
pub struct OciDistributionClient {
    http: Client,
    allow_loopback_http: bool,
}

impl OciDistributionClient {
    /// Construct a new OCI Distribution v1.1 client.
    pub fn new(allow_loopback_http: bool) -> CompassResult<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| CompassError::ContainerEngine(format!("build OCI HTTP client: {e}")))?;
        Ok(Self {
            http,
            allow_loopback_http,
        })
    }

    fn split_registry_and_repo<'a>(&self, repository: &'a str) -> CompassResult<(String, &'a str)> {
        let (host, repo_path) = repository.split_once('/').ok_or_else(|| {
            CompassError::Validation(format!(
                "OCI repository '{repository}' must include explicit registry host (e.g. registry.example/org/image)"
            ))
        })?;
        let is_loopback = host.starts_with("127.0.0.1")
            || host.starts_with("localhost")
            || host.starts_with("[::1]");
        let scheme = if is_loopback {
            if !self.allow_loopback_http {
                return Err(CompassError::Validation(format!(
                    "loopback HTTP registry '{host}' is disabled in strict TLS policy"
                )));
            }
            "http"
        } else {
            "https"
        };
        Ok((format!("{scheme}://{host}"), repo_path))
    }

    /// Fetch the manifest for `image_raw` (`<registry>/<repo>@sha256:<hex>`) and verify `SHA256(body) == digest`.
    pub async fn fetch_and_verify_manifest(&self, image_raw: &str) -> CompassResult<OciImageRef> {
        let parsed = OciImageRef::parse(image_raw, false)?;
        let expected_digest = parsed
            .sha256_digest
            .as_deref()
            .ok_or_else(|| CompassError::Validation("missing @sha256 digest".into()))?;

        let (base_url, repo_path) = self.split_registry_and_repo(&parsed.repository)?;
        let url = format!("{base_url}/v2/{repo_path}/manifests/sha256:{expected_digest}");

        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, ACCEPT_OCI_MANIFESTS)
            .send()
            .await
            .map_err(|e| {
                CompassError::ContainerEngine(format!("GET OCI manifest {url} failed: {e}"))
            })?;

        if !resp.status().is_success() {
            return Err(CompassError::NotFound(format!(
                "OCI registry returned HTTP {} for {url}",
                resp.status()
            )));
        }

        let body = resp.bytes().await.map_err(|e| {
            CompassError::ContainerEngine(format!("read OCI manifest body {url}: {e}"))
        })?;
        let actual_digest = hex::encode(Sha256::digest(&body));
        if actual_digest != expected_digest {
            return Err(CompassError::IntegrityViolation(format!(
                "OCI registry manifest digest mismatch for {url}: expected sha256:{expected_digest}, computed sha256:{actual_digest}"
            )));
        }

        Ok(parsed)
    }
}

#[async_trait]
impl OciRegistryClient for OciDistributionClient {
    async fn verify_image(&self, image_raw: &str) -> CompassResult<OciImageRef> {
        self.fetch_and_verify_manifest(image_raw).await
    }
}
