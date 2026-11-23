//! Operator-side staging. Scientific workers never receive a workspace, identity token or publish handle.
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

pub struct Build {
    _temp: TempDir,
    pub rendered: PathBuf,
    pub validation: Value,
}

#[cfg(unix)]
fn container_writable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    // The enclosing TempDir is accessible only to the operator. This child is
    // bind-mounted for the unrelated, unprivileged OCI uid.
    fs::set_permissions(path, fs::Permissions::from_mode(0o777)).map_err(storage)
}
#[cfg(not(unix))]
fn container_writable(_: &Path) -> Result<(), String> {
    Ok(())
}

fn storage(e: impl std::fmt::Display) -> String {
    format!("storage: {e}")
}

pub fn image_id(image: &str) -> Result<&str, String> {
    let hex = image.strip_prefix("sha256:").unwrap_or("");
    if hex.len() == 64
        && hex
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        Ok(image)
    } else {
        Err("invalid: expected an immutable local OCI image ID (sha256:<64 hex>)".into())
    }
}

pub fn archive_hash(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    let mut file = fs::File::open(path).map_err(storage)?;
    std::io::copy(&mut file, &mut digest).map_err(storage)?;
    Ok(hex::encode(digest.finalize()))
}

pub fn source(workspace: &Path, draft: &str) -> Result<PathBuf, String> {
    let sha = draft
        .strip_prefix("sha256-")
        .ok_or("invalid: invalid draft ID")?;
    image_id(&format!("sha256:{sha}"))?;
    let dir = workspace.join("drafts").join(draft);
    let path = dir.join("source.zip");
    if archive_hash(&path)? != sha {
        return Err("conflict: saved source checksum differs from draft ID".into());
    }
    Ok(path)
}

pub fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_reader(fs::File::open(path).map_err(storage)?).map_err(storage)
}

fn run(
    engine: &str,
    image: &str,
    mounts: &[(&Path, &str, bool)],
    entry: Option<&str>,
    args: &[&str],
) -> Result<(), String> {
    image_id(image)?;
    let mut cmd = Command::new(engine);
    cmd.args([
        "run",
        "--rm",
        "--pull=never",
        "--network=none",
        "--read-only",
        "--user",
        "65532:65532",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--pids-limit=96",
        "--cpus=2",
        "--memory=2g",
        "--tmpfs",
        "/tmp:rw,noexec,nosuid,size=1g",
    ]);
    for (path, target, writable) in mounts {
        let host = fs::canonicalize(path).map_err(storage)?;
        let host = host.to_str().ok_or("invalid: unsupported mount path")?;
        if host.contains(',') {
            return Err("invalid: commas in mount paths are unsupported".into());
        }
        let mode = if *writable { "" } else { ",readonly" };
        cmd.args([
            "--mount",
            &format!("type=bind,src={host},dst={target}{mode}"),
        ]);
    }
    if let Some(name) = entry {
        cmd.args(["--entrypoint", name]);
    }
    let output = cmd.arg(image).args(args).output().map_err(storage)?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        if entry.is_some()
            && let Some(code @ (2 | 3)) = output.status.code()
        {
            return Err(format!(
                "{}: {}",
                if code == 3 { "conflict" } else { "invalid" },
                message.chars().take(1000).collect::<String>()
            ));
        }
        eprintln!(
            "Pinned dataset worker failed ({}): {}",
            output.status,
            message.chars().take(1000).collect::<String>()
        );
        return Err("storage: pinned worker failed; draft and releases are unchanged".into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn render_version(
    engine: &str,
    validator_image: &str,
    lifecycle_image: &str,
    original: &Path,
    parent: &Path,
    incoming: Option<&Path>,
    operation: &Value,
    candidate: Option<&Path>,
) -> Result<(Build, Value), String> {
    let temp = match candidate {
        Some(dir) => TempDir::new_in(dir),
        None => TempDir::new(),
    }
    .map_err(storage)?;
    let output = directory(temp.path(), "version")?;
    container_writable(&output)?;
    let request = temp.path().join("operation.json");
    fs::write(&request, serde_json::to_vec(operation).map_err(storage)?).map_err(storage)?;
    let mut mounts = vec![
        (parent, "/in/base.zip", false),
        (&request, "/in/operation.json", false),
        (&output, "/out", true),
    ];
    if let Some(incoming) = incoming {
        mounts.push((incoming, "/in/incoming.zip", false));
    }
    run(
        engine,
        lifecycle_image,
        &mounts,
        Some("python"),
        &[
            "-m",
            "bc_hla_import.version_changes",
            "/in/base.zip",
            if incoming.is_some() {
                "/in/incoming.zip"
            } else {
                "/in/base.zip"
            },
            "/in/operation.json",
            "/out",
        ],
    )?;
    let rendered = output.join("rendered");
    let mut validation = validator(
        engine,
        validator_image,
        &rendered.join("bundle.zip"),
        temp.path(),
    )?;
    validation["original_sha256"] = json!(archive_hash(original)?);
    validation["validator_image"] = json!(validator_image);
    validation["lifecycle_image"] = json!(lifecycle_image);
    let effects = read_json(&output.join("effects.json"))?;
    Ok((
        Build {
            _temp: temp,
            rendered,
            validation,
        },
        effects,
    ))
}

fn directory(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = root.join(name);
    fs::create_dir(&path).map_err(storage)?;
    Ok(path)
}

fn validator(engine: &str, image: &str, source: &Path, root: &Path) -> Result<Value, String> {
    let out = directory(root, "validation")?;
    container_writable(&out)?;
    let params = root.join("params.json");
    fs::write(&params, b"{}").map_err(storage)?;
    run(
        engine,
        image,
        &[
            (source, "/in/data/bundle.zip", false),
            (&params, "/in/params.json", false),
            (&out, "/out", true),
        ],
        None,
        &[],
    )?;
    let report = read_json(&out.join("result.json"))?;
    if report["format"] != "bc-hla-import.validation.v1"
        || !report["valid"].is_boolean()
        || report["source_sha256"].as_str() != Some(&archive_hash(source)?)
    {
        return Err("conflict: validator report does not identify the staged bundle".into());
    }
    Ok(report)
}

pub fn attest(engine: &str, image: &str, overlay: &str, source: &Path) -> Result<Value, String> {
    image_id(overlay)?;
    let original = read_json(&source.with_file_name("report.json"))?;
    let temp = TempDir::new().map_err(storage)?;
    let mut checked = validator(engine, image, source, temp.path())?;
    if checked != original {
        return Err("conflict: pinned validator did not reproduce the saved draft report".into());
    }
    checked["original_sha256"] = checked["source_sha256"].clone();
    checked["validator_image"] = json!(image);
    checked["overlay_image"] = json!(overlay);
    Ok(checked)
}

pub fn render(
    engine: &str,
    validator_image: &str,
    overlay_image: &str,
    source: &Path,
    patches: &[Value],
    publication_dir: Option<&Path>,
) -> Result<Build, String> {
    // A killed publication leaves its worker files in the unpublished candidate.
    // The next locked attempt can remove them along with the incomplete DuckLake.
    let temp = match publication_dir {
        Some(dir) => TempDir::new_in(dir),
        None => TempDir::new(),
    }
    .map_err(storage)?;
    let rendered = directory(temp.path(), "rendered")?;
    container_writable(&rendered)?;
    let changes = temp.path().join("patches.json");
    fs::write(&changes, serde_json::to_vec(patches).map_err(storage)?).map_err(storage)?;
    run(
        engine,
        overlay_image,
        &[
            (source, "/in/source.zip", false),
            (&changes, "/in/patches.json", false),
            (&rendered, "/out", true),
        ],
        Some("python"),
        &[
            "-m",
            "bc_hla_import.overlay",
            "/in/source.zip",
            "/out",
            "/in/patches.json",
        ],
    )?;
    let mut validation = validator(
        engine,
        validator_image,
        &rendered.join("bundle.zip"),
        temp.path(),
    )?;
    validation["original_sha256"] = json!(archive_hash(source)?);
    validation["validator_image"] = json!(validator_image);
    validation["overlay_image"] = json!(overlay_image);
    Ok(Build {
        _temp: temp,
        rendered,
        validation,
    })
}
