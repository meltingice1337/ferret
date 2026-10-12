//! Long-running background jobs (copy, move, delete, extract, compress…).
//! Each job runs on a blocking thread and reports progress through the
//! `job-update` event, throttled to ~10 updates per second.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn registry() -> &'static Mutex<HashMap<u64, Arc<AtomicBool>>> {
    static R: OnceLock<Mutex<HashMap<u64, Arc<AtomicBool>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct JobUpdate {
    pub id: u64,
    pub kind: String,
    pub label: String,
    /// "running" | "done" | "error" | "cancelled"
    pub status: &'static str,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub current: String,
    pub error: Option<String>,
    /// Paths created by the job (for undo / selecting results).
    pub created: Vec<String>,
    /// Existing items that were moved to the Recycle Bin because they were replaced.
    pub replaced: Vec<String>,
    /// (source, destination) for each item a copy/move produced, for exact undo.
    pub pairs: Vec<(String, String)>,
    /// Items left alone because of a "Skip" conflict choice.
    pub skipped: u64,
}

pub struct JobCtx {
    app: AppHandle,
    cancel: Arc<AtomicBool>,
    state: Mutex<(JobUpdate, Instant)>,
}

impl JobCtx {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Returns Err("Cancelled") if the user cancelled; use with `?` in loops.
    /// Makes the job end as "cancelled" rather than "error" when it returns Err,
    /// for cancellations that didn't come from `cancel_job` (e.g. the user
    /// declined the UAC prompt).
    pub fn mark_cancelled(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err("Cancelled".into())
        } else {
            Ok(())
        }
    }

    pub fn set_totals(&self, bytes_total: u64, files_total: u64) {
        let mut s = self.state.lock().unwrap();
        s.0.bytes_total = bytes_total;
        s.0.files_total = files_total;
        self.emit_locked(&mut s, true);
    }

    pub fn set_current(&self, current: &str) {
        let mut s = self.state.lock().unwrap();
        s.0.current = current.to_string();
        self.emit_locked(&mut s, false);
    }

    pub fn add_bytes(&self, n: u64) {
        let mut s = self.state.lock().unwrap();
        s.0.bytes_done += n;
        self.emit_locked(&mut s, false);
    }

    pub fn file_done(&self) {
        let mut s = self.state.lock().unwrap();
        s.0.files_done += 1;
        self.emit_locked(&mut s, false);
    }

    pub fn created(&self, path: impl Into<String>) {
        self.state.lock().unwrap().0.created.push(path.into());
    }

    pub fn replaced(&self, path: impl Into<String>) {
        self.state.lock().unwrap().0.replaced.push(path.into());
    }

    pub fn skipped(&self) {
        self.state.lock().unwrap().0.skipped += 1;
    }

    pub fn pair(&self, from: impl Into<String>, to: impl Into<String>) {
        self.state.lock().unwrap().0.pairs.push((from.into(), to.into()));
    }

    fn emit_locked(&self, s: &mut (JobUpdate, Instant), force: bool) {
        if force || s.1.elapsed() >= Duration::from_millis(100) {
            s.1 = Instant::now();
            let _ = self.app.emit("job-update", &s.0);
        }
    }
}

/// Starts a job and returns its id immediately. The closure does the work;
/// returning Err marks the job failed (or cancelled if the user cancelled).
pub fn spawn<F>(app: &AppHandle, kind: &str, label: &str, work: F) -> u64
where
    F: FnOnce(&JobCtx) -> Result<(), String> + Send + 'static,
{
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    registry().lock().unwrap().insert(id, cancel.clone());
    let ctx = JobCtx {
        app: app.clone(),
        cancel,
        state: Mutex::new((
            JobUpdate { id, kind: kind.into(), label: label.into(), status: "running", ..Default::default() },
            Instant::now() - Duration::from_secs(1),
        )),
    };
    log::info!("job {id} {kind} started: {label}");
    {
        let mut s = ctx.state.lock().unwrap();
        ctx.emit_locked(&mut s, true);
    }
    tauri::async_runtime::spawn_blocking(move || {
        let started = Instant::now();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&ctx)))
            .unwrap_or_else(|_| Err("Internal error".into()));
        let mut s = ctx.state.lock().unwrap();
        match res {
            Ok(()) => s.0.status = "done",
            Err(_) if ctx.is_cancelled() => s.0.status = "cancelled",
            Err(e) => {
                s.0.status = "error";
                s.0.error = Some(e);
            }
        }
        let u = &s.0;
        log::log!(
            if u.status == "error" { log::Level::Warn } else { log::Level::Info },
            "job {id} {} {} in {:?}: {}/{} files, {}/{} bytes, {} created, {} replaced, {} skipped{}",
            u.kind, u.status, started.elapsed(),
            u.files_done, u.files_total, u.bytes_done, u.bytes_total, u.created.len(), u.replaced.len(), u.skipped,
            u.error.as_ref().map(|e| format!("; error: {e}")).unwrap_or_default()
        );
        ctx.emit_locked(&mut s, true);
        registry().lock().unwrap().remove(&id);
    });
    id
}

#[tauri::command]
pub fn cancel_job(id: u64) {
    if let Some(flag) = registry().lock().unwrap().get(&id) {
        flag.store(true, Ordering::Relaxed);
    }
}
