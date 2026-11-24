//! Standard OCI Distribution Spec v1.1 (`/v2/`) manifest client verifying `@sha256` digests cryptographically.

use async_trait::async_trait;
use biocompass_core_traits::{CompassError, CompassResult, OciImageRef, OciRegistryClient};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

const ACCEPT_OCI_MANIFESTS: &str = concat!(
    "application/vnd.oci.image.manifest.v1+json, ",
    "application/vnd.oci.image.index.v1+json, ",
    "application/vnd.docker.distribution.manifest.v2+json, ",
    "application/vnd.docker.distribution.manifest.list.v2+json"
);

/// Hard upper bound (`4 MiB`) on OCI manifest responses (`SEC-23`).
pub const MAX_OCI_MANIFEST_BYTES: usize = 4 * 1024 * 1024;

/// Check whether an IP address belongs to a disallowed SSRF range (`SEC-23`).
/// Loopback (`127.0.0.0/8`, `::1`) is allowed ONLY when `allow_loopback` is `true`.
#[must_use]
pub fn is_disallowed_registry_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => is_disallowed_v4(v4, allow_loopback),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_disallowed_v4(mapped, allow_loopback);
            }
            if v6.is_loopback() {
                return !allow_loopback;
            }
            is_disallowed_v6(v6)
        }
    }
}

fn is_disallowed_v4(ip: Ipv4Addr, allow_loopback: bool) -> bool {
    let o = ip.octets();
    if ip.is_loopback() {
        return !allow_loopback;
    }
    ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        // 0.0.0.0/8 ("this network")
        || o[0] == 0
        // 100.64.0.0/10 (Carrier-Grade NAT / Tailscale / internal cloud VPCs)
        || (o[0] == 100 && (o[1] & 0b1100_0000) == 64)
        // 192.0.0.0/24, 198.18.0.0/15 (benchmarking), 240.0.0.0/4 (reserved)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
        || o[0] >= 240
}

fn is_disallowed_v6(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();
    ip.is_unspecified()
        || ip.is_multicast()
        // Unique Local Addresses (fc00::/7)
        || (seg[0] & 0xfe00) == 0xfc00
        // Link-local unicast (fe80::/10)
        || (seg[0] & 0xffc0) == 0xfe80
        // Deprecated site-local (fec0::/10)
        || (seg[0] & 0xffc0) == 0xfec0
        // Documentation (2001:db8::/32)
        || (seg[0] == 0x2001 && seg[1] == 0x0db8)
}

/// Check that the HTTP `Content-Type` header matches an allowed OCI/Docker manifest media type.
#[must_use]
pub fn is_valid_oci_manifest_content_type(raw: Option<&str>) -> bool {
    let Some(header) = raw else {
        return false;
    };
    let media_type = header
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        media_type.as_str(),
        "application/vnd.oci.image.manifest.v1+json"
            | "application/vnd.oci.image.index.v1+json"
            | "application/vnd.docker.distribution.manifest.v2+json"
            | "application/vnd.docker.distribution.manifest.list.v2+json"
            | "application/vnd.docker.distribution.manifest.v1+json"
            | "application/vnd.docker.distribution.manifest.v1+prettyjws"
    )
}

/// Cloud-agnostic OCI Distribution Spec v1.1 HTTP client (works with Zot, Harbor, `registry:2`, GHCR, Scaleway, Exoscale, etc.).
#[derive(Debug, Clone)]
pub struct OciDistributionClient {
    http: Client,
    allow_loopback_http: bool,
    loopback_https: bool,
}

impl OciDistributionClient {
    /// Construct a new OCI Distribution v1.1 client with strict no-redirect & timeout hardening (`SEC-23`).
    pub fn new(allow_loopback_http: bool) -> CompassResult<Self> {
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| CompassError::ContainerEngine(format!("build OCI HTTP client: {e}")))?;
        Ok(Self {
            http,
            allow_loopback_http,
            loopback_https: false,
        })
    }

    /// Construct an OCI Distribution v1.1 client that talks strictly over HTTPS to loopback
    /// registries verified against the supplied PEM CA certificate (used for self-contained
    /// TLS loopback test registries without weakening TLS validation or SSRF rules).
    pub fn with_loopback_tls_ca_pem(ca_cert_pem: &[u8]) -> CompassResult<Self> {
        let certs = reqwest::Certificate::from_pem_bundle(ca_cert_pem).map_err(|e| {
            CompassError::Validation(format!("invalid loopback registry CA PEM certificate: {e}"))
        })?;
        if certs.is_empty() {
            return Err(CompassError::Validation(
                "loopback registry CA PEM bundle contains no certificates".into(),
            ));
        }
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20));
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
        let http = builder
            .build()
            .map_err(|e| CompassError::ContainerEngine(format!("build OCI TLS HTTP client: {e}")))?;
        Ok(Self {
            http,
            allow_loopback_http: true,
            loopback_https: true,
        })
    }

    fn split_registry_and_repo<'a>(
        &self,
        repository: &'a str,
    ) -> CompassResult<(String, String, u16, bool, &'a str)> {
        let (host_authority, repo_path) = repository.split_once('/').ok_or_else(|| {
            CompassError::Validation(format!(
                "OCI repository '{repository}' must include explicit registry host (e.g. registry.example/org/image)"
            ))
        })?;
        // Strict authority parsing via URL parser (rejects userinfo, query, fragment, trailing junk).
        let parsed_origin = reqwest::Url::parse(&format!("https://{host_authority}/")).map_err(|_| {
            CompassError::Validation(format!("invalid OCI registry host '{host_authority}'"))
        })?;
        if !parsed_origin.username().is_empty()
            || parsed_origin.password().is_some()
            || parsed_origin.query().is_some()
            || parsed_origin.fragment().is_some()
            || parsed_origin.path() != "/"
        {
            return Err(CompassError::Validation(format!(
                "OCI registry host '{host_authority}' must not include userinfo, path, query, or fragment"
            )));
        }
        let host_str = parsed_origin
            .host_str()
            .ok_or_else(|| CompassError::Validation(format!("missing OCI registry host in '{repository}'")))?;
        let unbracketed = host_str.trim_start_matches('[').trim_end_matches(']');

        let is_loopback = if let Ok(ip) = unbracketed.parse::<IpAddr>() {
            if is_disallowed_registry_ip(ip, self.allow_loopback_http) {
                return Err(CompassError::Validation(format!(
                    "OCI registry IP '{host_authority}' is blocked by SSRF policy"
                )));
            }
            ip.is_loopback()
        } else {
            // Exact match ONLY — never `starts_with("localhost")` which matches `localhost.attacker.tld`!
            unbracketed.eq_ignore_ascii_case("localhost")
        };

        let scheme = if is_loopback {
            if !self.allow_loopback_http {
                return Err(CompassError::Validation(format!(
                    "loopback HTTP registry '{host_authority}' is disabled in strict TLS policy"
                )));
            }
            if self.loopback_https { "https" } else { "http" }
        } else {
            "https"
        };
        let port = parsed_origin
            .port()
            .unwrap_or(if scheme == "http" { 80 } else { 443 });

        Ok((
            format!("{scheme}://{host_authority}"),
            unbracketed.to_owned(),
            port,
            is_loopback,
            repo_path,
        ))
    }

    /// Resolve host DNS before connecting and reject any private/link-local/loopback SSRF target (`SEC-23`).
    async fn verify_dns_target(
        &self,
        host: &str,
        port: u16,
        declared_loopback: bool,
    ) -> CompassResult<()> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_disallowed_registry_ip(ip, self.allow_loopback_http && declared_loopback) {
                return Err(CompassError::Validation(format!(
                    "OCI registry IP '{host}' is blocked by SSRF policy"
                )));
            }
            return Ok(());
        }
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| CompassError::ContainerEngine(format!("resolve OCI registry '{host}': {e}")))?;
        let mut saw_any = false;
        for addr in addrs {
            saw_any = true;
            if is_disallowed_registry_ip(addr.ip(), self.allow_loopback_http && declared_loopback) {
                return Err(CompassError::Validation(format!(
                    "OCI registry host '{host}' resolved to disallowed IP address"
                )));
            }
        }
        if !saw_any {
            return Err(CompassError::ContainerEngine(format!(
                "OCI registry host '{host}' resolved to zero IP addresses"
            )));
        }
        Ok(())
    }

    /// Fetch the manifest for `image_raw` (`<registry>/<repo>@sha256:<hex>`) and verify `SHA256(body) == digest`.
    pub async fn fetch_and_verify_manifest(&self, image_raw: &str) -> CompassResult<OciImageRef> {
        let parsed = OciImageRef::parse(image_raw, false)?;
        let expected_digest = parsed
            .sha256_digest
            .as_deref()
            .ok_or_else(|| CompassError::Validation("missing @sha256 digest".into()))?;

        let (base_url, host, port, declared_loopback, repo_path) =
            self.split_registry_and_repo(&parsed.repository)?;
        self.verify_dns_target(&host, port, declared_loopback).await?;
        let url = format!("{base_url}/v2/{repo_path}/manifests/sha256:{expected_digest}");

        let mut resp = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, ACCEPT_OCI_MANIFESTS)
            .send()
            .await
            .map_err(|e| {
                CompassError::ContainerEngine(format!("GET OCI manifest {url} failed: {e}"))
            })?;

        if resp.status().is_redirection() {
            return Err(CompassError::Validation(format!(
                "OCI registry manifest endpoint returned forbidden HTTP redirect ({}) for {url}",
                resp.status()
            )));
        }

        if !resp.status().is_success() {
            return Err(CompassError::NotFound(format!(
                "OCI registry returned HTTP {} for {url}",
                resp.status()
            )));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());
        if !is_valid_oci_manifest_content_type(content_type) {
            return Err(CompassError::IntegrityViolation(format!(
                "OCI registry returned unexpected Content-Type '{}' for {url}",
                content_type.unwrap_or("<missing>")
            )));
        }

        if let Some(len) = resp.content_length()
            && len > MAX_OCI_MANIFEST_BYTES as u64
        {
            return Err(CompassError::IntegrityViolation(format!(
                "OCI manifest response exceeded {MAX_OCI_MANIFEST_BYTES} byte limit for {url}"
            )));
        }

        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| {
            CompassError::ContainerEngine(format!("read OCI manifest body {url}: {e}"))
        })? {
            if body.len().saturating_add(chunk.len()) > MAX_OCI_MANIFEST_BYTES {
                return Err(CompassError::IntegrityViolation(format!(
                    "OCI manifest response exceeded {MAX_OCI_MANIFEST_BYTES} byte limit for {url}"
                )));
            }
            body.extend_from_slice(&chunk);
        }

        let actual_digest = hex::encode(Sha256::digest(&body));
        if actual_digest != expected_digest {
            // SEC-23: Never leak `actual_digest` in the error message (eliminates SHA-256 oracle).
            return Err(CompassError::IntegrityViolation(format!(
                "OCI registry manifest digest mismatch for {url}: expected sha256:{expected_digest}"
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn sec_23_ssrf_blocks_private_link_local_loopback_and_localhost_subdomain() {
        let strict = OciDistributionClient::new(false).unwrap();
        let loopback_allowed = OciDistributionClient::new(true).unwrap();

        // Private / link-local / ULA / unspecified / IPv4-mapped blocked even when loopback is enabled.
        for bad_host in [
            "169.254.169.254",
            "10.0.0.1",
            "172.16.0.5:5000",
            "192.168.1.10:5000",
            "100.64.0.1:5000",
            "0.0.0.0:5000",
            "[fe80::1]:5000",
            "[fc00::1]:5000",
            "[::ffff:169.254.169.254]:80",
        ] {
            let repo = format!("{bad_host}/org/img");
            assert!(
                strict.split_registry_and_repo(&repo).is_err(),
                "strict must reject {repo}"
            );
            assert!(
                loopback_allowed.split_registry_and_repo(&repo).is_err(),
                "loopback_allowed must still reject private/link-local {repo}"
            );
        }

        // Loopback rejected in strict mode, permitted in loopback mode.
        for lb in ["127.0.0.1:5500/org/img", "localhost:5000/org/img", "[::1]:5000/org/img"] {
            assert!(strict.split_registry_and_repo(lb).is_err(), "strict must reject {lb}");
            let (base, _, _, is_lb, _) = loopback_allowed
                .split_registry_and_repo(lb)
                .expect("loopback mode allows exact loopback");
            assert!(base.starts_with("http://"));
            assert!(is_lb);
        }

        // `localhost.attacker.tld` MUST NOT be treated as loopback HTTP!
        let (base, _, _, is_lb, _) = loopback_allowed
            .split_registry_and_repo("localhost.attacker.tld/org/img")
            .expect("external hostname uses https");
        assert_eq!(base, "https://localhost.attacker.tld");
        assert!(!is_lb);
    }

    async fn serve_single_http_response(raw_response: Vec<u8>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut req_buf = [0u8; 2048];
                let _ = stream.read(&mut req_buf).await;
                let _ = stream.write_all(&raw_response).await;
                let _ = stream.shutdown().await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn sec_23_blocks_redirects_oversize_bodies_bad_content_type_and_sha256_oracle() {
        let client = OciDistributionClient::new(true).unwrap();
        let fake_sha = "a".repeat(64);

        // 1. Redirect (302) MUST NOT be followed!
        let redir_addr = serve_single_http_response(
            b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/\r\nContent-Length: 0\r\n\r\n".to_vec(),
        )
        .await;
        let err = client
            .fetch_and_verify_manifest(&format!("{redir_addr}/org/img@sha256:{fake_sha}"))
            .await
            .expect_err("redirect must fail");
        assert!(err.to_string().contains("redirect"), "unexpected err: {err}");

        // 2. Non-OCI Content-Type (e.g. text/html) MUST be rejected.
        let html_addr = serve_single_http_response(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 10\r\n\r\n<html>ok!>".to_vec(),
        )
        .await;
        let err = client
            .fetch_and_verify_manifest(&format!("{html_addr}/org/img@sha256:{fake_sha}"))
            .await
            .expect_err("non-OCI content-type must fail");
        assert!(err.to_string().contains("Content-Type"), "unexpected err: {err}");

        // 3. Digest mismatch MUST NOT reveal computed SHA-256 (no oracle!).
        let body = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json"}"#;
        let real_sha = hex::encode(Sha256::digest(body));
        let mut mismatch_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.oci.image.manifest.v1+json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        mismatch_resp.extend_from_slice(body);
        let mismatch_addr = serve_single_http_response(mismatch_resp).await;
        let err = client
            .fetch_and_verify_manifest(&format!("{mismatch_addr}/org/img@sha256:{fake_sha}"))
            .await
            .expect_err("mismatch must fail");
        assert!(
            !err.to_string().contains(&real_sha),
            "SEC-23 SHA-256 oracle leaked actual digest: {err}"
        );

        // 4. Oversize manifest (> 4 MiB) MUST be rejected before OOM.
        let big_len = MAX_OCI_MANIFEST_BYTES + 1024;
        let big_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.oci.image.manifest.v1+json\r\nContent-Length: {big_len}\r\n\r\n"
        )
        .into_bytes();
        let big_addr = serve_single_http_response(big_resp).await;
        let err = client
            .fetch_and_verify_manifest(&format!("{big_addr}/org/img@sha256:{fake_sha}"))
            .await
            .expect_err("oversize manifest must fail");
        assert!(err.to_string().contains("exceeded"), "unexpected err: {err}");

        // 5. Valid manifest with matching digest & OCI Content-Type succeeds.
        let mut ok_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.oci.image.manifest.v1+json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        ok_resp.extend_from_slice(body);
        let ok_addr = serve_single_http_response(ok_resp).await;
        let parsed = client
            .fetch_and_verify_manifest(&format!("{ok_addr}/org/img@sha256:{real_sha}"))
            .await
            .expect("valid manifest must pass");
        assert_eq!(parsed.sha256_digest.as_deref(), Some(real_sha.as_str()));
    }

    #[test]
    fn loopback_tls_client_uses_https_and_rejects_invalid_pem_or_ssrf() {
        assert!(OciDistributionClient::with_loopback_tls_ca_pem(b"not-a-cert").is_err());
        let mut tls_client = OciDistributionClient::new(true).unwrap();
        tls_client.loopback_https = true;
        let (base, _, _, is_lb, _) = tls_client
            .split_registry_and_repo("127.0.0.1:5000/org/img")
            .expect("loopback TLS client accepts loopback");
        assert_eq!(base, "https://127.0.0.1:5000");
        assert!(is_lb);
        assert!(tls_client.split_registry_and_repo("169.254.169.254/org/img").is_err());
    }
}
