//! Library entrypoint for `biocompass` binary components (`cloud_client`, `llm_backends`, `state`, and `server`).

mod app_distribution;
pub mod cloud_client;
pub mod llm_backends;
mod package_install;
#[path = "bin/dataset_manager/store.rs"]
pub mod dataset_store;
mod native_import;
pub mod server;
pub mod state;
pub mod workspace;
