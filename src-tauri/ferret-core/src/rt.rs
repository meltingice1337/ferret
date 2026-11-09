//! The async runtime the core's work runs on. One multi-threaded tokio runtime for the
//! process; the app hands it to its own framework (Tauri: `tauri::async_runtime::set`) so the
//! commands, SSH sessions and blocking jobs all share it.

use std::future::Future;
use std::sync::OnceLock;
use tokio::runtime::{Handle, Runtime};
use tokio::task::JoinHandle;

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("the async runtime"))
}

/// The runtime's handle (for the app to run its own tasks on).
pub fn handle() -> Handle {
    runtime().handle().clone()
}

/// Runs blocking work on the runtime's blocking threads.
pub fn spawn_blocking<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    runtime().spawn_blocking(f)
}

/// Starts a task on the runtime.
pub fn spawn<F>(f: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(f)
}

/// Runs a future to completion from code that isn't async (not from one of the runtime's own
/// threads: use [`block`] there).
pub fn block_on<F: Future>(f: F) -> F::Output {
    runtime().block_on(f)
}

/// Runs async work from blocking code: job threads, the file scheme, folder-size workers.
/// (On one of the runtime's own threads it steps aside first, as `block_in_place` does.)
pub fn block<F: Future>(f: F) -> F::Output {
    let handle = handle();
    tokio::task::block_in_place(|| handle.block_on(f))
}
