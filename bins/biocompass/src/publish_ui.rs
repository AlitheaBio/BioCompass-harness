use std::{collections::HashSet, path::Path};

use biocompass_core_traits::{CompassError, CompassResult};
use html5gum::{DefaultEmitter, Token, Tokenizer};
use reqwest::Url;

use super::UiBundleFileUploadDto;

fn executes_script(kind: &str) -> bool {
    matches!(
        kind.to_ascii_lowercase().as_str(),
        "" | "module"
            | "application/javascript"
            | "application/ecmascript"
            | "application/x-javascript"
            | "application/x-ecmascript"
            | "text/javascript"
            | "text/ecmascript"
            | "text/jscript"
            | "text/livescript"
            | "text/x-javascript"
            | "text/x-ecmascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
    )
}

fn is_site_url(reference: &str) -> bool {
    reference.starts_with('/') || Url::parse(reference).is_ok()
}

/// Check the entry HTML against the exact bytes that will be uploaded, before registering it.
pub(super) fn validate_entry_assets(
    entry: &Path,
    files: &[UiBundleFileUploadDto],
) -> CompassResult<()> {
    let entry_name = entry
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| CompassError::Validation("UI entry must have a filename".into()))?;
    let file = files.iter().find(|f| f.path == entry_name).ok_or_else(|| {
        CompassError::Validation(format!("UI entry {entry_name} is missing from the bundle"))
    })?;
    let html = hex::decode(&file.data_hex).map_err(|e| CompassError::Internal(e.to_string()))?;
    let location = entry
        .canonicalize()
        .map_err(|e| CompassError::Storage(format!("resolve UI entry: {e}")))?;
    let entry_url = Url::from_file_path(location)
        .map_err(|_| CompassError::Validation("invalid UI entry path".into()))?;
    // Round-trip the root as well as its assets so Windows verbatim drive paths compare alike.
    let entry_path = entry_url
        .to_file_path()
        .map_err(|_| CompassError::Validation("invalid UI entry file URL".into()))?;
    let root = entry_path
        .parent()
        .expect("an absolute UI file path has a parent");
    let names: HashSet<_> = files.iter().map(|f| Path::new(&f.path)).collect();
    let mut emitter = DefaultEmitter::default();
    emitter.naively_switch_states(true);
    let mut base = None;
    let mut assets = Vec::new();
    let mut template_depth = 0usize;
    for token in Tokenizer::new_with_emitter(html.as_slice(), emitter).flatten() {
        let tag = match token {
            Token::StartTag(tag) if tag.name.as_slice() == b"template" => {
                template_depth += 1;
                continue;
            }
            Token::EndTag(tag) if tag.name.as_slice() == b"template" => {
                template_depth = template_depth.saturating_sub(1);
                continue;
            }
            Token::StartTag(tag) if template_depth == 0 => tag,
            _ => continue,
        };
        let attr = |key: &[u8]| {
            tag.attributes
                .get(key)
                .and_then(|v| std::str::from_utf8(&v.value).ok())
                .map(str::trim)
        };
        let reference = match tag.name.as_slice() {
            b"base" if base.is_none() => {
                base = attr(b"href").map(str::to_owned);
                None
            }
            b"script" if executes_script(attr(b"type").unwrap_or("")) => attr(b"src"),
            b"link"
                if attr(b"rel")
                    .unwrap_or("")
                    .split_ascii_whitespace()
                    .any(|rel| {
                        rel.eq_ignore_ascii_case("stylesheet")
                            || rel.eq_ignore_ascii_case("modulepreload")
                    }) =>
            {
                attr(b"href")
            }
            _ => None,
        };
        if let Some(reference) = reference.filter(|s| !s.is_empty()) {
            assets.push(reference.to_owned());
        }
    }
    // Relative references below a host-supplied base resolve outside the package.
    if base.as_deref().is_some_and(is_site_url) {
        return Ok(());
    }
    let base = base
        .and_then(|href| entry_url.join(&href).ok())
        .unwrap_or(entry_url);
    for reference in assets {
        // A site route or fully qualified URL belongs to the host, outside this upload.
        if is_site_url(&reference) {
            continue;
        }
        let resolved = base.join(&reference).map_err(|e| {
            CompassError::Validation(format!(
                "invalid asset URL '{reference}' in {entry_name}: {e}"
            ))
        })?;
        if resolved.scheme() != "file" {
            continue;
        }
        let path = resolved.to_file_path().map_err(|_| {
            CompassError::Validation(format!("invalid file URL '{reference}' in {entry_name}"))
        })?;
        let relative = path.strip_prefix(root).map_err(|_| {
            CompassError::Validation(format!(
                "UI entry {entry_name} references '{reference}' outside the bundle"
            ))
        })?;
        if !names.contains(relative) {
            return Err(CompassError::Validation(format!(
                "UI entry {entry_name} references missing bundle asset '{}'; rebuild or include this file",
                relative.display()
            )));
        }
    }
    Ok(())
}
