use super::*;
use serde_json::json;
use std::io::{Seek, Write};

const REPO: &str = "127.0.0.1:2/qa/release";
fn blob(value: &[u8], ty: &str, blobs: &mut BTreeMap<String, Vec<u8>>) -> Value {
    let hash = hex::encode(Sha256::digest(value));
    blobs.insert(format!("blobs/sha256/{hash}"), value.to_vec());
    json!({"digest":format!("sha256:{hash}"), "size":value.len(), "mediaType":ty})
}
type Files = BTreeMap<String, Vec<u8>>;
fn fixture(index: bool, edit: impl FnOnce(&mut Value)) -> (String, Files) {
    let mut files = Files::new();
    let config = blob(
        br#"{"os":"linux","architecture":"arm64"}"#,
        "application/vnd.oci.image.config.v1+json",
        &mut files,
    );
    let layer = blob(
        b"deterministic neutral fixture",
        "application/vnd.oci.image.layer.v1.tar",
        &mut files,
    );
    let mut image =
        json!({"schemaVersion":2,"mediaType":MANIFEST,"config":config,"layers":[layer]});
    edit(&mut image);
    let mut root = blob(&serde_json::to_vec(&image).unwrap(), MANIFEST, &mut files);
    if index {
        root["platform"] = json!({"os":"linux","architecture":"arm64"});
        let ix = json!({"schemaVersion":2,"mediaType":LIST,"manifests":[root]});
        root = blob(&serde_json::to_vec(&ix).unwrap(), LIST, &mut files);
    }
    let digest = root["digest"].as_str().unwrap().to_owned();
    let tag = format!("{REPO}:biocompass-{}", &digest[7..]);
    root["annotations"] =
        json!({"io.containerd.image.name":tag,"org.opencontainers.image.ref.name":tag});
    files.insert(
        "index.json".into(),
        json!({"schemaVersion":2,"manifests":[root]})
            .to_string()
            .into_bytes(),
    );
    files.insert(
        "oci-layout".into(),
        br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
    );
    (format!("{REPO}@{digest}"), files)
}
fn pack(files: &Files, special: Option<(&str, tar::EntryType)>) -> File {
    let mut f = tempfile::tempfile().unwrap();
    {
        let mut tar = tar::Builder::new(&mut f);
        for (path, bytes) in files {
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o444);
            tar.append_data(&mut header, path, bytes.as_slice())
                .unwrap();
        }
        if let Some((path, ty)) = special {
            let mut header = tar::Header::new_ustar();
            header.set_size(0);
            header.set_mode(0o444);
            header.set_entry_type(ty);
            tar.append_data(&mut header, path, std::io::empty())
                .unwrap();
        }
        tar.finish().unwrap();
    }
    f.rewind().unwrap();
    f
}

#[test]
fn verifies_both_leaf_and_index_with_the_exact_canonical_pin() {
    for index in [false, true] {
        let (pin, files) = fixture(index, |_| {});
        let mut file = pack(&files, None);
        assert_eq!(
            verify(&mut file, &pin).unwrap(),
            vec![Platform {
                os: "linux".into(),
                architecture: "arm64".into(),
                variant: String::new()
            }]
        );
        assert_eq!(file.stream_position().unwrap(), 0);
        assert!(verify(&mut file, &pin.replace(REPO, "other.invalid/qa/release")).is_err());
    }
}
#[test]
fn rejects_layer_tampering_size_mismatch_missing_and_unbound_blobs() {
    let (pin, files) = fixture(false, |_| {});
    let address = format!(
        "blobs/sha256/{:x}",
        Sha256::digest(b"deterministic neutral fixture")
    );
    let mut tampered = files.clone();
    tampered.get_mut(&address).unwrap()[0] ^= 1;
    assert!(verify(&mut pack(&tampered, None), &pin).is_err());
    let mut missing = files.clone();
    missing.remove(&address);
    assert!(verify(&mut pack(&missing, None), &pin).is_err());
    let mut extra = files;
    blob(b"unrelated", "application/octet-stream", &mut extra);
    assert!(verify(&mut pack(&extra, None), &pin).is_err());
    let (pin, wrong_size) = fixture(false, |doc| doc["layers"][0]["size"] = json!(1));
    assert!(verify(&mut pack(&wrong_size, None), &pin).is_err());
}
#[test]
fn rejects_extra_import_tags_special_files_duplicate_names_and_concatenated_archives() {
    let (pin, mut files) = fixture(false, |_| {});
    for (path, kind) in [
        ("manifest.json", tar::EntryType::Regular),
        ("oci-layout", tar::EntryType::Regular),
        ("alias", tar::EntryType::Symlink),
    ] {
        assert!(verify(&mut pack(&files, Some((path, kind))), &pin).is_err());
    }
    let mut combined = pack(&files, None);
    combined.seek(SeekFrom::End(0)).unwrap();
    combined
        .write_all(b"unvalidated archive behind padding")
        .unwrap();
    combined.rewind().unwrap();
    assert!(verify(&mut combined, &pin).is_err());
    let mut index: Value = serde_json::from_slice(&files["index.json"]).unwrap();
    index["manifests"][0]["annotations"]["io.containerd.image.name"] = json!("unrelated:latest");
    files.insert("index.json".into(), index.to_string().into_bytes());
    assert!(verify(&mut pack(&files, None), &pin).is_err());
}

#[tokio::test]
#[ignore = "requires a real engine and the disposable runnable archive in BIOCOMPASS_QA_ARCHIVE"]
async fn clean_engine_runs_verified_canonical_pin_offline() {
    let path = std::env::var("BIOCOMPASS_QA_ARCHIVE").unwrap();
    let pin = std::env::var("BIOCOMPASS_QA_PIN").unwrap();
    let runner = match std::env::var("BIOCOMPASS_QA_PODMAN") {
        Ok(path) => {
            OciContainerRunner::from_engine(super::super::probe_podman(&path).await.unwrap())
        }
        Err(_) => OciContainerRunner::detect().await.unwrap(),
    };
    assert!(
        !runner.has_verified_local_digest(&pin).await,
        "fixture pin must start absent"
    );
    runner
        .import_release_image(&pin, Path::new(&path))
        .await
        .unwrap();
    assert!(runner.has_verified_local_digest(&pin).await);
    let stage = tempfile::tempdir().unwrap();
    let input = stage.path().join("in");
    let output = stage.path().join("out");
    crate::prepare_stage_permissions(stage.path(), &input, &output).unwrap();
    let spec = crate::ContainerRunSpec {
        run_id: "offline-oci-qa".into(),
        image: OciImageRef::parse(&pin, false).unwrap(),
        in_dir: input,
        out_dir: output.clone(),
        resources: biocompass_plugin_spec::PluginResources {
            cpus: 1,
            memory_mb: 64,
            timeout_sec: 30,
            ..Default::default()
        },
        network: Default::default(),
        command_override: None,
    };
    let report = runner.execute(&spec, None).await.unwrap();
    assert_eq!(report.exit_code, 0, "{:?}", report.stderr_lines);
    let value: Value =
        serde_json::from_slice(&std::fs::read(output.join("result.json")).unwrap()).unwrap();
    assert_eq!(value, json!({"number":17}));
    eprintln!(
        "engine={:?} version={} pin={} result={}",
        report.engine, report.engine_version, pin, value
    );
}

#[tokio::test]
#[ignore = "requires a real engine with the foreign-platform fixture preloaded"]
async fn cached_foreign_only_digest_is_ineligible() {
    let runner = OciContainerRunner::detect().await.unwrap();
    let pin = std::env::var("BIOCOMPASS_QA_PIN").unwrap();
    let cached = runner
        .inspect_release(&pin)
        .await
        .expect("known digest must exist");
    let native = runner.local_platform().await.unwrap();
    assert!(!native.matches(&cached));
    assert!(
        !runner.has_verified_local_digest(&pin).await,
        "foreign-only cached digest must not qualify for native installation"
    );
    let archive = std::env::var("BIOCOMPASS_QA_ARCHIVE").unwrap();
    assert!(
        runner
            .import_release_image(&pin, Path::new(&archive))
            .await
            .unwrap_err()
            .to_string()
            .contains("does not contain")
    );
}
