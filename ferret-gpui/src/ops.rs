//! File operations (src/lib/ops.ts): the clipboard (shared with Explorer), paste and drops with
//! the conflict question, delete (Recycle Bin or for good), rename, new items, extract and
//! compress, the Recycle Bin, favorites, and undo/redo for each step. Long work runs as
//! ferret-core jobs, shown as cards with progress.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ferret_core::fileops::{self, Policy};
use ferret_core::paths;
use gpui::AnimationExt;
use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px, relative};

use crate::files::plural;
use crate::input::TextInput;
use crate::theme as t;
use crate::ui::{self, icon};
use crate::util::{self, same};
use crate::view::{Action, Ferret, JobCard, Rename, action};

/// An undoable step: its label ("Rename to “x”"), and how to undo and redo it.
pub struct Step {
    /// Unique for the run: a toast's Undo is for its own step only.
    pub id: u64,
    /// The toast that announced it (dismissed when the step is undone, as the app does).
    pub toast: Option<u64>,
    pub label: String,
    pub undo: Rc<dyn Fn(&mut Ferret, &mut Context<Ferret>)>,
    pub redo: Rc<dyn Fn(&mut Ferret, &mut Context<Ferret>)>,
}

/// What a finished job reported.
#[derive(Default, Clone)]
pub struct JobDone {
    pub status: String,
    pub error: Option<String>,
    pub created: Vec<String>,
    pub replaced: Vec<String>,
    pub pairs: Vec<(String, String)>,
    pub failed: Vec<String>,
    /// The same, with why each was left (ferret-core's markers kept: "elevate:" wants an administrator).
    pub failed_why: Vec<(String, String)>,
    pub skipped: u64,
}

type Waiter = Box<dyn FnOnce(&mut Ferret, JobDone, &mut Context<Ferret>)>;

thread_local! {
    /// Callbacks for when a job ends (the step it records, the toast it shows).
    static WAITERS: std::cell::RefCell<HashMap<u64, Waiter>> = Default::default();
}

pub fn when_done(id: u64, f: impl FnOnce(&mut Ferret, JobDone, &mut Context<Ferret>) + 'static) {
    WAITERS.with(|w| w.borrow_mut().insert(id, Box::new(f)));
}

/// OS error text, said plainly.
pub fn plain_error(e: &str) -> String {
    // ferret-core's markers for the UI ("elevate:" wants an administrator, "no-bin:" / "nobin:"
    // doesn't fit the Recycle Bin) are for deciding what to ask, never for showing.
    // At the start, or after a "Delete failed: " in front of it.
    let mut owned = e.to_string();
    for m in ["elevate:", "no-bin:", "nobin:"] {
        if let Some(rest) = owned.strip_prefix(m) {
            owned = rest.to_string();
        }
        owned = owned.replace(&format!(": {m}"), ": ");
    }
    let e = owned.as_str();
    let l = e.to_lowercase();
    if l.contains("access is denied") || l.contains("os error 5") || l.contains("permission denied") {
        return "Windows didn't allow it (access denied)".into();
    }
    if l.contains("being used by another process") || l.contains("os error 32") {
        return "It's open in another program".into();
    }
    if l.contains("cannot find the path") || l.contains("cannot find the file") || l.contains("os error 2") || l.contains("os error 3") {
        return "It isn't there anymore".into();
    }
    if l.contains("not enough space") || l.contains("os error 112") {
        return "There isn't enough space on the drive".into();
    }
    if l.contains("already exists") || l.contains("os error 183") {
        return "Something with that name is already there".into();
    }
    e.to_string()
}

fn describe(paths: &[String]) -> String {
    if paths.len() == 1 { format!("“{}”", util::file_name(&paths[0])) } else { plural(paths.len(), "item") }
}

/// Up to three names ("“a”, “b” and “c”"), then how many more.
fn names(paths: &[String]) -> String {
    let q: Vec<String> = paths.iter().take(if paths.len() > 3 { 2 } else { 3 }).map(|p| format!("“{}”", util::file_name(p))).collect();
    let more = paths.len() - q.len();
    match (q.len(), more) {
        (1, 0) => q[0].clone(),
        (_, 0) => format!("{} and {}", q[..q.len() - 1].join(", "), q[q.len() - 1]),
        _ => format!("{} and {} more", q.join(", "), more),
    }
}

pub fn zip_name_for(paths: &[String]) -> String {
    if paths.len() == 1 {
        let n = util::file_name(&paths[0]);
        let stem = if std::path::Path::new(&paths[0]).is_dir() { n } else { n.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(n) };
        format!("{stem}.zip")
    } else {
        let dir = util::parent(&paths[0]).map(|p| util::folder_name(&p)).unwrap_or_else(|| "Archive".into());
        format!("{dir}.zip")
    }
}

pub fn is_compressible_picture(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "png" | "heic" | "heif" | "webp" | "avif" | "bmp" | "tif" | "tiff" | "jxr" | "wdp")
}

/// Windows wants an administrator for this: the caller offers to go on as one (`ask_admin`).
/// ferret-core says so with "elevate:" (a protected folder, a file only admins may delete).
fn denied(e: &str) -> bool {
    let l = e.to_lowercase();
    l.contains("access is denied") || l.contains("os error 5") || l.starts_with("denied:") || l.starts_with("elevate:")
}

impl Ferret {
    // ───────────────────────────── jobs ─────────────────────────────

    pub fn job_update(&mut self, payload: &serde_json::Value, cx: &mut Context<Self>) {
        let Some(id) = payload["id"].as_u64() else { return };
        let status = payload["status"].as_str().unwrap_or("running").to_string();
        let finished = matches!(status.as_str(), "done" | "error" | "cancelled");
        if !self.jobs.iter().any(|j| j.id == id) {
            let now = Instant::now();
            self.jobs.push(JobCard {
                id,
                kind: payload["kind"].as_str().unwrap_or("").into(),
                label: String::new(),
                status: String::new(),
                done: 0,
                total: 0,
                files_done: 0,
                files_total: 0,
                current: String::new(),
                note: String::new(),
                question: None,
                error: None,
                started: now,
                current_since: now,
                finished_at: None,
                shown: false,
            });
        }
        let job = self.jobs.iter_mut().find(|j| j.id == id).unwrap();
        job.label = payload["label"].as_str().unwrap_or("").to_string();
        job.done = payload["bytesDone"].as_u64().unwrap_or(job.done);
        job.total = payload["bytesTotal"].as_u64().unwrap_or(job.total);
        job.files_done = payload["filesDone"].as_u64().unwrap_or(job.files_done);
        job.files_total = payload["filesTotal"].as_u64().unwrap_or(job.files_total);
        let current = payload["current"].as_str().unwrap_or("").to_string();
        if current != job.current {
            job.current_since = Instant::now();
            job.current = current;
        }
        job.note = payload["note"].as_str().unwrap_or("").to_string();
        job.question = payload["question"].as_str().map(str::to_string);
        job.error = payload["error"].as_str().map(str::to_string);
        let newly_done = finished && job.finished_at.is_none();
        job.status = status.clone();
        if newly_done {
            job.finished_at = Some(Instant::now());
            let kind = job.kind.clone();
            if let Some(b) = self.bench.as_ref() {
                b.write("job_done", format!("{id} {status} {}", payload["label"].as_str().unwrap_or("")));
            }
            let strings = |v: &serde_json::Value| -> Vec<String> { v.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)).collect() };
            let done = JobDone {
                status: status.clone(),
                error: payload["error"].as_str().map(str::to_string),
                created: strings(&payload["created"]),
                replaced: strings(&payload["replaced"]),
                pairs: payload["pairs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str()?.to_string())))
                    .collect(),
                failed: payload["failed"].as_array().into_iter().flatten().filter_map(|p| p[0].as_str().map(str::to_string)).collect(),
                failed_why: payload["failed"].as_array().into_iter().flatten().filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str().unwrap_or("").to_string()))).collect(),
                skipped: payload["skipped"].as_u64().unwrap_or(0),
            };
            if status == "cancelled" && kind != "media" {
                self.toast("Stopped", cx);
            }
            if let Some(w) = WAITERS.with(|w| w.borrow_mut().remove(&id)) {
                w(self, done.clone(), cx);
            } else if status == "error" {
                if let Some(e) = &done.error {
                    self.toast_error(e.clone(), cx);
                }
            }
            // What it changed shows up everywhere.
            self.reload_all(cx);
            self.refresh_bin(cx);
            // And the sizes of the folders it touched are counted again (servers and network
            // shares have no watcher to notice; sizes there stayed as they were). What a job
            // doesn't report (a delete's items) was noted when it started (`touched_sizes`).
            let mut touched = std::mem::take(&mut self.touched_sizes);
            touched.extend(done.pairs.iter().flat_map(|(a, b)| [a.clone(), b.clone()]));
            touched.extend(done.created.iter().cloned());
            touched.retain(|p| !p.is_empty());
            self.sizes_changed(touched, false, cx);
        }
        cx.notify();
    }

    // ───────────────────────────── clipboard ─────────────────────────────

    /// Copy or Cut (Ctrl+C / Ctrl+X): Windows' clipboard, so Explorer can paste them too.
    pub fn clipboard(&mut self, paths: Vec<String>, cut: bool, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        if cut && paths.iter().any(|p| ferret_core::archive::split_virtual(p).is_some() && !std::path::Path::new(p).exists()) {
            self.toast_error("Items inside an archive can't be cut: copy them instead", cx);
            return;
        }
        // A server's files can't go on Windows' clipboard: the window keeps them, for as long as
        // nothing else is copied (Windows' count of clipboard changes), as the app does.
        if paths.iter().any(|p| paths::is_remote(p)) {
            self.server_clip = Some((paths.clone(), cut, ferret_core::shell::clipboard_sequence()));
            self.cut = if cut { paths.iter().map(|p| paths::key(p)).collect() } else { Default::default() };
            self.toast(format!("{} {} (paste them in a Ferret folder)", if cut { "Cut" } else { "Copied" }, describe(&paths)), cx);
            return;
        }
        self.server_clip = None;
        let msg = format!("{} {}", if cut { "Cut" } else { "Copied" }, describe(&paths));
        let p2 = paths.clone();
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::shell::clipboard_write_files(p2, cut)) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| match r {
                Ok(()) => {
                    f.clip_seq = ferret_core::shell::clipboard_sequence();
                    f.cut = if cut { paths.iter().map(|p| paths::key(p)).collect() } else { Default::default() };
                    f.toast(msg, cx);
                }
                Err(e) => f.toast_error(format!("Clipboard error: {e}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn paste(&mut self, dest: String, cx: &mut Context<Self>) {
        if ferret_core::recycle::is_recycle(&dest) {
            self.toast_error("To delete files, drop them on the Recycle Bin or press Delete", cx);
            return;
        }
        if ferret_core::search::search_query_of(&dest).is_some() {
            self.toast_error("Search tabs show matches from many folders: paste into a folder", cx);
            return;
        }
        // Server files the window kept, if nothing was copied since.
        if let Some((list, cut, seq)) = self.server_clip.clone() {
            if seq == ferret_core::shell::clipboard_sequence() {
                if cut {
                    // A pasted cut leaves the clipboard.
                    self.server_clip = None;
                    self.cut.clear();
                    self.transfer_now("move", list, dest, cx);
                } else {
                    self.transfer_now("copy", list, dest, cx);
                }
                return;
            }
            self.server_clip = None;
        }
        let task = cx.background_executor().spawn(async { ferret_core::rt::block_on(ferret_core::shell::clipboard_read_files()) });
        cx.spawn(async move |this, cx| {
            let clip = task.await;
            this.update(cx, |f, cx| match clip {
                Err(e) => f.toast_error(format!("Clipboard error: {e}"), cx),
                Ok(None) => f.toast("Nothing to paste: the clipboard has no files", cx),
                Ok(Some(c)) if c.paths.is_empty() => f.toast("Nothing to paste: the clipboard has no files", cx),
                Ok(Some(c)) => {
                    if c.cut {
                        let sources: Vec<String> = c.paths.into_iter().filter(|p| std::path::Path::new(p).exists() || paths::is_remote(p)).collect();
                        if sources.is_empty() {
                            f.toast("Nothing to paste: the cut items have already been moved", cx);
                            return;
                        }
                        // Like Explorer, a pasted cut leaves the clipboard.
                        let left = sources.clone();
                        f.cut.clear();
                        f.transfer_now("move", sources, dest.clone(), cx);
                        cx.background_executor()
                            .spawn(async move { ferret_core::rt::block_on(ferret_core::shell::clipboard_after_cut(left, Vec::new())) })
                            .detach();
                    } else {
                        f.transfer_now("copy", c.paths, dest.clone(), cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn copy_paths(&mut self, list: &[String], quoted: bool, cx: &mut Context<Self>) {
        let shown: Vec<String> = list
            .iter()
            .map(|p| paths::remote_parts(p).map(|(_, l)| l).unwrap_or_else(|| p.clone()))
            .map(|p| if quoted { format!("\"{p}\"") } else { p })
            .collect();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(shown.join("\r\n")));
        self.toast(if list.len() == 1 { "Copied the path".to_string() } else { format!("Copied {} paths", list.len()) }, cx);
    }

    pub fn copy_names(&mut self, list: &[String], cx: &mut Context<Self>) {
        let names: Vec<String> = list.iter().map(|p| util::file_name(p)).collect();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(names.join("\r\n")));
        self.toast(if list.len() == 1 { "Copied the name".to_string() } else { format!("Copied {} names", list.len()) }, cx);
    }

    // ───────────────────────────── copy and move ─────────────────────────────

    /// Files dropped on a folder (from here or another app): same drive moves, another drive
    /// copies; Ctrl copies, Shift moves. The Recycle Bin deletes; the Dropzone keeps them.
    pub fn drop_paths(&mut self, dropped: Vec<std::path::PathBuf>, dest: String, window: &mut Window, cx: &mut Context<Self>) {
        self.drag_hint = None;
        let sources: Vec<String> = dropped.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        if sources.is_empty() {
            return;
        }
        if let Some(b) = self.bench.as_ref() {
            b.write("drop_in", format!("{} -> {dest}", sources.join("|")));
        }
        if ferret_core::recycle::is_recycle(&dest) {
            self.delete_items(sources, false, cx);
            return;
        }
        if dest == "dropzone" {
            let mut fresh: Vec<String> = Vec::new();
            for s in sources {
                if !self.shelf.iter().chain(fresh.iter()).any(|x| same(x, &s)) {
                    fresh.push(s);
                }
            }
            if fresh.is_empty() {
                return;
            }
            // As the app's `addToShelf`: says what went in.
            let what = if fresh.len() == 1 { util::file_name(&fresh[0]) } else { format!("{} items", fresh.len()) };
            self.shelf.extend(fresh);
            self.save_session();
            self.toast(format!("Added {what} to the Dropzone"), cx);
            cx.notify();
            return;
        }
        let op = self.drop_op(&sources, &dest, window.modifiers());
        if sources.iter().all(|s| util::parent(s).is_some_and(|p| same(&p, &dest))) {
            return;
        }
        self.transfer_to(op, sources, dest, window, cx);
    }

    /// Copy or move, for files dropped on `dest`, as Explorer decides it: Ctrl copies, Shift
    /// moves, otherwise the same drive moves. Files from another app that only lets them be
    /// copied (mail, browsers, editors) are always copied: moving would take them away from it.
    /// Ctrl and Shift are read as they are now: during a drag Windows runs its own loop and
    /// GPUI's `modifiers` can still be what they were when the drag began.
    pub fn drop_op(&self, sources: &[String], dest: &str, m: gpui::Modifiers) -> &'static str {
        if !self.own_drag && !gpui_windows::ferret::drop_allows_move() {
            return "copy";
        }
        let (ctrl, shift) = held_keys();
        let same_volume = sources.iter().all(|s| volume(s) == volume(dest));
        if ctrl || m.control {
            "copy"
        } else if shift || m.shift || same_volume {
            "move"
        } else {
            "copy"
        }
    }

    pub fn transfer_to(&mut self, op: &'static str, sources: Vec<String>, dest: String, _window: &mut Window, cx: &mut Context<Self>) {
        self.transfer_now(op, sources, dest, cx);
    }

    /// Copies or moves into `dest`: asks about name clashes first, runs it as a job, records the
    /// undo step and selects what arrived.
    pub fn transfer_now(&mut self, op: &'static str, sources: Vec<String>, dest: String, cx: &mut Context<Self>) {
        if ferret_core::recycle::is_recycle(&dest) {
            self.delete_items(sources, false, cx);
            return;
        }
        // Where they came from and where they go: counted again when the job ends.
        self.touched_sizes.extend(sources.iter().cloned());
        if !dest.is_empty() {
            self.touched_sizes.push(dest.clone());
        }
        if ferret_core::search::search_query_of(&dest).is_some() {
            self.toast_error("Search tabs show matches from many folders: drop into a folder", cx);
            return;
        }
        let sources: Vec<String> = sources.into_iter().filter(|s| !(op == "move" && util::parent(s).is_some_and(|p| same(&p, &dest)))).collect();
        if sources.is_empty() {
            return;
        }
        if sources.iter().any(|s| util::within(&dest, s)) {
            self.toast_error("Can't put a folder inside itself", cx);
            return;
        }
        // Items inside archives are extracted instead of copied.
        if sources.iter().any(|s| ferret_core::archive::split_virtual(s).is_some() && !std::path::Path::new(s).exists()) {
            let archive = ferret_core::archive::split_virtual(&sources[0]).map(|(a, _)| a).unwrap_or_default();
            self.extract_into(archive, dest, Some(sources), cx);
            return;
        }
        let (s2, d2) = (sources.clone(), dest.clone());
        let task = cx.background_executor().spawn(async move {
            let conflicts = fileops::check_conflicts(s2, d2.clone());
            let no_bin = !paths::is_remote(&d2) && !conflicts.is_empty() && !ferret_core::recycle::no_recycle_bin(vec![d2]).is_empty();
            (conflicts, no_bin)
        });
        cx.spawn(async move |this, cx| {
            let (conflicts, no_bin) = task.await;
            this.update(cx, |f, cx| {
                if conflicts.is_empty() {
                    f.run_transfer(op, sources, dest, Policy::KeepBoth, cx);
                } else {
                    f.ask_conflicts(conflicts, op, dest, sources, no_bin, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn run_transfer(&mut self, op: &'static str, sources: Vec<String>, dest: String, policy: Policy, cx: &mut Context<Self>) {
        let label_dest = util::folder_name(&dest);
        let asked = format!("{} {} to {label_dest}", if op == "move" { "Move" } else { "Copy" }, describe(&sources));
        match fileops::transfer(op.into(), sources.clone(), dest.clone(), policy) {
            Err(e) if denied(&e) => {
                let op_shell = ferret_core::shellops::ShellOp::Transfer { op: op.into(), sources: sources.clone(), dest: dest.clone(), policy };
                self.ask_admin(op_shell, asked, cx);
            }
            Err(e) => self.toast_error(format!("{asked} failed: {e}"), cx),
            Ok(id) => {
                let dest2 = dest.clone();
                when_done(id, move |f, u, cx| {
                    if u.status == "error" {
                        let e = u.error.unwrap_or_default();
                        f.toast_error(format!("{asked} failed: {}", plain_error(&e)), cx);
                        return;
                    }
                    if u.status == "cancelled" {
                        return;
                    }
                    let skipped = u.skipped as usize + u.failed.len();
                    let note = if skipped > 0 { format!(" · skipped {skipped}") } else { String::new() };
                    if u.created.is_empty() && u.replaced.is_empty() && u.pairs.is_empty() {
                        f.toast(if u.failed.is_empty() { format!("Skipped {}: already in {label_dest}", plural(u.skipped as usize, "item")) } else { "Nothing to do".into() }, cx);
                        return;
                    }
                    // What arrived gets selected where it went.
                    if same(&f.pane().path, &dest2) {
                        f.pane_mut().pending_select = Some(u.created.clone());
                    }
                    let done: Vec<String> = sources.iter().filter(|s| !u.failed.iter().any(|x| same(x, s))).cloned().collect();
                    let (created, replaced, pairs) = (u.created.clone(), u.replaced.clone(), u.pairs.clone());
                    let (redo_src, redo_dest) = (done.clone(), dest2.clone());
                    if op == "copy" {
                        f.record(
                            asked.clone(),
                            move |f, cx| {
                                f.recycle_quiet(created.clone(), cx);
                                if !replaced.is_empty() {
                                    f.unrecycle(replaced.clone(), cx);
                                }
                            },
                            move |_, cx| {
                                let _ = fileops::transfer("copy".into(), redo_src.clone(), redo_dest.clone(), policy);
                                cx.notify();
                            },
                            Some(format!("Copied {} to {label_dest}{note}", describe(&done))),
                            cx,
                        );
                    } else {
                        // The Dropzone follows what moved.
                        for (from, to) in &pairs {
                            for s in f.shelf.iter_mut() {
                                if same(s, from) {
                                    *s = to.clone();
                                }
                            }
                        }
                        f.record(
                            asked.clone(),
                            move |f, cx| {
                                let _ = fileops::undo_moves(pairs.clone());
                                if !replaced.is_empty() {
                                    f.unrecycle(replaced.clone(), cx);
                                }
                            },
                            move |f, cx| {
                                let _ = fileops::transfer("move".into(), redo_src.clone(), redo_dest.clone(), policy);
                                let _ = f;
                                cx.notify();
                            },
                            Some(format!("Moved {} to {label_dest}{note}", describe(&done))),
                            cx,
                        );
                    }
                });
            }
        }
        cx.notify();
    }

    pub fn duplicate(&mut self, list: Vec<String>, cx: &mut Context<Self>) {
        let Some(dir) = list.first().and_then(|p| util::parent(p)) else { return };
        self.run_transfer("copy", list, dir, Policy::KeepBoth, cx);
    }

    pub fn transfer_with_picker(&mut self, op: &'static str, list: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose folder".into()) });
        let _ = window;
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(dest) = paths.first() {
                    let dest = dest.to_string_lossy().into_owned();
                    this.update(cx, |f, cx| f.transfer_now(op, list, dest, cx)).ok();
                }
            }
        })
        .detach();
    }

    // ───────────────────────────── delete ─────────────────────────────

    /// Delete: to the Recycle Bin (undoable), or for good after asking (Shift+Delete, or where
    /// there's no Recycle Bin: WSL, network folders, USB sticks, servers).
    pub fn delete_items(&mut self, list: Vec<String>, permanent: bool, cx: &mut Context<Self>) {
        if list.is_empty() {
            return;
        }
        // The folders holding them get smaller: counted again when the job ends.
        self.touched_sizes.extend(list.iter().cloned());
        if list.iter().any(|p| ferret_core::archive::split_virtual(p).is_some() && !std::path::Path::new(p).exists()) {
            self.toast_error("Deleting inside archives isn't supported yet", cx);
            return;
        }
        if self.pane().is_bin() {
            self.purge_from_bin(list, cx);
            return;
        }
        if list.iter().any(|p| paths::is_remote(p)) {
            let what = describe(&list);
            // As the app's `deleteOnServer`: the server by name, "it" or "them".
            let server = ferret_core::paths::remote_parts(&list[0]).map(|(n, _)| ferret_core::remote::name_of(n)).unwrap_or_else(|| "the server".into());
            self.confirm(
                format!("Permanently delete {what}?"),
                format!("This can't be undone: {server} deletes {} right away.", if list.len() == 1 { "it" } else { "them" }),
                "Delete permanently",
                true,
                action(move |f, _, cx| f.run_delete(list.clone(), true, cx)),
                cx,
            );
            return;
        }
        let l2 = list.clone();
        let task = cx.background_executor().spawn(async move { if permanent { Vec::new() } else { ferret_core::recycle::no_recycle_bin(l2) } });
        cx.spawn(async move |this, cx| {
            let no_bin = task.await;
            this.update(cx, |f, cx| {
                if permanent || !no_bin.is_empty() {
                    let rest: Vec<String> = list.iter().filter(|p| !no_bin.contains(p)).cloned().collect();
                    let gone = if permanent { list.clone() } else { no_bin.clone() };
                    if !permanent && !rest.is_empty() {
                        f.run_delete(rest, false, cx);
                    }
                    let place = if paths::is_wsl(&gone[0]) { "WSL" } else if gone[0].starts_with("\\\\") { "A network folder" } else { "This drive" };
                    let body = if permanent { "This can't be undone. The items won't go to the Recycle Bin.".to_string() } else { format!("{place} has no Recycle Bin, so this can't be undone.") };
                    let what = describe(&gone);
                    f.confirm(format!("Permanently delete {what}?"), body, "Delete permanently", true, action(move |f, _, cx| f.run_delete(gone.clone(), true, cx)), cx);
                } else {
                    f.run_delete(list, false, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Deletes (or recycles) `list` in one job. What Windows protects doesn't stop it: the rest
    /// goes, with one toast and one undo step, then one question for everything that needs an
    /// administrator (it used to stop at the first such item, and every "Skip" started a new
    /// job, card, toast and undo step).
    fn run_delete(&mut self, list: Vec<String>, permanent: bool, cx: &mut Context<Self>) {
        // The cursor moves past what's deleted, with nothing selected.
        self.step_past(&list, cx);
        match fileops::delete_items_opts(list.clone(), permanent, true) {
            Err(e) if denied(&e) => {
                let op = ferret_core::shellops::ShellOp::Delete { items: list.clone(), permanent };
                let why = e.starts_with("elevate:").then(|| plain_error(&e));
                self.ask_admin_for(op, format!("Delete {}", describe(&list)), why, None, cx);
            }
            Err(e) => self.toast_error(format!("Delete failed: {}", plain_error(&e)), cx),
            Ok(id) => {
                when_done(id, move |f, u, cx| {
                    // Too big for the bin: ask before deleting for good.
                    if u.status == "error" && u.error.as_deref().is_some_and(|e| e.starts_with("no-bin:")) {
                        let e = u.error.unwrap_or_default();
                        let big = if u.failed.is_empty() { list.clone() } else { u.failed.clone() };
                        let what = describe(&big);
                        f.confirm(
                            format!("Permanently delete {what}?"),
                            format!("{}, so this can't be undone.", e.trim_start_matches("no-bin:")),
                            "Delete permanently",
                            true,
                            action(move |f, _, cx| f.run_delete(big.clone(), true, cx)),
                            cx,
                        );
                        return;
                    }
                    if u.status == "error" {
                        f.toast_error(format!("Delete failed: {}", plain_error(&u.error.clone().unwrap_or_default())), cx);
                    }
                    let left: Vec<String> = u.failed.clone();
                    // What really went. A job that stopped (an error, Cancel) never got to what
                    // came after: "everything but the failed ones" would claim those as moved.
                    let gone = |p: &String| {
                        if paths::is_remote(p) { u.pairs.iter().any(|(a, _)| same(a, p)) } else { !std::path::Path::new(p.as_str()).exists() }
                    };
                    let moved: Vec<String> = if u.status == "done" {
                        list.iter().filter(|p| !left.iter().any(|x| same(x, p))).cloned().collect()
                    } else {
                        list.iter().filter(|p| gone(p)).cloned().collect()
                    };
                    // Left for an administrator (asked about below), and skipped on the job's card.
                    let admin: Vec<String> = u.failed_why.iter().filter(|(_, why)| why.starts_with("elevate:")).map(|(p, _)| p.clone()).collect();
                    let skipped = left.len().saturating_sub(admin.len());
                    let mut note = String::new();
                    if skipped > 0 {
                        note.push_str(&format!(" · {} skipped", plural(skipped, "item")));
                    }
                    if !admin.is_empty() {
                        note.push_str(&format!(" · {} {} an administrator", plural(admin.len(), "item"), if admin.len() == 1 { "needs" } else { "need" }));
                    }
                    if moved.is_empty() {
                        if u.status == "done" && admin.is_empty() {
                            f.toast(format!("Nothing was {}{note}", if permanent { "deleted" } else { "moved to the Recycle Bin" }), cx);
                        }
                    } else if permanent {
                        f.toast(format!("Deleted {}{note}", describe(&moved)), cx);
                    } else {
                        let (u1, r1) = (moved.clone(), moved.clone());
                        f.record(
                            format!("Delete {}", describe(&moved)),
                            move |f, cx| f.unrecycle(u1.clone(), cx),
                            move |f, cx| f.recycle_quiet(r1.clone(), cx),
                            Some(format!("Moved {} to the Recycle Bin{note}", describe(&moved))),
                            cx,
                        );
                    }
                    f.shelf.retain(|s| !moved.iter().any(|m| same(m, s)));
                    if !left.is_empty() {
                        f.pane_mut().pending_select = Some(left);
                    }
                    if !admin.is_empty() {
                        let them = if admin.len() == 1 { "it" } else { "them" };
                        let mut why = format!("{} {} an administrator's permission", names(&admin), if admin.len() == 1 { "needs" } else { "need" });
                        if !moved.is_empty() {
                            why.push_str(if permanent { "; everything else was deleted" } else { "; everything else went to the Recycle Bin" });
                        }
                        why.push_str(&format!(". “Just this action” asks Windows once, for {}; “Skip” leaves {them} where {} {}.", if admin.len() == 1 { "it" } else { "these" }, if admin.len() == 1 { "it" } else { "they" }, if admin.len() == 1 { "is" } else { "are" }));
                        let what = format!("{} {}", if permanent { "Delete" } else { "Move to the Recycle Bin:" }, describe(&admin));
                        let op = ferret_core::shellops::ShellOp::Delete { items: admin, permanent };
                        f.ask_admin_for(op, what, Some(why), Some(action(|_, _, _| {})), cx);
                    }
                });
            }
        }
    }

    fn step_past(&mut self, gone: &[String], cx: &mut Context<Self>) {
        let p = self.pane_mut();
        let order: Vec<usize> = p.visible_rows().collect();
        let is_gone = |i: usize| gone.iter().any(|g| same(g, &p.rows[i].e.path));
        let first = order.iter().position(|&i| is_gone(i));
        if let Some(first) = first {
            let next = order[first..].iter().find(|&&i| !is_gone(i)).or_else(|| order[..first].iter().rev().find(|&&i| !is_gone(i))).copied();
            p.marked.clear();
            p.selected = next;
            if let Some(n) = next {
                p.anchor = n;
            }
        }
        cx.notify();
    }

    /// To the Recycle Bin with no undo step of its own (undoing a copy, a new folder…).
    pub fn recycle_quiet(&mut self, list: Vec<String>, cx: &mut Context<Self>) {
        let list: Vec<String> = list.into_iter().filter(|p| std::path::Path::new(p).exists()).collect();
        if list.is_empty() {
            return;
        }
        if let Err(e) = fileops::delete_items(list, false) {
            self.toast_error(e, cx);
        }
    }

    /// Brings recycled items back (by their original paths).
    pub fn unrecycle(&mut self, originals: Vec<String>, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move {
            // restore_from_trash takes the paths they had: the latest deletion of each comes back.
            ferret_core::rt::block_on(fileops::restore_from_trash(originals))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| {
                match r {
                    Ok(restored) => {
                        if let Some(first) = restored.first() {
                            if let Some(dir) = util::parent(first) {
                                if same(&dir, &f.pane().path) {
                                    f.pane_mut().pending_select = Some(restored.clone());
                                }
                            }
                        }
                    }
                    Err(e) => f.toast_error(format!("Couldn't bring it back: {e}"), cx),
                }
                f.reload_all(cx);
                f.refresh_bin(cx);
            })
            .ok();
        })
        .detach();
    }

    // ───────────────────────────── Recycle Bin ─────────────────────────────

    pub fn restore_from_bin(&mut self, items: Vec<String>, cx: &mut Context<Self>) {
        if items.is_empty() {
            return;
        }
        let n = items.len();
        // Their original paths, for undo (deleting them again).
        let originals: Vec<String> = self
            .pane()
            .entries
            .iter()
            .filter(|e| items.iter().any(|i| same(i, &e.path)))
            .filter_map(|e| e.origin.as_ref().map(|o| format!("{}\\{}", o.trim_end_matches('\\'), e.name)))
            .collect();
        let taken: Vec<String> = originals.iter().filter(|p| std::path::Path::new(p).exists()).cloned().collect();
        let _ = &originals;
        // The Recycle Bin's items (their `$Recycle.Bin` paths) go back through the shell, as
        // Explorer's Restore does; what came back can be recycled again (undo).
        let go = move |f: &mut Ferret, cx: &mut Context<Ferret>| {
            match ferret_core::shellops::shell_op(None, ferret_core::shellops::ShellOp::Restore { items: items.clone() }, false) {
                Ok(id) => when_done(id, move |f, u, cx| {
                    if u.status != "done" {
                        if let Some(e) = u.error {
                            f.toast_error(format!("Restore failed: {e}"), cx);
                        }
                        return;
                    }
                    let back = u.created.clone();
                    let (b1, b2) = (back.clone(), back.clone());
                    let msg = if back.len() == 1 {
                        format!("Restored “{}” to {}", util::file_name(&back[0]), util::parent(&back[0]).map(|p| util::folder_name(&p)).unwrap_or_default())
                    } else {
                        format!("Restored {}", plural(back.len().max(n), "item"))
                    };
                    f.record(format!("Restore {}", plural(back.len(), "item")), move |f, cx| f.recycle_quiet(b1.clone(), cx), move |f, cx| f.unrecycle(b2.clone(), cx), Some(msg), cx);
                    f.refresh_bin(cx);
                }),
                Err(e) => f.toast_error(format!("Restore failed: {e}"), cx),
            }
        };
        if !taken.is_empty() {
            let what = if taken.len() == 1 { format!("“{}” is", util::file_name(&taken[0])) } else { format!("{} are", plural(taken.len(), "name")) };
            let go = Rc::new(go);
            self.confirm(
                "Restore and keep both?".into(),
                format!("{what} already taken where it was deleted from. Windows restores it under a new name."),
                "Keep both",
                false,
                action(move |f, _, cx| go(f, cx)),
                cx,
            );
            return;
        }
        go(self, cx);
    }

    pub fn purge_from_bin(&mut self, items: Vec<String>, cx: &mut Context<Self>) {
        let what = describe(&items);
        self.confirm(
            format!("Permanently delete {what}?"),
            "This can't be undone.".into(),
            "Delete permanently",
            true,
            action(move |f, _, cx| {
                match ferret_core::shellops::shell_op(None, ferret_core::shellops::ShellOp::Purge { items: items.clone() }, false) {
                    Ok(id) => when_done(id, |f, _, cx| f.refresh_bin(cx)),
                    Err(e) => f.toast_error(e, cx),
                }
            }),
            cx,
        );
    }

    pub fn empty_bin(&mut self, cx: &mut Context<Self>) {
        let n = self.bin.map(|b| b.items).unwrap_or(0);
        self.confirm(
            "Empty the Recycle Bin?".into(),
            format!("{} will be deleted permanently. This can't be undone.", plural(n as usize, "item")),
            "Empty Recycle Bin",
            true,
            action(|f, _, cx| match ferret_core::recycle::empty_recycle_bin() {
                Ok(id) => when_done(id, |f, u, cx| {
                    if u.status == "done" {
                        f.toast("The Recycle Bin is empty", cx);
                    }
                    f.refresh_bin(cx);
                }),
                Err(e) => f.toast_error(e, cx),
            }),
            cx,
        );
    }

    // ───────────────────────────── rename ─────────────────────────────

    /// F2: rename in place (the name selected without its extension); several items get the
    /// batch rename dialog.
    pub fn start_rename(&mut self, cx: &mut Context<Self>) {
        let p = self.pane();
        if p.is_bin() {
            self.toast_error("Items in the Recycle Bin can't be renamed: restore them first", cx);
            return;
        }
        let rows = p.marked_rows();
        if rows.is_empty() {
            return;
        }
        if rows.len() > 1 {
            let list: Vec<String> = rows.iter().map(|r| r.e.path.clone()).collect();
            self.open_batch_rename(list, cx);
            return;
        }
        let r = rows[0];
        if r.e.in_archive {
            self.toast_error("Items inside an archive can't be renamed", cx);
            return;
        }
        let (path, name, is_dir) = (r.e.path.clone(), r.e.name.clone(), r.e.is_dir);
        let ix = p.selected.unwrap_or(0);
        let id = p.id;
        let stem = util::stem_len(&name, is_dir);
        // Letters typed while it was being made replace the name.
        let typed = self.type_ahead.take().map(|(t, _)| t).filter(|t| !t.is_empty());
        // In the grid the box wraps the name over lines (the app's textarea); in the list it's one line.
        let grid = self.pane().view.mode == crate::sortgroup::Mode::Grid;
        let input = cx.new(|cx| {
            let mut i = TextInput::new("", cx).sized(if grid { 12. } else { crate::theme::row_text() }, if grid { 16. } else { crate::theme::row_h() - 8. }).named("Rename");
            if grid {
                i = i.wrapping();
            }
            match &typed {
                Some(t) => i.set_text(t, cx),
                None => {
                    i.set_text(&name, cx);
                    i.select(0..stem, cx);
                }
            }
            i
        });
        let dir = util::parent(&path).unwrap_or_default();
        let sub = cx.subscribe(&input, move |f, input, _: &crate::input::Changed, cx| {
            let text = input.read(cx).text().to_string();
            let err = util::bad_name(&text).or_else(|| {
                let clash = !text.eq_ignore_ascii_case(&util::file_name(&f.rename.as_ref().map(|r| r.path.clone()).unwrap_or_default()))
                    && std::path::Path::new(&dir).join(text.trim_end_matches(['.', ' '])).exists();
                clash.then_some("Something with this name is already here")
            });
            if let Some(r) = f.rename.as_mut() {
                r.error = err;
            }
            cx.notify();
        });
        self.rename = Some(Rename { pane: id, path, is_dir, input: input.clone(), error: None, _sub: sub });
        self.pane_ref(id).unwrap().scroll_to(ix);
        // Focus goes to the field on the next frame (it isn't drawn yet).
        crate::dialogs::focus_later(input, cx);
        cx.notify();
    }

    pub fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rename = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Enter, Tab or a click elsewhere: renames (asking first if the extension changes).
    pub fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.rename.take() else { return };
        let text = r.input.read(cx).text().trim_end_matches(['.', ' ']).to_string();
        let old = util::file_name(&r.path);
        if text.is_empty() || text == old {
            cx.notify();
            return;
        }
        if let Some(e) = util::bad_name(&text) {
            self.toast_error(e, cx);
            return;
        }
        let old_ext = if r.is_dir { String::new() } else { old.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default() };
        let new_ext = if r.is_dir { String::new() } else { text.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default() };
        if !r.is_dir && old_ext != new_ext && !old_ext.is_empty() {
            let (path, t1, t2) = (r.path.clone(), text.clone(), text.clone());
            let stem = t2.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(t2.clone());
            let keep = format!("{stem}.{old_ext}");
            let p2 = path.clone();
            self.ask_extension(path, t1, p2, keep, new_ext, cx);
            return;
        }
        self.rename_to(r.path, text, cx);
    }

    pub fn rename_to(&mut self, path: String, name: String, cx: &mut Context<Self>) {
        let (p2, n2) = (path.clone(), name.clone());
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(fileops::rename_item(p2, n2)) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| match r {
                Ok(to) => {
                    let old = util::file_name(&path);
                    let (t1, n1) = (to.clone(), old.clone());
                    let (p1, nn) = (path.clone(), name.clone());
                    f.record(
                        format!("Rename to “{name}”"),
                        move |f, cx| f.rename_quiet(t1.clone(), n1.clone(), cx),
                        move |f, cx| f.rename_quiet(p1.clone(), nn.clone(), cx),
                        None,
                        cx,
                    );
                    for s in f.shelf.iter_mut() {
                        if same(s, &path) {
                            *s = to.clone();
                        }
                    }
                    f.pane_mut().pending_select = Some(vec![to]);
                    let dir = f.pane().path.clone();
                    f.reload_dir(&dir, cx);
                }
                Err(e) if denied(&e) => {
                    let op = ferret_core::shellops::ShellOp::Rename { path: path.clone(), new_name: name.clone() };
                    f.ask_admin(op, format!("Rename to “{name}”"), cx);
                }
                Err(e) => f.toast_error(plain_error(&e), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Undo/redo of a rename: the item is selected again under the name it gets back (as the app).
    fn rename_quiet(&mut self, path: String, name: String, cx: &mut Context<Self>) {
        let dir = util::parent(&path).unwrap_or_default();
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(fileops::rename_item(path, name)) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| {
                match r {
                    Ok(to) => {
                        if same(&f.pane().path, &dir) {
                            f.pane_mut().pending_select = Some(vec![to]);
                        }
                    }
                    Err(e) => f.toast_error(plain_error(&e), cx),
                }
                f.reload_all(cx);
            })
            .ok();
        })
        .detach();
    }

    // ───────────────────────────── new items ─────────────────────────────

    /// Ctrl+Shift+N: a new folder, straight into rename.
    pub fn new_folder(&mut self, cx: &mut Context<Self>) {
        let p = self.pane();
        if p.is_bin() || p.is_search() || p.is_archive() {
            self.toast_error("Can't create items here", cx);
            return;
        }
        let dir = p.path.clone();
        self.type_ahead = Some((String::new(), Instant::now()));
        let d2 = dir.clone();
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(fileops::create_folder(d2, None)) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| match r {
                Ok(path) => {
                    let (u, rd) = (path.clone(), path.clone());
                    f.record(
                        "New folder".into(),
                        move |f, cx| {
                            let p = u.clone();
                            // Still empty: removed outright; otherwise to the Recycle Bin.
                            if std::fs::remove_dir(&p).is_err() {
                                f.recycle_quiet(vec![p], cx);
                            }
                        },
                        move |_, _| {
                            let _ = std::fs::create_dir(&rd);
                        },
                        None,
                        cx,
                    );
                    let p = f.pane_mut();
                    p.pending_select = Some(vec![path]);
                    p.pending_rename = true;
                    f.reload_dir(&dir, cx);
                }
                Err(e) if denied(&e) => {
                    f.ask_admin(ferret_core::shellops::ShellOp::NewFolder { dir: dir.clone(), name: "New folder".into() }, "New folder".into(), cx);
                }
                Err(e) => f.toast_error(plain_error(&e), cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn new_file(&mut self, dir: String, name: &str, content: Option<String>, cx: &mut Context<Self>) {
        let name = name.to_string();
        let (d2, n2) = (dir.clone(), name.clone());
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(fileops::create_file(d2, n2, content)) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| match r {
                Ok(path) => {
                    let (u, rd) = (path.clone(), path.clone());
                    f.record(format!("New {name}"), move |f, cx| f.recycle_quiet(vec![u.clone()], cx), move |f, cx| f.unrecycle(vec![rd.clone()], cx), None, cx);
                    let p = f.pane_mut();
                    p.pending_select = Some(vec![path]);
                    p.pending_rename = true;
                    f.reload_dir(&dir, cx);
                }
                Err(e) => f.toast_error(plain_error(&e), cx),
            })
            .ok();
        })
        .detach();
    }

    // ───────────────────────────── archives ─────────────────────────────

    pub fn extract_here(&mut self, archive: String, cx: &mut Context<Self>) {
        let dest = util::parent(&archive).unwrap_or_default();
        self.extract_into(archive, dest, None, cx);
    }

    /// "Extract to “stem\”" / "Extract all": next to the archive, in a folder of its own unless it
    /// holds a single folder.
    pub fn extract_smart(&mut self, archive: String, cx: &mut Context<Self>) {
        let a2 = archive.clone();
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::archive::archive_extract_smart(a2, None)) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| match r {
                Ok(id) => f.after_extract(id, util::parent(&archive).unwrap_or_default(), cx),
                Err(e) if e.contains("password") => f.ask_password(archive.clone(), None, cx),
                Err(e) => f.toast_error(format!("Extract failed: {e}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn extract_into(&mut self, archive: String, dest: String, entries: Option<Vec<String>>, cx: &mut Context<Self>) {
        let (a2, d2, e2) = (archive.clone(), dest.clone(), entries.clone());
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::archive::archive_extract(a2, d2, e2, None)) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| match r {
                Ok(id) => f.after_extract(id, dest.clone(), cx),
                Err(e) if e.contains("password") => f.ask_password(archive.clone(), Some((dest.clone(), entries.clone())), cx),
                Err(e) => f.toast_error(format!("Extract failed: {e}"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn after_extract(&mut self, id: u64, dest: String, _cx: &mut Context<Self>) {
        when_done(id, move |f, u, cx| {
            if u.status != "done" {
                if let Some(e) = u.error {
                    f.toast_error(format!("Extract failed: {e}"), cx);
                }
                return;
            }
            let created = u.created.clone();
            let c2 = created.clone();
            f.record(
                format!("Extract {}", plural(created.len(), "item")),
                move |f, cx| f.recycle_quiet(c2.clone(), cx),
                move |f, cx| f.unrecycle(created.clone(), cx),
                Some(format!("Extracted to {}", util::folder_name(&dest))),
                cx,
            );
            // The result is shown.
            if let Some(first) = u.created.first() {
                if let Some(dir) = util::parent(first) {
                    if !same(&dir, &f.pane().path) {
                        f.go(dir, cx);
                    }
                    f.pane_mut().pending_select = Some(u.created.clone());
                }
            }
        });
    }

    pub fn extract_to_picked(&mut self, archive: String, entries: Option<Vec<String>>, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Extract here".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(dest) = paths.first() {
                    let dest = dest.to_string_lossy().into_owned();
                    this.update(cx, |f, cx| f.extract_into(archive, dest, entries, cx)).ok();
                }
            }
        })
        .detach();
    }

    /// Compress to “name.zip”: beside the items, then selected and renamed.
    pub fn compress_zip(&mut self, list: Vec<String>, cx: &mut Context<Self>) {
        let Some(dir) = list.first().and_then(|p| util::parent(p)) else { return };
        let name = util::unique_name(&dir, &zip_name_for(&list));
        let dest = format!("{}\\{name}", dir.trim_end_matches('\\'));
        match ferret_core::archive::compress_zip(list, dest.clone()) {
            Ok(id) => when_done(id, move |f, u, cx| {
                if u.status != "done" {
                    return;
                }
                let (d1, d2) = (dest.clone(), dest.clone());
                f.record(format!("Compress to “{name}”"), move |f, cx| f.recycle_quiet(vec![d1.clone()], cx), move |f, cx| f.unrecycle(vec![d2.clone()], cx), None, cx);
                let p = f.pane_mut();
                p.pending_select = Some(vec![dest.clone()]);
                p.pending_rename = true;
                f.reload_dir(&dir, cx);
            }),
            Err(e) => self.toast_error(format!("Compress failed: {e}"), cx),
        }
    }

    // ───────────────────────────── undo ─────────────────────────────

    /// Records an undoable step (and says what happened, with Undo on the toast).
    pub fn record(
        &mut self,
        label: String,
        undo: impl Fn(&mut Ferret, &mut Context<Ferret>) + 'static,
        redo: impl Fn(&mut Ferret, &mut Context<Ferret>) + 'static,
        announce: Option<String>,
        cx: &mut Context<Self>,
    ) {
        static STEPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = STEPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        self.undo.push(Step { id, toast: None, label, undo: Rc::new(undo), redo: Rc::new(redo) });
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
        if let Some(text) = announce {
            // A toast's Undo is for its own step only (the app's `record`): it used to check the
            // stack's length, so after an undo and a new step the old toast undid the new one.
            self.toast_action(text, "Undo", action(move |f, _, cx| {
                if f.undo.last().is_some_and(|s| s.id == id) {
                    f.undo(cx);
                } else if f.undo.iter().any(|s| s.id == id) {
                    f.toast("Undo the later changes first", cx);
                } else {
                    f.toast("Already undone", cx);
                }
            }), cx);
            let toast = self.toast_seq;
            if let Some(s) = self.undo.last_mut().filter(|s| s.id == id) {
                s.toast = Some(toast);
            }
        }
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        let Some(step) = self.undo.pop() else {
            self.toast("Nothing to undo", cx);
            return;
        };
        // Its toast (with an Undo that no longer applies) goes, as in the app.
        if let Some(id) = step.toast {
            self.toasts.retain(|t| t.id != id);
        }
        (step.undo)(self, cx);
        self.toast(format!("Undone: {}", step.label), cx);
        self.redo.push(step);
        self.reload_all(cx);
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        let Some(step) = self.redo.pop() else {
            self.toast("Nothing to redo", cx);
            return;
        };
        (step.redo)(self, cx);
        self.toast(format!("Redone: {}", step.label), cx);
        self.undo.push(step);
        self.reload_all(cx);
    }

    pub fn undo_label(&self) -> String {
        self.undo.last().map(|s| format!("Undo {}", s.label)).unwrap_or_else(|| "Undo".into())
    }

    pub fn redo_label(&self) -> String {
        self.redo.last().map(|s| format!("Redo {}", s.label)).unwrap_or_else(|| "Redo".into())
    }

    // ───────────────────────────── favorites, tools ─────────────────────────────

    pub fn toggle_favorite(&mut self, path: String, is_dir: bool, cx: &mut Context<Self>) {
        let on = self.favorites.iter().any(|f| same(&f.path, &path));
        let r = if on {
            ferret_core::browse::remove_favorite(path.clone())
        } else {
            ferret_core::browse::add_favorite(path.clone(), util::folder_name(&path), is_dir)
        };
        match r {
            Ok(list) => {
                self.favorites = list;
                self.toast(if on { format!("Removed {} from favorites", util::folder_name(&path)) } else { format!("Added {} to favorites", util::folder_name(&path)) }, cx);
            }
            Err(e) => self.toast_error(e, cx),
        }
        cx.notify();
    }

    fn core_call(&mut self, what: &'static str, fut: impl std::future::Future<Output = Result<(), String>> + Send + 'static, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(fut) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                this.update(cx, |f, cx| f.toast_error(format!("{what}: {e}"), cx)).ok();
            }
        })
        .detach();
    }

    pub fn open_terminal(&mut self, path: String, cx: &mut Context<Self>) {
        self.core_call("Couldn't open a terminal", ferret_core::shell::open_terminal(path), cx);
    }

    pub fn open_vscode(&mut self, path: String, cx: &mut Context<Self>) {
        self.core_call("Couldn't open VS Code", ferret_core::shell::open_in_vscode(path), cx);
    }

    pub fn reveal_in_explorer(&mut self, path: String, cx: &mut Context<Self>) {
        if let Err(e) = ferret_core::integration::open_in_explorer(path) {
            self.toast_error(e, cx);
        }
    }

    pub fn open_with(&mut self, path: String, app: String, cx: &mut Context<Self>) {
        self.core_call("Couldn't open it", ferret_core::shell::open_with(path, app), cx);
    }

    pub fn open_with_dialog(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hwnd) = crate::view::hwnd(window) else { return };
        self.core_call("Couldn't open it", ferret_core::shell::open_with_dialog(hwnd, path), cx);
    }

    pub fn invoke_shell_item(&mut self, token: u64, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hwnd) = crate::view::hwnd(window) else { return };
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::shellmenu::shell_menu_invoke(hwnd, token, id)) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| match r {
                Ok(Some(v)) if v == "rename" => f.start_rename(cx),
                Ok(_) => {
                    let p = f.pane().path.clone();
                    f.reload_dir(&p, cx);
                }
                Err(e) => f.toast_error(e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// A new window of the prototype as administrator, at the folder (Windows asks with its
    /// consent prompt). The folder goes as an argument: an elevated program doesn't get the
    /// environment of the one that started it.
    pub fn run_as_admin(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        let Ok(exe) = std::env::current_exe() else { return };
        let dir = path.unwrap_or_else(|| self.pane().path.clone());
        let ps = format!(
            "Start-Process -FilePath '{}' -ArgumentList '\"{}\"' -Verb RunAs",
            exe.to_string_lossy().replace('\'', "''"),
            ferret_core::paths::for_io(&dir).replace('\'', "''")
        );
        let r = std::process::Command::new("powershell.exe").args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps]).spawn();
        if let Err(e) = r {
            self.toast_error(e.to_string(), cx);
        }
    }

    /// "Elevate window": this window restarts as administrator, as the app does it (ferret-core's
    /// `restart_elevated`): after the UAC prompt an elevated copy opens with the same tabs in
    /// the same place, and this one closes once it's on screen.
    pub fn restart_elevated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = crate::view::hwnd(window);
        let Some(at) = owner.and_then(|h| crate::view::placement_of(h, window.is_maximized())) else {
            self.run_as_admin(None, cx);
            return;
        };
        let tabs: Vec<String> = self.tabs.iter().map(|t| t.main.path.clone()).collect();
        let active = self.active;
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::integration::restart_elevated(owner, at, tabs, active)) });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            this.update_in(cx, |f, window, cx| match r {
                Ok(()) => window.remove_window(),
                Err(e) if e == "Cancelled" => {}
                Err(e) => f.toast_error(format!("Couldn't restart as administrator: {e}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// "Administrator permission needed": just this action (UAC through the shell copy engine),
    /// or the whole window.
    pub fn ask_admin(&mut self, op: ferret_core::shellops::ShellOp, what: String, cx: &mut Context<Self>) {
        self.ask_admin_for(op, what, None, None, cx);
    }

    /// "Administrator permission needed", with Windows' reason when there is one (`why`). With
    /// `skip` (the rest of an operation went): "Skip" leaves what needs an administrator.
    pub fn ask_admin_for(&mut self, op: ferret_core::shellops::ShellOp, what: String, why: Option<String>, skip: Option<Action>, cx: &mut Context<Self>) {
        let op = Rc::new(op);
        let o2 = op.clone();
        self.dialog = Some(crate::dialogs::Dialog::Admin {
            what,
            why,
            skip,
            just_this: action(move |f, window, cx| {
                let owner = crate::view::hwnd(window);
                match ferret_core::shellops::shell_op(owner, (*o2).clone(), true) {
                    Ok(id) => when_done(id, |f, u, cx| {
                        if u.status == "done" {
                            f.toast("Done", cx);
                        }
                        f.reload_all(cx);
                    }),
                    Err(e) => f.toast_error(e, cx),
                }
            }),
        });
        let _ = op;
        cx.notify();
    }

    pub fn open_image_editor(&mut self, list: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.show_image_editor(list, window, cx);
    }
}

/// Ctrl and Shift held right now (Windows' key state, which a drag's own loop keeps up to date).
pub fn held_keys() -> (bool, bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
    let down = |vk: i32| unsafe { GetKeyState(vk) } < 0;
    (down(VK_CONTROL.0 as i32), down(VK_SHIFT.0 as i32))
}

/// The drive (or server share) a path is on: dropping on the same one moves.
pub fn volume(p: &str) -> String {
    let l = p.to_lowercase();
    if let Some(rest) = l.strip_prefix("\\\\") {
        let mut it = rest.split('\\');
        return format!("\\\\{}\\{}", it.next().unwrap_or(""), it.next().unwrap_or(""));
    }
    l.chars().take(2).collect()
}

/// The jobs card: each running job with progress, speed and time left, and its question when
/// it's stuck on an item.
pub fn jobs_view(f: &Ferret, cx: &mut Context<Ferret>) -> Option<AnyElement> {
    let shown: Vec<&JobCard> = f.jobs.iter().filter(|j| j.shown).collect();
    // An update on offer (or downloading) is a card here too, above the jobs.
    let update = f.update_card(cx);
    if shown.is_empty() && update.is_none() {
        return None;
    }
    Some(
        div()
            .absolute()
            .right(px(18.))
            .bottom(px(52.))
            .w(px(340.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(update)
            .children(shown.into_iter().map(|j| {
                let by_bytes = j.total > 0;
                let frac = if by_bytes {
                    j.done as f32 / j.total as f32
                } else if j.files_total > 0 {
                    j.files_done as f32 / j.files_total as f32
                } else if j.status == "done" {
                    1.
                } else {
                    -1.
                };
                let secs = j.started.elapsed().as_secs_f32().max(0.1);
                let speed = if by_bytes {
                    format!("{}/s", crate::kinds::format_size((j.done as f32 / secs) as u64))
                } else {
                    format!("{:.0} items/s", j.files_done as f32 / secs)
                };
                let left = if frac > 0.02 && frac < 1. {
                    let rest = secs / frac * (1. - frac);
                    // Under a second says nothing (it read "0s left"), as the app's `timeLeft`.
                    if rest < 1. {
                        String::new()
                    } else if rest > 90. {
                        format!(" · {} min left", (rest / 60.).ceil())
                    } else {
                        format!(" · {}s left", rest.ceil())
                    }
                } else {
                    String::new()
                };
                // An error is said in full on the line below, wrapped: in the title row it ran out of
                // the card (it doesn't shrink there).
                let error = (j.status == "error").then(|| j.error.clone().map(|e| plain_error(&e))).flatten();
                // Stopped for an administrator: not a failure, a question (the dialog asks it).
                let needs_admin = j.status == "error" && j.error.as_deref().is_some_and(|e| e.starts_with("elevate:"));
                let (status, color) = match j.status.as_str() {
                    "done" => ("Done".to_string(), t::ok()),
                    "error" if needs_admin => ("Needs administrator".to_string(), t::warn()),
                    "error" => ("Failed".to_string(), t::danger()),
                    "cancelled" => ("Cancelled".to_string(), t::fg3()),
                    _ if frac >= 0. => (format!("{:.0}%", frac * 100.), t::fg2()),
                    _ => (String::new(), t::fg2()),
                };
                let stuck = j.current_since.elapsed() > Duration::from_secs(3) && !j.current.is_empty();
                let line: SharedString = if j.question.is_some() {
                    // Not stuck on the last item: paused for the question below.
                    "Waiting for your answer".into()
                } else if !j.note.is_empty() {
                    j.note.clone().into()
                } else if stuck {
                    format!("Still on “{}”…", util::file_name(&j.current)).into()
                } else if j.current.is_empty() {
                    "Starting…".into()
                } else {
                    j.current.clone().into()
                };
                let id = j.id;
                let kind_icon = match j.kind.as_str() {
                    "move" => "folder-input",
                    "delete" | "recycle" => "trash-2",
                    "extract" => "package-open",
                    "compress" => "archive",
                    "media" => "clapperboard",
                    // An add-on being fetched (addons.rs).
                    "download" => "download",
                    _ => "copy",
                };
                // To a screen reader: a progress bar named by the job, its percent as the value,
                // and the end ("Done", an error) read out as it happens.
                let finished = matches!(j.status.as_str(), "done" | "error" | "cancelled");
                let card = div()
                    .id(("job", id as usize))
                    .role(gpui::Role::ProgressIndicator)
                    .aria_label(j.label.clone())
                    .when(frac >= 0., |d| d.aria_numeric_value((frac * 100.).round() as f64).aria_min_numeric_value(0.).aria_max_numeric_value(100.))
                    .aria_description(status.clone())
                    .when(finished, |d| d.a11y_synthetic_children(|b| b.parent_node().set_live(gpui::accesskit::Live::Polite)))
                    .p(px(12.))
                    .rounded(px(10.))
                    .bg(t::raised())
                    .border_1()
                    .border_color(t::line2())
                    .shadow(t::shadow())
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(icon(kind_icon, 15., t::accent_text()))
                            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(j.label.clone()))
                            .child(div().flex_none().text_size(px(12.)).text_color(color).child(status))
                            .when(j.status == "running" || j.status == "waiting", |d| {
                                d.child(
                                    div()
                                        .id(("job-cancel", id as usize))
                                        .p(px(3.))
                                        .rounded(px(5.))
                                        .hover(|s| s.bg(t::press()))
                                        .tooltip(ui::tip("Cancel"))
                                        .on_click(cx.listener(move |_, _, _, _| ferret_core::jobs::cancel_job(id)))
                                        .child(icon("x", 13., t::fg3())),
                                )
                            }),
                    )
                    .child(
                        div().h(px(4.)).rounded(px(4.)).bg(t::line2()).my(px(8.)).overflow_hidden().child(if frac >= 0. {
                            div().h_full().w(relative(frac.clamp(0., 1.))).rounded(px(4.)).bg(t::accent()).into_any_element()
                        } else {
                            // Not started: a sweep.
                            div()
                                .h_full()
                                .w(relative(0.3))
                                .rounded(px(4.))
                                .bg(t::accent())
                                .with_animation(("sweep", id as usize), gpui::Animation::new(Duration::from_millis(1100)).repeat(), |d, k| d.ml(relative(k * 0.7)))
                                .into_any_element()
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .text_size(px(11.5))
                            .text_color(t::fg3())
                            .child(match error.clone() {
                                Some(e) => div().flex_1().min_w_0().text_color(if needs_admin { t::warn() } else { t::danger() }).child(e),
                                None => div().flex_1().min_w_0().truncate().child(line),
                            })
                            .when(j.status == "running" && frac > 0., |d| d.child(div().flex_none().child(format!("{speed}{left}")))),
                    )
                    .when_some(j.question.clone(), |d, q| {
                        let btn = |label: &'static str, answer: &'static str| {
                            ui::button(SharedString::from(format!("job-{id}-{answer}")), label, answer == "retry")
                                .h(px(28.))
                                .on_click(move |_, _, _| ferret_core::jobs::answer_job(id, answer.into()))
                        };
                        d.child(div().mt(px(8.)).text_size(px(12.)).text_color(t::warn()).child(q)).child(
                            div().mt(px(8.)).flex().gap(px(6.)).child(btn("Try again", "retry")).child(btn("Skip", "skip")).child(btn("Skip all", "skipAll")).child(btn("Cancel", "cancel")),
                        )
                    });
                if t::animate() {
                    card.with_animation(("job-in", id as usize), gpui::Animation::new(Duration::from_millis(160)).with_easing(gpui::ease_out_quint()), |d, k| d.opacity(k).mt(px((1. - k) * 8.)))
                        .into_any_element()
                } else {
                    card.into_any_element()
                }
            }))
            .into_any_element(),
    )
}

