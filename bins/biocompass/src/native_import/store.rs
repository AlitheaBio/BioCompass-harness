//! Private local dataset authority. A dataset's state file is the single
//! visibility boundary for its ACL, draft audit and committed release.
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AuditEntry {
    pub revision: u64,
    pub actor: String,
    pub timestamp: String,
    pub reason: String,
    pub patch: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Release {
    pub version: u64,
    pub revision: u64,
    pub visibility: String,
    pub actor: String,
    pub timestamp: String,
    /// Relative to the dataset directory. This path is stable across restarts.
    pub object_dir: String,
    pub manifest: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub dataset: String,
    pub org: String,
    pub draft: String,
    pub source_sha: String,
    pub importer: String,
    pub editors: Vec<String>,
    pub publishers: Vec<String>,
    pub revision: u64,
    pub audit: Vec<AuditEntry>,
    pub validation: Option<Value>,
    pub release: Option<Release>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Release>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_owner: Option<Value>,
}

impl State {
    pub fn version(&self, version: Option<u64>) -> Result<&Release, String> {
        if version.is_none() {
            return self
                .release
                .as_ref()
                .ok_or_else(|| "not_found: no published private version".into());
        }
        self.release
            .iter()
            .chain(&self.history)
            .find(|release| Some(release.version) == version)
            .ok_or_else(|| "not_found: requested private dataset version is unavailable".into())
    }
}

#[derive(Clone)]
pub struct LocalStore {
    root: PathBuf,
}

enum Access {
    Read,
    Edit,
    Publish,
}

impl State {
    fn check(&self, org: &str, user: &str, action: Access) -> Result<(), String> {
        let edits = self.editors.iter().any(|id| id == user);
        let publishes = self.publishers.iter().any(|id| id == user);
        let permitted = match action {
            Access::Read => edits || publishes,
            Access::Edit => edits,
            Access::Publish => publishes,
        };
        if org != self.org || !permitted {
            return Err("forbidden: dataset permission required".into());
        }
        Ok(())
    }
}

fn storage(err: impl std::fmt::Display) -> String {
    format!("storage: {err}")
}

fn private_dir(path: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.recursive(true).create(path).map_err(storage)?;
    let meta = path.symlink_metadata().map_err(storage)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(storage("expected a real private directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(storage)?;
        }
    }
    Ok(())
}

fn sync_dir(dir: &Path) -> Result<(), String> {
    File::open(dir).and_then(|f| f.sync_all()).map_err(storage)
}

fn save(dir: &Path, state: &State) -> Result<(), String> {
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(storage)?;
    serde_json::to_writer(&mut tmp, state).map_err(storage)?;
    tmp.write_all(b"\n").map_err(storage)?;
    tmp.as_file().sync_all().map_err(storage)?;
    tmp.persist(dir.join("state.json")).map_err(storage)?;
    sync_dir(dir)
}

fn load(dir: &Path) -> Result<State, String> {
    let file = File::open(dir.join("state.json")).map_err(|err| {
        if err.kind() == ErrorKind::NotFound {
            "not_found: dataset does not exist".to_owned()
        } else {
            storage(err)
        }
    })?;
    serde_json::from_reader(io::BufReader::new(file)).map_err(storage)
}

fn lock(dir: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(dir.join("writer.lock")).map_err(storage)?;
    file.lock().map_err(storage)?;
    Ok(file)
}

fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn check_validation(report: &Value) -> Result<bool, String> {
    report
        .get("valid")
        .and_then(Value::as_bool)
        .ok_or_else(|| "invalid: validation report requires a boolean valid field".into())
}

fn sync_objects(dir: &Path) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        let kind = entry.file_type().map_err(storage)?;
        if kind.is_dir() {
            sync_objects(&entry.path())?;
        } else if kind.is_file() {
            File::open(entry.path())
                .and_then(|f| f.sync_all())
                .map_err(storage)?;
        } else {
            return Err(storage("release may contain only files and directories"));
        }
    }
    sync_dir(dir)
}

impl LocalStore {
    /// Root is a dedicated authority directory, outside the discoverable catalogs tree.
    pub fn open(root: &Path) -> Result<Self, String> {
        private_dir(root)?;
        Ok(Self {
            root: root.canonicalize().map_err(storage)?,
        })
    }

    fn dir(&self, dataset: &str) -> Result<PathBuf, String> {
        if dataset.len() > 128
            || !dataset.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !dataset
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err("invalid: dataset must be a short ASCII identifier".into());
        }
        Ok(self.root.join(dataset))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn provision_validated(
        &self,
        dataset: &str,
        org: &str,
        draft: &str,
        source_sha: &str,
        importer: &str,
        editors: Vec<String>,
        publishers: Vec<String>,
        validation: Value,
    ) -> Result<(), String> {
        check_validation(&validation)?;
        self.insert(
            dataset,
            org,
            draft,
            source_sha,
            importer,
            editors,
            publishers,
            Some(validation),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &self,
        dataset: &str,
        org: &str,
        draft: &str,
        source_sha: &str,
        importer: &str,
        mut editors: Vec<String>,
        mut publishers: Vec<String>,
        validation: Option<Value>,
    ) -> Result<(), String> {
        let dir = self.dir(dataset)?;
        let pin = importer
            .rsplit_once('@')
            .map_or(importer, |(_, digest)| digest);
        if org.trim().is_empty()
            || editors
                .iter()
                .chain(publishers.iter())
                .any(|u| u.trim().is_empty())
            || draft != format!("sha256-{source_sha}")
            || !hex_digest(source_sha)
            || !pin.strip_prefix("sha256:").is_some_and(hex_digest)
        {
            return Err("invalid: expected an organization, actor IDs, matching draft/source SHA-256 and immutable importer digest".into());
        }
        editors.sort();
        editors.dedup();
        publishers.sort();
        publishers.dedup();
        private_dir(&dir)?;
        let _lock = lock(&dir)?;
        if dir.join("state.json").try_exists().map_err(storage)? {
            return Err("conflict: dataset already provisioned".into());
        }
        let state = State {
            dataset: dataset.into(),
            org: org.into(),
            draft: draft.into(),
            source_sha: source_sha.into(),
            importer: importer.into(),
            editors,
            publishers,
            revision: 0,
            audit: vec![],
            validation,
            release: None,
            history: Vec::new(), node_owner: None,
        };
        save(&dir, &state)?;
        sync_dir(&self.root)
    }

    /// Publish an operator-approved sealed catalog without transforming any original tables.
    pub fn admit_sealed(
        &self,
        dataset: &str,
        org: &str,
        mut publishers: Vec<String>,
        manifest: Value,
        copy: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<State, String> {
        let dir = self.dir(dataset)?;
        if org.trim().is_empty() || publishers.is_empty() || publishers.iter().any(|user| user.trim().is_empty()) {
            return Err("invalid: provide a dataset owner and authenticated publisher IDs".into());
        }
        publishers.sort();
        publishers.dedup();
        let digest = manifest["dataset_digest"].as_str().filter(|digest| hex_digest(digest))
            .ok_or_else(|| "invalid: sealed catalog requires a dataset digest".to_string())?.to_owned();
        private_dir(&dir)?;
        let _lock = lock(&dir)?;
        if dir.join("state.json").try_exists().map_err(storage)? {
            let state = load(&dir)?;
            if state.org == org && state.publishers == publishers
                && state.release.as_ref().is_some_and(|release| release.manifest == manifest)
            {
                return Ok(state);
            }
            return Err("conflict: dataset already provisioned".into());
        }
        let objects = dir.join("objects");
        private_dir(&objects)?;
        for entry in fs::read_dir(&objects).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            if entry.file_name().to_string_lossy().strip_prefix("sealed-")
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()) && entry.file_type().map_err(storage)?.is_dir()
            {
                fs::remove_dir_all(entry.path()).map_err(storage)?;
            }
        }
        let object_dir = format!("objects/sealed-{}", uuid::Uuid::new_v4());
        let object = dir.join(&object_dir);
        private_dir(&object)?;
        copy(&object)?;
        sync_objects(&object)?;
        sync_dir(&objects)?;
        let state = State {
            dataset: dataset.into(), org: org.into(), draft: format!("sha256-{digest}"),
            source_sha: digest, importer: "ducklake-sealed-catalog.v1".into(), editors: Vec::new(),
            revision: 0, audit: Vec::new(), validation: Some(serde_json::json!({"valid": true})),
            release: Some(Release {
                version: 1, revision: 0, visibility: "private".into(), actor: "operator".into(),
                timestamp: Utc::now().to_rfc3339(), object_dir, manifest,
            }),
            publishers, history: Vec::new(), node_owner: None,
        };
        save(&dir, &state)?;
        sync_dir(&self.root)?;
        Ok(state)
    }

    fn authorized(
        &self,
        dataset: &str,
        org: &str,
        user: &str,
        action: Access,
    ) -> Result<State, String> {
        let state = load(&self.dir(dataset)?)?;
        if state.dataset != dataset {
            return Err("not_found: dataset does not exist".into());
        }
        state.check(org, user, action)?;
        Ok(state)
    }

    pub fn dataset_directory(&self, dataset: &str) -> Result<PathBuf,String> {
        self.dir(dataset)
    }

    /// For the governed provider after it has checked a separate query grant.
    pub fn published_state(&self, dataset: &str) -> Result<State,String> {
        load(&self.dir(dataset)?)
    }

    /// Install an original publisher's delegation once under the existing family lock.
    /// A concurrent opener observes the winning signed binding.
    pub fn bind_node_owner(&self, dataset: &str, org: &str, original: &str, provider: &str, proof: Value) -> Result<State, String> {
        let dir = self.dir(dataset)?;
        let _lock = lock(&dir)?;
        let mut state = load(&dir)?;
        if state.dataset != dataset || state.org != org || state.version(Some(1))?.actor != original {
            return Err("forbidden: original publisher does not own this local dataset".into());
        }
        if state.node_owner.is_none() {
            // Leave standalone/group ACLs with their original authority.
            if state.editors != [original] || state.publishers != [original] {
                return Err("forbidden: private dataset is outside this laptop owner's authority".into());
            }
            state.editors = vec![provider.to_owned()];
            state.publishers = vec![provider.to_owned()];
            state.node_owner = Some(proof);
            save(&dir, &state)?;
        }
        Ok(state)
    }

    pub fn read_state(&self, dataset: &str, org: &str, user: &str) -> Result<State, String> {
        self.authorized(dataset, org, user, Access::Read)
    }

    pub fn authorize_edit(&self, dataset: &str, org: &str, user: &str) -> Result<State, String> {
        self.authorized(dataset, org, user, Access::Edit)
    }

    pub fn authorize_publish(&self, dataset: &str, org: &str, user: &str) -> Result<State, String> {
        self.authorized(dataset, org, user, Access::Publish)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn edit(
        &self,
        dataset: &str,
        org: &str,
        user: &str,
        expected_revision: u64,
        patch: Value,
        reason: &str,
        validated: Value,
    ) -> Result<State, String> {
        self.authorize_edit(dataset, org, user)?;
        let dir = self.dir(dataset)?;
        let _lock = lock(&dir)?;
        let mut state = self.authorize_edit(dataset, org, user)?;
        if state.revision != expected_revision {
            // A lost response can be replayed without appending the edit again.
            if state.audit.iter().any(|e| {
                e.revision.checked_sub(1) == Some(expected_revision)
                    && e.actor == user
                    && e.patch == patch
                    && e.reason == reason
            }) {
                return Ok(state);
            }
            return Err("conflict: draft revision has changed".into());
        }
        if state.release.is_some() {
            return Err("conflict: draft was already published".into());
        }
        let is_object = |key| {
            patch
                .get(key)
                .and_then(Value::as_object)
                .is_some_and(|m| !m.is_empty())
        };
        let same_fields = patch["before"]
            .as_object()
            .zip(patch["after"].as_object())
            .is_some_and(|(before, after)| {
                before.len() == after.len() && before.keys().all(|k| after.contains_key(k))
            });
        if patch
            .get("table")
            .and_then(Value::as_str).is_none_or(|name| name.is_empty())
            || !is_object("row_key")
            || !is_object("before")
            || !is_object("after")
            || patch["before"] == patch["after"]
            || !same_fields
        {
            return Err(
                "invalid: correction requires a table, stable row key, and matching old/new fields"
                    .into(),
            );
        }
        check_validation(&validated)?;
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or("conflict: draft revision exhausted")?;
        state.audit.push(AuditEntry {
            revision: state.revision,
            actor: user.into(),
            timestamp: Utc::now().to_rfc3339(),
            reason: reason.into(),
            patch,
        });
        state.validation = Some(validated);
        save(&dir, &state)?;
        Ok(state)
    }

    pub fn publish<F>(
        &self,
        dataset: &str,
        org: &str,
        user: &str,
        revision: u64,
        build: F,
    ) -> Result<State, String>
    where
        F: FnOnce(&Path, &State) -> Result<Value, String>,
    {
        self.authorize_publish(dataset, org, user)?;
        let dir = self.dir(dataset)?;
        let _lock = lock(&dir)?;
        let mut state = self.authorize_publish(dataset, org, user)?;
        if state.revision != revision {
            return Err("conflict: draft revision has changed".into());
        }
        if state.release.is_some() {
            return Ok(state);
        }
        if !state
            .validation
            .as_ref()
            .is_some_and(|v| v.get("valid") == Some(&Value::Bool(true)))
        {
            return Err("invalid: draft has not passed validation".into());
        }
        // The materializer can record absolute DuckLake file locations. Build
        // in the final path, outside all discovery roots. Only state.json makes
        // a release visible. While holding the writer lock and observing no
        // release, a v1 directory can only be an uncommitted build attempt.
        let objects = dir.join("objects");
        private_dir(&objects)?;
        for item in fs::read_dir(&objects).map_err(storage)? {
            let item = item.map_err(storage)?;
            let name = item.file_name();
            let name = name.to_string_lossy();
            let abandoned = name == "v1"
                || name
                    .strip_prefix("v1-")
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
            if abandoned && item.file_type().map_err(storage)?.is_dir() {
                fs::remove_dir_all(item.path()).map_err(storage)?;
            }
        }
        // Reusing the same mounted parent after a crash can leave Docker Desktop
        // referring to its old inode. DuckLake keeps this attempt's final path.
        let relative_path = format!("objects/v1-{}", uuid::Uuid::new_v4());
        let candidate = dir.join(&relative_path);
        private_dir(&candidate)?;
        let manifest = build(&candidate, &state)?;
        if !manifest.is_object() {
            return Err("invalid: release manifest must be an object".into());
        }
        sync_objects(&candidate)?;
        sync_dir(&objects)?;
        state.release = Some(Release {
            version: 1,
            revision,
            visibility: "private".into(),
            actor: user.into(),
            timestamp: Utc::now().to_rfc3339(),
            object_dir: relative_path,
            manifest,
        });
        save(&dir, &state)?;
        Ok(state)
    }

    pub fn publish_change<F>(
        &self,
        dataset: &str,
        org: &str,
        user: &str,
        id: &str,
        build: F,
    ) -> Result<Release, String>
    where
        F: FnOnce(&Path, &State, &Release, u64) -> Result<Value, String>,
    {
        self.authorize_publish(dataset, org, user)?;
        let dir = self.dir(dataset)?;
        let _lock = lock(&dir)?;
        let mut state = self.authorize_publish(dataset, org, user)?;
        if let Some(release) = state
            .release
            .iter()
            .chain(&state.history)
            .find(|release| release.manifest["version_change"]["id"] == id)
        {
            return Ok(release.clone());
        }
        let parent = state.version(None)?;
        let version = parent
            .version
            .checked_add(1)
            .ok_or("conflict: dataset version exhausted")?;
        let objects = dir.join("objects");
        let prefix = format!("v{version}-");
        // An uncommitted attempt for the next version is unreachable by readers.
        for item in fs::read_dir(&objects).map_err(storage)? {
            let item = item.map_err(storage)?;
            if item.file_type().map_err(storage)?.is_dir()
                && item
                    .file_name()
                    .to_string_lossy()
                    .strip_prefix(&prefix)
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            {
                fs::remove_dir_all(item.path()).map_err(storage)?;
            }
        }
        let object_dir = format!("objects/{prefix}{}", uuid::Uuid::new_v4());
        let candidate = dir.join(&object_dir);
        private_dir(&candidate)?;
        let manifest = match build(&candidate, &state, parent, version) {
            Ok(manifest) => manifest,
            Err(error) => {
                fs::remove_dir_all(candidate).map_err(storage)?;
                return Err(error);
            }
        };
        sync_objects(&candidate)?;
        sync_dir(&objects)?;
        let release = Release {
            version,
            revision: state.revision,
            visibility: "private".into(),
            actor: user.into(),
            timestamp: Utc::now().to_rfc3339(),
            object_dir,
            manifest,
        };
        let parent = state
            .release
            .replace(release.clone())
            .expect("current release checked");
        state.history.push(parent);
        save(&dir, &state)?;
        Ok(release)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, LocalStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path()).unwrap();
        let digest = "1".repeat(64);
        store
            .provision_validated(
                "study",
                "org-a",
                &format!("sha256-{digest}"),
                &digest,
                &format!("sha256:{digest}"),
                vec!["editor".into(), "both".into()],
                vec!["publisher".into(), "both".into()],
                json!({"valid": false}),
            )
            .unwrap();
        (dir, store)
    }

    fn correction() -> Value {
        json!({"table":"items","row_key":{"id":"old"},"before":{"id":"old"},"after":{"id":"new"}})
    }

    #[test]
    fn commits_a_valid_original_and_rejects_a_stale_or_ambiguous_edit() {
        let (root, store) = fixture();
        let sha = "b".repeat(64);
        store
            .provision_validated(
                "second",
                "org-a",
                &format!("sha256-{sha}"),
                &sha,
                &format!("sha256:{sha}"),
                vec![],
                vec!["reviewer".into()],
                json!({"valid":true}),
            )
            .unwrap();
        assert!(
            store
                .publish("second", "org-a", "publisher", 0, |_, _| unreachable!())
                .is_err()
        );
        let second = store
            .publish("second", "org-a", "reviewer", 0, |_, _| {
                Ok(json!({"count":0}))
            })
            .unwrap();
        assert_eq!(second.release.unwrap().revision, 0);
        let repaired = store
            .edit(
                "study",
                "org-a",
                "both",
                0,
                correction(),
                "s1",
                json!({"valid":false}),
            )
            .unwrap();
        let retry = store
            .edit(
                "study",
                "org-a",
                "both",
                0,
                correction(),
                "s1",
                json!({"valid":false}),
            )
            .unwrap();
        assert_eq!(repaired, retry);
        assert!(
            store
                .edit(
                    "study",
                    "org-a",
                    "editor",
                    0,
                    correction(),
                    "new",
                    json!({"valid":true})
                )
                .is_err()
        );
        assert!(
            store
                .publish("study", "org-a", "both", 1, |_, _| unreachable!())
                .is_err()
        );
        let mut invalid = correction();
        invalid["after"] = json!({"different_field":"new"});
        assert!(
            store
                .edit(
                    "study",
                    "org-a",
                    "both",
                    1,
                    invalid,
                    "s1",
                    json!({"valid":true})
                )
                .is_err()
        );
        assert_eq!(
            store.read_state("study", "org-a", "both").unwrap(),
            repaired
        );
        assert!(store.read_state("../second", "org-a", "reviewer").is_err());
        assert!(!root.path().join("second/objects/v2").exists());
    }

    #[test]
    fn publication_crash_child() {
        let Some(root) = std::env::var_os("BC_STORE_UNIT_CHILD_ROOT") else {
            return;
        };
        let root = Path::new(&root);
        let store = LocalStore::open(root).unwrap();
        let stage = std::env::var("BC_STORE_UNIT_CHILD_STAGE").unwrap();
        let pause = || {
            fs::write(root.join(format!("ready-{stage}")), b"ready").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(20));
            panic!("child was not interrupted");
        };
        store
            .publish("study", "org-a", "publisher", 1, |path, _| {
                assert!(!path.join("interrupted").exists());
                if stage == "before" {
                    fs::write(path.join("interrupted"), b"partial").unwrap();
                    pause();
                }
                fs::write(path.join("result"), b"complete").unwrap();
                Ok(json!({"resources":["result"]}))
            })
            .unwrap();
        pause();
    }

    #[test]
    fn process_kills_on_both_sides_of_the_commit_preserve_one_release() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let (dir, store) = fixture();
        store
            .edit(
                "study",
                "org-a",
                "editor",
                0,
                correction(),
                "reviewed",
                json!({"valid":true}),
            )
            .unwrap();
        for stage in ["before", "after"] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .arg("publication_crash_child")
                .env("BC_STORE_UNIT_CHILD_ROOT", dir.path())
                .env("BC_STORE_UNIT_CHILD_STAGE", stage)
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !dir.path().join(format!("ready-{stage}")).exists() {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "child exited before {stage} checkpoint"
                );
                if Instant::now() > deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("child did not reach {stage} checkpoint");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let visible = store.read_state("study", "org-a", "editor").unwrap();
            assert_eq!(visible.release.is_some(), stage == "after");
            child.kill().unwrap();
            assert!(!child.wait().unwrap().success());
        }
        let restarted = LocalStore::open(dir.path()).unwrap();
        let replay = restarted
            .publish("study", "org-a", "publisher", 1, |_, _| unreachable!())
            .unwrap();
        let release = replay.release.unwrap();
        let objects = dir.path().join("study").join(release.object_dir);
        assert_eq!(fs::read(objects.join("result")).unwrap(), b"complete");
        assert_eq!(fs::read_dir(&objects).unwrap().count(), 1);
        assert_eq!(fs::read_dir(objects.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn separates_permissions_and_commits_an_audited_private_release_once() {
        let (_dir, store) = fixture();
        let denied = |v: Result<State, String>| assert!(v.unwrap_err().starts_with("forbidden:"));
        denied(store.read_state("study", "other-org", "both"));
        denied(store.read_state("study", "org-a", "stranger"));
        denied(store.edit(
            "study",
            "org-a",
            "publisher",
            0,
            correction(),
            "curation",
            json!({"valid":true}),
        ));
        denied(store.publish("study", "org-a", "editor", 0, |_, _| unreachable!()));
        assert!(
            store
                .publish("study", "org-a", "publisher", 0, |_, _| unreachable!())
                .is_err()
        );
        let draft = store
            .edit(
                "study",
                "org-a",
                "editor",
                0,
                correction(),
                "curation",
                json!({"valid":true}),
            )
            .unwrap();
        assert_eq!(draft.audit[0].actor, "editor");
        assert_eq!(draft.audit[0].patch, correction());
        assert_eq!(draft.revision, 1);
        let release = store
            .publish("study", "org-a", "publisher", 1, |path, _| {
                std::fs::write(path.join("one"), b"one").unwrap();
                Ok(json!({"resources":["one"]}))
            })
            .unwrap()
            .release
            .unwrap();
        assert_eq!(release.visibility, "private");
        let retry = store
            .publish("study", "org-a", "publisher", 1, |_, _| unreachable!())
            .unwrap()
            .release
            .unwrap();
        assert_eq!(release, retry);
        assert!(
            store
                .edit(
                    "study",
                    "org-a",
                    "both",
                    1,
                    correction(),
                    "again",
                    json!({"valid":true})
                )
                .is_err()
        );
    }
}
