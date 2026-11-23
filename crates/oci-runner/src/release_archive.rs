//! Verify the published OCI graph before handing any image bytes to an engine.
use super::{EngineKind, OciContainerRunner};
use biocompass_core_traits::{CompassError, CompassResult, OciImageRef};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const INDEX: &str = "application/vnd.oci.image.index.v1+json";
const LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const SCHEMA2: &str = "application/vnd.docker.distribution.manifest.v2+json";
const MAX_METADATA: u64 = 8 * 1024 * 1024;

fn invalid(message: impl ToString) -> CompassError {
    CompassError::IntegrityViolation(format!("OCI release archive: {}", message.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    media_type: String,
    digest: String,
    size: u64,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
    #[serde(default)]
    platform: Option<Platform>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct Platform {
    os: String,
    architecture: String,
    #[serde(default)]
    variant: String,
}

impl Platform {
    fn matches(&self, other: &Self) -> bool {
        self.os == other.os
            && self.architecture == other.architecture
            && (self.variant.is_empty()
                || other.variant.is_empty()
                || self.variant == other.variant)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Document {
    schema_version: u64,
    media_type: Option<String>,
    manifests: Option<Vec<Descriptor>>,
    layers: Option<Vec<Descriptor>>,
    config: Option<Descriptor>,
    subject: Option<Descriptor>,
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn pinned_image(reference: &str) -> CompassResult<(String, String)> {
    let image = OciImageRef::parse(reference, false)?;
    let repo = &image.repository;
    if reference != image.raw
        || !repo.as_bytes()[0].is_ascii_alphanumeric()
        || repo
            .bytes()
            .any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && !b".:/_-[]".contains(&c))
        || repo
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || (repo.contains('/') && repo.rsplit('/').next().is_some_and(|s| s.contains(':')))
    {
        return Err(invalid("invalid canonical repository name"));
    }
    let hash = image
        .sha256_digest
        .ok_or_else(|| invalid("missing published image digest"))?;
    Ok((
        format!("sha256:{hash}"),
        format!("{repo}:biocompass-{hash}"),
    ))
}

#[derive(Clone, Copy)]
struct Member {
    offset: u64,
    size: u64,
}

fn read_json<T: serde::de::DeserializeOwned>(file: &mut File, member: Member) -> CompassResult<T> {
    if member.size > MAX_METADATA {
        return Err(invalid("oversized OCI JSON document"));
    }
    file.seek(SeekFrom::Start(member.offset)).map_err(invalid)?;
    serde_json::from_reader(file.take(member.size)).map_err(invalid)
}

fn scan(file: &mut File) -> CompassResult<BTreeMap<String, Member>> {
    let mut files = BTreeMap::new();
    let mut archive = tar::Archive::new(&mut *file);
    for item in archive.entries().map_err(invalid)?.raw(true) {
        let mut entry = item.map_err(invalid)?;
        let path = std::str::from_utf8(entry.header().path_bytes().as_ref())
            .map_err(invalid)?
            .to_owned();
        let blob = path.strip_prefix("blobs/sha256/");
        if !entry.header().entry_type().is_file()
            || entry.header().link_name_bytes().is_some()
            || !(path == "index.json" || path == "oci-layout" || blob.is_some_and(valid_hash))
        {
            return Err(invalid(format!("unexpected archive entry '{path}'")));
        }
        let member = Member {
            offset: entry.raw_file_position(),
            size: entry.size(),
        };
        if files.len() >= 16_384 || files.insert(path.clone(), member).is_some() {
            return Err(invalid("duplicate or excessive archive entries"));
        }
        if let Some(digest) = blob {
            let mut hasher = Sha256::new();
            let bytes = std::io::copy(&mut entry, &mut hasher).map_err(invalid)?;
            if bytes != member.size || hex::encode(hasher.finalize()) != digest {
                return Err(invalid(format!(
                    "content differs from blob address {digest}"
                )));
            }
        }
    }
    // Reject hidden concatenated tar archives. Standard tar padding contains only NUL bytes.
    let mut buffer = [0; 8192];
    loop {
        let n = file.read(&mut buffer).map_err(invalid)?;
        if n == 0 {
            break;
        }
        if buffer[..n].iter().any(|&b| b != 0) {
            return Err(invalid("nonzero bytes after end of archive"));
        }
    }
    Ok(files)
}

struct Graph<'a> {
    file: &'a mut File,
    files: &'a BTreeMap<String, Member>,
    referenced: BTreeSet<String>,
    configs: BTreeMap<String, Vec<Platform>>,
}

impl Graph<'_> {
    fn member(&mut self, d: &Descriptor) -> CompassResult<Member> {
        let hash = d
            .digest
            .strip_prefix("sha256:")
            .filter(|v| valid_hash(v))
            .ok_or_else(|| invalid("invalid referenced digest"))?;
        let member = *self
            .files
            .get(&format!("blobs/sha256/{hash}"))
            .ok_or_else(|| invalid(format!("missing blob {}", d.digest)))?;
        if member.size != d.size || d.media_type.is_empty() {
            return Err(invalid("invalid descriptor size or media type"));
        }
        self.referenced.insert(d.digest.clone());
        Ok(member)
    }

    fn visit(&mut self, d: &Descriptor, depth: usize) -> CompassResult<Vec<Platform>> {
        if depth > 16 {
            return Err(invalid("excessive manifest nesting"));
        }
        let member = self.member(d)?;
        let key = format!("{}:{}", d.media_type, d.digest);
        if let Some(platforms) = self.configs.get(&key) {
            return Ok(platforms.clone());
        }
        let doc: Document = read_json(self.file, member)?;
        if doc.schema_version != 2 || doc.media_type.as_ref().is_some_and(|m| m != &d.media_type) {
            return Err(invalid(
                "manifest schema or media type differs from descriptor",
            ));
        }
        if let Some(subject) = &doc.subject {
            self.visit(subject, depth + 1)?;
        }
        let mut platforms = Vec::new();
        match d.media_type.as_str() {
            INDEX | LIST => {
                if doc.config.is_some() || doc.layers.is_some() {
                    return Err(invalid("unexpected config/layers in index"));
                }
                for child in doc
                    .manifests
                    .ok_or_else(|| invalid("index missing manifests"))?
                {
                    let available = self.visit(&child, depth + 1)?;
                    if let Some(advertised) = child.platform {
                        if !available.is_empty() {
                            if !available.iter().any(|p| advertised.matches(p)) {
                                return Err(invalid(
                                    "descriptor platform differs from image config",
                                ));
                            }
                            platforms.push(advertised);
                        }
                    } else {
                        platforms.extend(available);
                    }
                }
            }
            MANIFEST | SCHEMA2 => {
                if doc.manifests.is_some() {
                    return Err(invalid("unexpected manifests in image"));
                }
                let config = doc.config.ok_or_else(|| invalid("image missing config"))?;
                let cm = self.member(&config)?;
                if matches!(
                    config.media_type.as_str(),
                    "application/vnd.oci.image.config.v1+json"
                        | "application/vnd.docker.container.image.v1+json"
                ) {
                    platforms.push(read_json::<Platform>(self.file, cm)?);
                }
                for layer in doc.layers.ok_or_else(|| invalid("image missing layers"))? {
                    self.member(&layer)?;
                }
            }
            _ => return Err(invalid("unsupported root or child manifest media type")),
        }
        platforms.sort();
        platforms.dedup();
        self.configs.insert(key, platforms.clone());
        Ok(platforms)
    }
}

fn verify(file: &mut File, reference: &str) -> CompassResult<Vec<Platform>> {
    let (digest, tag) = pinned_image(reference)?;
    let files = scan(file)?;
    let metadata = |name: &str| {
        files
            .get(name)
            .copied()
            .ok_or_else(|| invalid(format!("missing {name}")))
    };
    let layout: Value = read_json(file, metadata("oci-layout")?)?;
    if layout["imageLayoutVersion"] != "1.0.0" {
        return Err(invalid("invalid OCI layout version"));
    }
    let index: Document = read_json(file, metadata("index.json")?)?;
    let Some(roots) = index.manifests else {
        return Err(invalid("missing import index descriptors"));
    };
    let [root] = roots.as_slice() else {
        return Err(invalid("expected one published root"));
    };
    if index.schema_version != 2
        || index.media_type.as_deref().is_some_and(|m| m != INDEX)
        || index.layers.is_some()
        || index.config.is_some()
        || index.subject.is_some()
        || root.digest != digest
        || root.annotations.get("org.opencontainers.image.ref.name") != Some(&tag)
        || root.annotations.get("io.containerd.image.name") != Some(&tag)
    {
        return Err(invalid(
            "archive root or import name differs from pinned image",
        ));
    }
    let mut graph = Graph {
        file,
        files: &files,
        referenced: BTreeSet::new(),
        configs: BTreeMap::new(),
    };
    let platforms = graph.visit(root, 0)?;
    if graph.referenced.len() + 2 != files.len() {
        return Err(invalid("unreferenced blob in archive"));
    }
    file.rewind().map_err(invalid)?;
    Ok(platforms)
}

impl OciContainerRunner {
    async fn local_platform(&self) -> CompassResult<Platform> {
        let args = match self.engine.kind {
            EngineKind::Docker => ["version", "--format", "{{.Server.Os}}/{{.Server.Arch}}"],
            EngineKind::Podman => ["info", "--format", "{{.Host.OS}}/{{.Host.Arch}}"],
        };
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            Command::new(&self.engine.binary_path)
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| CompassError::ContainerEngine("engine platform lookup timed out".into()))?
        .map_err(|e| CompassError::ContainerEngine(e.to_string()))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let (os, architecture) = text
            .trim()
            .split_once('/')
            .filter(|(os, arch)| out.status.success() && !os.is_empty() && !arch.is_empty())
            .ok_or_else(|| {
                CompassError::ContainerEngine("could not determine engine OS/architecture".into())
            })?;
        Ok(Platform {
            os: os.into(),
            architecture: architecture.into(),
            variant: String::new(),
        })
    }

    async fn inspect_release(&self, reference: &str) -> Option<Platform> {
        pinned_image(reference).ok()?;
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            Command::new(&self.engine.binary_path)
                .args(["image", "inspect", reference])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        if !out.status.success() {
            return None;
        }
        let images: Vec<Value> = serde_json::from_slice(&out.stdout).ok()?;
        let [image] = images.as_slice() else {
            return None;
        };
        if !image["RepoDigests"]
            .as_array()?
            .iter()
            .any(|s| s.as_str() == Some(reference))
        {
            return None;
        }
        Some(Platform {
            os: image["Os"].as_str()?.into(),
            architecture: image["Architecture"].as_str()?.into(),
            variant: image["Variant"].as_str().unwrap_or_default().into(),
        })
    }

    /// Look up the exact repository and immutable digest in this engine without registry traffic.
    pub async fn has_verified_local_digest(&self, image_ref: &str) -> bool {
        let Some(image) = self.inspect_release(image_ref).await else {
            return false;
        };
        self.local_platform()
            .await
            .is_ok_and(|native| image.matches(&native))
    }

    /// Verify and load a release archive into the active engine. Nothing is extracted on the host.
    pub async fn import_release_image(&self, image_ref: &str, archive: &Path) -> CompassResult<()> {
        let path = archive.to_owned();
        let reference = image_ref.to_owned();
        let (file, platforms) = tokio::task::spawn_blocking(move || {
            let mut file = File::open(path).map_err(|e| CompassError::Storage(e.to_string()))?;
            let platforms = verify(&mut file, &reference)?;
            Ok::<_, CompassError>((file, platforms))
        })
        .await
        .map_err(|e| CompassError::Internal(e.to_string()))??;
        let native = self.local_platform().await?;
        if !platforms.iter().any(|p| p.matches(&native)) {
            return Err(invalid(format!(
                "archive does not contain {}/{}",
                native.os, native.architecture
            )));
        }
        let out = tokio::time::timeout(
            Duration::from_secs(600),
            Command::new(&self.engine.binary_path)
                .arg("load")
                .stdin(Stdio::from(file))
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| CompassError::ContainerEngine("image load timed out".into()))?
        .map_err(|e| CompassError::ContainerEngine(e.to_string()))?;
        if !out.status.success() {
            return Err(CompassError::ContainerEngine(format!(
                "image load failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let found = self.inspect_release(image_ref).await
            .ok_or_else(|| invalid("engine did not retain the published repository digest; a digest-preserving image store is required"))?;
        if !found.matches(&native) || !platforms.iter().any(|p| p.matches(&found)) {
            return Err(invalid(
                "loaded image platform differs from its verified archive",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "release_archive_tests.rs"]
mod tests;
