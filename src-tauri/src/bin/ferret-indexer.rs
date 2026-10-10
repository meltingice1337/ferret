//! FerretIndexer service + CLI (see `ferret_lib::search::service`).

fn main() {
    std::process::exit(ferret_lib::search::service::main_entry());
}
