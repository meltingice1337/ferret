//! Long-running background jobs (copy, move, delete, extract, compress…).
//! Each job runs on a blocking thread and reports progress through the
//! `job-update` event, throttled to ~10 updates per second.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// At most ~10 progress updates a second.
const THROTTLE: Duration = Duration::from_millis(100);
/// Jobs running right now. Background work that competes for the disk (folder-size counting)
/// waits while there are any.
static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A copy, move, delete or other file job is running.
pub fn busy() -> bool {
    ACTIVE.load(Ordering::Relaxed) > 0
}

/** What the UI can do to a running job: cancel it, or answer the question it's waiting on. */
#[derive(Default)]
struct Control {
    cancel: AtomicBool,
    answer: Mutex<Option<String>>,
    wake: Condvar,
}

fn registry() -> &'static Mutex<HashMap<u64, Arc<Control>>> {
    static R: OnceLock<Mutex<HashMap<u64, Arc<Control>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct JobUpdate {
    pub id: u64,
    pub kind: String,
    pub label: String,
    /// "running" | "waiting" (for an answer to `question`) | "done" | "error" | "cancelled"
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
    /// Items the job couldn't handle and left in place (path, reason): a locked file doesn't
    /// stop the other 4,000.
    pub failed: Vec<(String, String)>,
    /// While waiting: what went wrong, for the user to retry, skip or cancel.
    pub question: Option<String>,
}

pub struct JobCtx {
    app: AppHandle,
    control: Arc<Control>,
    /// The latest update, when it was last sent, and whether it changed since (a throttled
    /// change is sent shortly after by the job's ticker instead of being lost).
    state: Arc<Mutex<(JobUpdate, Instant, bool)>>,
}

impl JobCtx {
    pub fn is_cancelled(&self) -> bool {
        self.control.cancel.load(Ordering::Relaxed)
    }

    /// Returns Err("Cancelled") if the user cancelled; use with `?` in loops.
    /// Makes the job end as "cancelled" rather than "error" when it returns Err,
    /// for cancellations that didn't come from `cancel_job` (e.g. the user
    /// declined the UAC prompt).
    pub fn mark_cancelled(&self) {
        self.control.cancel.store(true, Ordering::Relaxed);
    }

    /// Pauses the job until the user answers `question` in the jobs panel: "retry", "skip",
    /// "skipAll" or "cancel" (also what a cancelled job gets).
    pub fn ask(&self, question: String) -> String {
        *self.control.answer.lock().unwrap() = None;
        {
            let mut s = self.state.lock().unwrap();
            s.0.status = "waiting";
            s.0.question = Some(question);
            self.emit_locked(&mut s, true);
        }
        let mut answer = self.control.answer.lock().unwrap();
        let choice = loop {
            if self.is_cancelled() {
                break "cancel".to_string();
            }
            if let Some(a) = answer.take() {
                break a;
            }
            answer = self.control.wake.wait(answer).unwrap();
        };
        drop(answer);
        let mut s = self.state.lock().unwrap();
        s.0.status = "running";
        s.0.question = None;
        self.emit_locked(&mut s, true);
        choice
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

    pub fn failed(&self, path: impl Into<String>, reason: impl Into<String>) {
        self.state.lock().unwrap().0.failed.push((path.into(), reason.into()));
    }

    pub fn pair(&self, from: impl Into<String>, to: impl Into<String>) {
        self.state.lock().unwrap().0.pairs.push((from.into(), to.into()));
    }

    fn emit_locked(&self, s: &mut (JobUpdate, Instant, bool), force: bool) {
        if force || s.1.elapsed() >= THROTTLE {
            s.1 = Instant::now();
            s.2 = false;
            let _ = self.app.emit("job-update", &s.0);
        } else {
            s.2 = true;
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
    let control = Arc::new(Control::default());
    registry().lock().unwrap().insert(id, control.clone());
    let ctx = JobCtx {
        app: app.clone(),
        control,
        state: Arc::new(Mutex::new((
            JobUpdate { id, kind: kind.into(), label: label.into(), status: "running", ..Default::default() },
            Instant::now() - Duration::from_secs(1),
            false,
        ))),
    };
    // Sends a change the throttle held back ("now on “big folder”") even when the job then
    // works on that one item for a long time and reports nothing else.
    let finished = Arc::new(AtomicBool::new(false));
    {
        let (state, app, finished) = (ctx.state.clone(), app.clone(), finished.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(THROTTLE);
            if finished.load(Ordering::Relaxed) {
                break;
            }
            let mut s = state.lock().unwrap();
            if s.2 {
                s.1 = Instant::now();
                s.2 = false;
                let _ = app.emit("job-update", &s.0);
            }
        });
    }
    log::info!("job {id} {kind} started: {label}");
    ACTIVE.fetch_add(1, Ordering::Relaxed);
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
            "job {id} {} {} in {:?}: {}/{} files, {}/{} bytes, {} created, {} replaced, {} skipped, {} failed{}",
            u.kind, u.status, started.elapsed(),
            u.files_done, u.files_total, u.bytes_done, u.bytes_total, u.created.len(), u.replaced.len(), u.skipped, u.failed.len(),
            u.error.as_ref().map(|e| format!("; error: {e}")).unwrap_or_default()
        );
        ctx.emit_locked(&mut s, true);
        finished.store(true, Ordering::Relaxed);
        registry().lock().unwrap().remove(&id);
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
    });
    id
}

#[tauri::command]
pub fn cancel_job(id: u64) {
    if let Some(c) = registry().lock().unwrap().get(&id) {
        c.cancel.store(true, Ordering::Relaxed);
        let _guard = c.answer.lock().unwrap();
        c.wake.notify_all();
    }
}

/// The user's answer to a waiting job's question (see `JobCtx::ask`).
#[tauri::command]
pub fn answer_job(id: u64, answer: String) {
    if let Some(c) = registry().lock().unwrap().get(&id) {
        *c.answer.lock().unwrap() = Some(answer);
        c.wake.notify_all();
    }
}
