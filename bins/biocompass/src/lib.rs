//! Library entrypoint for `biocompass` binary components (`cloud_client`, `llm_backends`, `state`, and `server`).

mod app_distribution;
pub mod cloud_client;
pub mod llm_backends;
pub mod marketplace;
mod package_install;
#[path = "native_import/store.rs"]
pub mod dataset_store;
mod native_import;
pub mod server;
pub mod state;
pub mod ui_cache;
pub mod workspace;

#[cfg(test)]
mod r5_tests {
    use std::path::PathBuf;

    #[test]
    fn dev_06b_r5_dataset_supervisor_retired_and_local_store_moved() {
        let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert!(
            !crate_dir.join("src/bin/biocompass-dataset.rs").exists(),
            "legacy biocompass-dataset binary must be retired"
        );
        assert!(
            !crate_dir.join("src/bin/dataset_manager").exists(),
            "legacy dataset_manager directory must be retired"
        );
        assert!(
            !crate_dir.join("PRIVATE_DATASETS.md").exists(),
            "legacy PRIVATE_DATASETS.md must be retired"
        );
        assert!(
            crate_dir.join("src/native_import/store.rs").is_file(),
            "LocalStore must live under src/native_import/store.rs"
        );
    }
}
