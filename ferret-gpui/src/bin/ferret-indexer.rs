//! The FerretIndexer search service and its command line (ferret-core's `search::service`), built
//! with the app so the installer ships both. The same program as the Tauri app's
//! src-tauri/src/bin/ferret-indexer.rs.

fn main() {
    std::process::exit(ferret_core::search::service::main_entry());
}
