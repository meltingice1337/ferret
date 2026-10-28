//! File operations: create, rename, copy/move with progress and conflict
//! policies, delete (Recycle Bin or permanent), restore from Recycle Bin (undo),
//! properties and small reads for previews.

use serde::Serialize;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tauri::AppHandle;

use crate::jobs::{self, JobCtx};
use crate::listing::join;
use crate::paths;

type Res<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn io(p: &str) -> PathBuf {
    PathBuf::from(paths::for_io(p))
}

/// Errors starting with "elevate:" make the UI offer to continue as administrator
/// (the operation is then re-run through the shell, which shows the UAC prompt).
fn admin_err(what: String) -> String {
    format!("elevate:You'll need administrator permission to {what}")
}

fn denied(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
}

fn file_name(p: &str) -> String {
    let n = paths::normalize(p);
    n.rsplit('\\').next().unwrap_or(&n).to_string()
}

const INVALID: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub fn validate_name(name: &str) -> Res<()> {
    let n = name.trim();
    if n.is_empty() || n == "." || n == ".." {
        return Err("Name can't be empty".into());
    }
    if let Some(c) = n.chars().find(|c| INVALID.contains(c) || (*c as u32) < 32) {
        return Err(format!("A name can't contain {c:?}"));
    }
    let stem = n.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return Err(format!("“{n}” is reserved by Windows"));
    }
    if n.ends_with('.') || n.ends_with(' ') {
        return Err("A name can't end with a dot or space".into());
    }
    Ok(())
}

/// `name (2).ext`, `name (3).ext`, … — first one that doesn't exist in `dir`.
pub fn unique_name(dir: &str, name: &str, copy_style: bool) -> String {
    if !io(&join(dir, name)).exists() && fs::symlink_metadata(io(&join(dir, name))).is_err() {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let candidates = (1..10_000).map(|i| {
        if copy_style {
            if i == 1 { format!("{stem} - Copy{ext}") } else { format!("{stem} - Copy ({i}){ext}") }
        } else {
            format!("{stem} ({}){ext}", i + 1)
        }
    });
    for c in candidates {
        if fs::symlink_metadata(io(&join(dir, &c))).is_err() {
            return c;
        }
    }
    format!("{stem} ({}){ext}", std::process::id())
}

// ---------- create / rename ----------

#[tauri::command]
pub async fn create_folder(dir: String, name: Option<String>) -> Res<String> {
    let name = name.unwrap_or_else(|| "New folder".into());
    if paths::is_remote(&dir) {
        return crate::remote::fs::create_folder(&dir, &name).await;
    }
    validate_name(&name)?;
    let name = unique_name(&dir, &name, false);
    let p = join(&paths::normalize(&dir), &name);
    fs::create_dir(io(&p)).map_err(|e| {
        if denied(&e) && crate::shellops::needs_admin_write(&dir) { admin_err(format!("create a folder in “{}”", file_name(&dir))) } else { err(e) }
    })?;
    Ok(p)
}

/// Undo of "New folder": removes the folder only if it's still empty (nothing to lose, so it
/// doesn't clutter the Recycle Bin). False when there's something in it, or it can't go.
#[tauri::command]
pub async fn remove_empty_folder(path: String) -> bool {
    if paths::is_remote(&path) {
        return crate::remote::fs::remove_empty_folder(&path).await;
    }
    fs::remove_dir(io(&paths::normalize(&path))).is_ok()
}

#[tauri::command]
pub async fn create_file(dir: String, name: String, content: Option<String>) -> Res<String> {
    if paths::is_remote(&dir) {
        return crate::remote::fs::create_file(&dir, &name, content).await;
    }
    validate_name(&name)?;
    let name = unique_name(&dir, &name, false);
    let p = join(&paths::normalize(&dir), &name);
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(io(&p)).map_err(|e| {
        if denied(&e) && crate::shellops::needs_admin_write(&dir) { admin_err(format!("create a file in “{}”", file_name(&dir))) } else { err(e) }
    })?;
    if let Some(c) = content {
        f.write_all(c.as_bytes()).map_err(err)?;
    }
    Ok(p)
}

#[tauri::command]
pub async fn rename_item(path: String, new_name: String) -> Res<String> {
    if paths::is_remote(&path) {
        return crate::remote::fs::rename(&path, &new_name).await;
    }
    let new_name = new_name.trim().to_string();
    validate_name(&new_name)?;
    let src = paths::normalize(&path);
    let dir = paths::parent(&src).ok_or("Can't rename a drive")?;
    let dst = join(&dir, &new_name);
    if dst == src {
        return Ok(dst);
    }
    // Case-only renames are allowed; anything else must not clobber.
    if paths::key(&dst) != paths::key(&src) && fs::symlink_metadata(io(&dst)).is_ok() {
        return Err(format!("“{new_name}” already exists here"));
    }
    fs::rename(io(&src), io(&dst)).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied if crate::shellops::needs_admin_delete(&src) || crate::shellops::needs_admin_write(&dir) => {
            admin_err(format!("rename “{}”", file_name(&src)))
        }
        std::io::ErrorKind::PermissionDenied => "Access denied: the item may be open in another program".to_string(),
        _ => e.to_string(),
    })?;
    log::info!("renamed \"{src}\" -> \"{dst}\"");
    Ok(dst)
}

// ---------- conflicts ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    name: String,
    source: String,
    target: String,
    source_size: u64,
    target_size: u64,
    source_modified: Option<i64>,
    target_modified: Option<i64>,
    is_dir: bool,
}

fn ms(m: &fs::Metadata) -> Option<i64> {
    m.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

impl Conflict {
    /// From the file systems' own descriptions (servers: remote/ops.rs).
    pub fn new(name: String, source: String, target: String, sm: &vfs::Meta, tm: &vfs::Meta) -> Self {
        let t = |m: &vfs::Meta| m.modified.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64);
        Conflict {
            name,
            is_dir: sm.is_dir,
            source_size: sm.len,
            target_size: tm.len,
            source_modified: t(sm),
            target_modified: t(tm),
            source,
            target,
        }
    }
}

/// Items in `sources` whose name already exists in `dest` (ignores moving onto itself).
// Off the main thread: with thousands of items this can take a moment, and the window
// must keep answering meanwhile.
#[tauri::command(async)]
pub fn check_conflicts(sources: Vec<String>, dest: String) -> Vec<Conflict> {
    if crate::remote::ops::involves(&sources, Some(&dest)) {
        return crate::remote::ops::conflicts(&sources, &dest);
    }
    let dest = paths::normalize(&dest);
    sources
        .iter()
        .filter_map(|s| {
            let s = paths::normalize(s);
            if paths::parent(&s).map(|p| paths::key(&p)) == Some(paths::key(&dest)) {
                return None;
            }
            let name = file_name(&s);
            let target = join(&dest, &name);
            let tm = fs::metadata(io(&target)).ok()?;
            let sm = fs::metadata(io(&s)).ok()?;
            Some(Conflict {
                name,
                is_dir: sm.is_dir(),
                source_size: if sm.is_dir() { 0 } else { sm.len() },
                target_size: if tm.is_dir() { 0 } else { tm.len() },
                source_modified: ms(&sm),
                target_modified: ms(&tm),
                source: s,
                target,
            })
        })
        .collect()
}

// ---------- copy / move ----------

pub use crate::vfs::Policy;
use crate::vfs::{self, Local};

/// Whether a new folder can be created in `dir` (creates and removes a probe folder).
fn can_create_folder(dir: &str) -> bool {
    let probe = io(&join(dir, &format!(".ferret-probe-{}", std::process::id())));
    match fs::create_dir(&probe) {
        Ok(()) => {
            let _ = fs::remove_dir(&probe);
            true
        }
        Err(e) => e.kind() == std::io::ErrorKind::AlreadyExists,
    }
}

fn same_volume(a: &str, b: &str) -> bool {
    let root = |p: &str| -> String {
        let k = paths::key(p);
        if let Some(rest) = k.strip_prefix(r"\\") {
            rest.split('\\').take(2).collect::<Vec<_>>().join("\\").to_lowercase()
        } else {
            k.chars().take(2).collect()
        }
    };
    root(a) == root(b)
}

fn remove_any(p: &Path) -> std::io::Result<()> {
    let m = fs::symlink_metadata(p)?;
    if m.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

/// Copy or move `sources` into `dest`. Returns a job id; progress via `job-update`.
// Off the main thread: with thousands of items this can take a moment, and the window
// must keep answering meanwhile.
#[tauri::command(async)]
pub fn transfer(app: AppHandle, op: String, sources: Vec<String>, dest: String, policy: Policy) -> Res<u64> {
    let moving = op == "move";
    if crate::remote::ops::involves(&sources, Some(&dest)) {
        return crate::remote::ops::transfer(&app, moving, sources, dest, policy);
    }
    let dest = paths::normalize(&dest);
    if !io(&dest).is_dir() {
        return Err(format!("{dest} is not a folder"));
    }
    for s in &sources {
        let k = paths::key(s);
        let dk = paths::key(&dest);
        if dk == k || dk.starts_with(&format!("{k}\\")) {
            return Err(format!("Can't {} “{}” into itself", if moving { "move" } else { "copy" }, file_name(s)));
        }
    }
    // Protected destinations (Program Files…) and sources: ask before doing anything.
    // Drive roots let users create folders but not files, so folders get their own probe.
    let only_folders = sources.iter().all(|s| io(s).is_dir());
    if crate::shellops::needs_admin_write(&dest) && !(only_folders && can_create_folder(&dest)) {
        return Err(admin_err(format!("{} to “{}”", if moving { "move" } else { "copy" }, file_name(&dest))));
    }
    if moving {
        if let Some(s) = sources.iter().take(64).find(|s| crate::shellops::needs_admin_delete(s)) {
            return Err(admin_err(format!("move “{}”", file_name(s))));
        }
    }
    let label = format!(
        "{} {} to {}",
        if moving { "Moving" } else { "Copying" },
        if sources.len() == 1 { format!("“{}”", file_name(&sources[0])) } else { format!("{} items", sources.len()) },
        file_name(&dest)
    );

    log::info!("{} to \"{dest}\" ({policy:?}): {}", if moving { "move" } else { "copy" }, crate::logging::paths(&sources));
    Ok(jobs::spawn(&app, if moving { "move" } else { "copy" }, &label, move |ctx| {
        let sources: Vec<String> = sources.iter().map(|s| paths::normalize(s)).collect();

        // Like the delete job: an item that can't be moved or copied (open in another program,
        // say) pauses the job with Try again / Skip / Skip all / Cancel; the others still go.
        let mut skip_all = false;
        // Fast path: same-volume moves are renames.
        let mut pending = Vec::new();
        for s in &sources {
            let same_dir = paths::parent(s).map(|p| paths::key(&p)) == Some(paths::key(&dest));
            if moving && same_dir {
                continue; // moving onto itself: nothing to do
            }
            let name = file_name(s);
            let mut target = join(&dest, &name);
            let exists = fs::symlink_metadata(io(&target)).is_ok();
            if (exists && policy == Policy::KeepBoth) || (!moving && same_dir) {
                target = join(&dest, &unique_name(&dest, &name, !moving && same_dir));
            } else if exists && policy == Policy::Skip {
                ctx.skipped();
                continue;
            }
            if moving && same_volume(s, &dest) {
                let target_exists = fs::symlink_metadata(io(&target)).is_ok();
                let src_is_dir = io(s).is_dir();
                if !target_exists || (policy == Policy::Replace && !src_is_dir) {
                    ctx.set_current(&name);
                    // A file is renamed or it isn't moved: copying one that can't be renamed (open
                    // in another program) would only leave a second copy behind.
                    let mut copy_instead = false;
                    let moved = per_item(ctx, &mut skip_all, s, || {
                        if !src_is_dir && let Some(why) = move_blocker(&io(s)) {
                            return Err(why);
                        }
                        if fs::symlink_metadata(io(&target)).is_ok() {
                            vfs::displace(&Local, &target, ctx)?;
                        }
                        match fs::rename(io(s), io(&target)) {
                            Ok(()) => Ok(()),
                            // Another volume mounted in a folder: copy it over instead.
                            Err(e) if src_is_dir || e.raw_os_error() == Some(17) => {
                                copy_instead = true;
                                Ok(())
                            }
                            Err(e) => Err(move_blocker(&io(s)).unwrap_or_else(|| format!("Couldn't move “{name}”: {e}"))),
                        }
                    })?;
                    if !moved {
                        continue;
                    }
                    if !copy_instead {
                        ctx.pair(s.clone(), target.clone());
                        ctx.created(target);
                        continue;
                    }
                }
            }
            pending.push((s.clone(), target));
        }
        if pending.is_empty() {
            ctx.set_totals(0, 0);
            return Ok(());
        }

        let (mut bytes, mut files) = (0u64, 0u64);
        for (s, _) in &pending {
            vfs::totals(&Local, s, &mut bytes, &mut files, ctx)?;
        }
        ctx.set_totals(bytes, files);

        for (s, target) in pending {
            let (src, tp) = (io(&s), io(&target));
            let src_is_dir = src.is_dir();
            let mut copied = false;
            per_item(ctx, &mut skip_all, &s, || {
                if !copied {
                    // Moving a file that can't be removed afterwards would leave two copies.
                    if moving && !src_is_dir && let Some(why) = move_blocker(&src) {
                        return Err(why);
                    }
                    // A file replacing a folder (or vice versa) can't merge: displace the whole item.
                    if policy == Policy::Replace && tp.exists() && (src_is_dir != tp.is_dir() || !src_is_dir) {
                        vfs::displace(&Local, &target, ctx)?;
                    }
                    vfs::copy_tree(&Local, &s, &Local, &target, policy, ctx, true)?;
                    copied = true;
                }
                // Try again only retries the removal: the copy is already there.
                if moving {
                    remove_any(&src).map_err(|e| match move_blocker(&src) {
                        Some(why) => format!("Copied, but couldn't remove the original: {why}"),
                        None if src_is_dir => format!("Copied, but couldn't remove all of “{}”: something inside it is open in another program", file_name(&s)),
                        None => format!("Copied, but couldn't remove “{}”: {e}", file_name(&s)),
                    })?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }))
}

/// Runs one item of a job, asking Try again / Skip / Skip all / Cancel when it fails (Skip all:
/// the rest fail without asking). Ok(false) when the item was skipped; Err when the job is cancelled.
pub(crate) fn per_item(ctx: &JobCtx, skip_all: &mut bool, item: &str, mut run: impl FnMut() -> Res<()>) -> Res<bool> {
    loop {
        ctx.check()?;
        let Err(why) = run() else { return Ok(true) };
        // Cancelled while it ran: nothing to ask about.
        ctx.check()?;
        if *skip_all {
            ctx.failed(item, why);
            return Ok(false);
        }
        match ctx.ask(why.clone()).as_str() {
            "retry" => continue,
            "skip" => {
                ctx.failed(item, why);
                return Ok(false);
            }
            "skipAll" => {
                *skip_all = true;
                ctx.failed(item, why);
                return Ok(false);
            }
            _ => {
                ctx.mark_cancelled();
                return Err("Cancelled".into());
            }
        }
    }
}

/// Whether a file can't be moved away from where it is because it's open in another program
/// that doesn't allow that (asking Windows for delete access tells, without changing anything).
/// Anything else is left to the move itself: shares and WSL answer such probes unevenly.
fn move_blocker(p: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const DELETE: u32 = 0x0001_0000;
        const SHARE_ALL: u32 = 0x7;
        if let Err(e) = fs::OpenOptions::new().access_mode(DELETE).share_mode(SHARE_ALL).open(p)
            && matches!(e.raw_os_error(), Some(32) | Some(33))
        {
            let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            return Some(format!("“{name}” is open in another program"));
        }
    }
    #[cfg(not(windows))]
    let _ = p;
    None
}

/// Undo for a move: puts each moved item back where it came from.
// Off the main thread: with thousands of items this can take a moment, and the window
// must keep answering meanwhile.
#[tauri::command(async)]
pub fn undo_moves(app: AppHandle, pairs: Vec<(String, String)>) -> Res<u64> {
    if pairs.iter().any(|(a, b)| paths::is_remote(a) || paths::is_remote(b)) {
        return crate::remote::ops::undo_moves(&app, pairs);
    }
    if let Some((from, _)) = pairs.iter().take(64).find(|(from, to)| {
        crate::shellops::needs_admin_delete(to) || paths::parent(from).is_some_and(|d| crate::shellops::needs_admin_write(&d))
    }) {
        return Err(admin_err(format!("move “{}” back", file_name(from))));
    }
    let label = if pairs.len() == 1 { format!("Moving “{}” back", file_name(&pairs[0].0)) } else { format!("Moving {} items back", pairs.len()) };
    log::info!("undo move: {}", crate::logging::paths(&pairs.iter().map(|(from, to)| format!("{to} -> {from}")).collect::<Vec<_>>()));
    Ok(jobs::spawn(&app, "move", &label, move |ctx| {
        ctx.set_totals(0, pairs.len() as u64);
        let mut blocked = Vec::new();
        for (from, to) in pairs.iter().rev() {
            ctx.check()?;
            let (f, t) = (io(from), io(to));
            if fs::symlink_metadata(&t).is_err() {
                continue; // already gone (moved/deleted since)
            }
            if fs::symlink_metadata(&f).is_ok() {
                blocked.push(file_name(from));
                continue;
            }
            if let Some(p) = f.parent() {
                fs::create_dir_all(p).map_err(err)?;
            }
            ctx.set_current(&file_name(from));
            if fs::rename(&t, &f).is_err() {
                vfs::copy_tree(&Local, to, &Local, from, Policy::KeepBoth, ctx, false)?;
                remove_any(&t).map_err(err)?;
            }
            ctx.created(from.clone());
            ctx.file_done();
        }
        if blocked.is_empty() {
            Ok(())
        } else {
            Err(format!("Something with the same name is back at the original location: {}", blocked.join(", ")))
        }
    }))
}

// ---------- delete / restore ----------

/// Items recycled per Windows operation (one operation per item is slow for thousands).
const RECYCLE_BATCH: usize = 64;

// Off the main thread: with thousands of items this can take a moment, and the window
// must keep answering meanwhile.
#[tauri::command(async)]
pub fn delete_items(app: AppHandle, items: Vec<String>, permanent: bool) -> Res<u64> {
    if crate::remote::ops::involves(&items, None) {
        return crate::remote::ops::delete(&app, items, permanent);
    }
    if let Some(p) = items.iter().take(256).find(|p| crate::shellops::needs_admin_delete(p)) {
        return Err(admin_err(format!("delete “{}”", file_name(p))));
    }
    let label = if items.len() == 1 {
        format!("{} “{}”", if permanent { "Deleting" } else { "Recycling" }, file_name(&items[0]))
    } else {
        format!("{} {} items", if permanent { "Deleting" } else { "Recycling" }, items.len())
    };
    log::info!("{}: {}", if permanent { "delete permanently" } else { "recycle" }, crate::logging::paths(&items));
    Ok(jobs::spawn(&app, if permanent { "delete" } else { "recycle" }, &label, move |ctx| {
        ctx.set_totals(0, items.len() as u64);
        // Like Explorer: an item that can't go (open in another program, protected) pauses the
        // job with Try again / Skip / Skip all / Cancel instead of failing the whole job.
        let mut skip_all = false;
        let job_start = std::time::Instant::now();
        let (mut fast_ok, mut fast_failed, mut slowest) = (0usize, 0usize, (std::time::Duration::ZERO, String::new()));
        for batch in recycle_units(&items, permanent).iter() {
            ctx.check()?;
            let started = std::time::Instant::now();
            // Fast path: a group of files in one Windows operation. It stops at the first item
            // that can't go; the group is then retried one by one below.
            if batch.len() > 1 {
                ctx.set_current(&file_name(&batch[0]));
                match trash::delete_all(batch.iter().map(|p| io(p))) {
                    Ok(()) => {
                        fast_ok += 1;
                        batch.iter().for_each(|_| ctx.file_done());
                        if started.elapsed() > SLOW {
                            log::info!("recycle: {} files from “{}” took {:?}", batch.len(), file_name(&batch[0]), started.elapsed());
                        }
                        continue;
                    }
                    Err(e) => {
                        fast_failed += 1;
                        log::info!("recycle: group of {} from “{}” stopped after {:?} ({e}); one by one", batch.len(), file_name(&batch[0]), started.elapsed());
                    }
                }
            }
            for p in batch.iter() {
                ctx.check()?;
                let path = io(p);
                // Already recycled by the batch attempt before it stopped (or gone meanwhile).
                if fs::symlink_metadata(&path).is_err() {
                    ctx.file_done();
                    continue;
                }
                ctx.set_current(&file_name(p));
                let item_start = std::time::Instant::now();
                loop {
                    let res = if permanent {
                        remove_any(&path).map_err(|e| format!("Couldn't delete “{}”: {e}", file_name(p)))
                    } else {
                        trash::delete(&path).map_err(|e| explain_delete_failure(&path, &file_name(p), &e.to_string()))
                    };
                    let Err(why) = res else { break };
                    if skip_all {
                        ctx.failed(p.clone(), why);
                        break;
                    }
                    match ctx.ask(why.clone()).as_str() {
                        "retry" => continue,
                        "skip" => ctx.failed(p.clone(), why),
                        "skipAll" => {
                            skip_all = true;
                            ctx.failed(p.clone(), why);
                        }
                        _ => {
                            ctx.mark_cancelled();
                            return Err("Cancelled".into());
                        }
                    }
                    break;
                }
                // Slow items, to see what a long job spent its time on (includes any wait for
                // the user's answer).
                let took = item_start.elapsed();
                if took > SLOW {
                    log::info!("{}: “{}” took {took:?}", if permanent { "delete" } else { "recycle" }, file_name(p));
                }
                if took > slowest.0 {
                    slowest = (took, file_name(p));
                }
                ctx.file_done();
            }
        }
        log::info!(
            "{}: {} items in {:?}; {fast_ok} file groups in one go, {fast_failed} redone one by one; slowest “{}” {:?}",
            if permanent { "delete" } else { "recycle" }, items.len(), job_start.elapsed(), slowest.1, slowest.0
        );
        Ok(())
    }))
}

/// Anything slower than this gets a log line (where did a long delete spend its time?).
const SLOW: std::time::Duration = std::time::Duration::from_secs(1);

/// The delete job's units of work: files go in groups (one Windows operation each, fast for
/// thousands), folders one at a time. A folder can take long (Windows goes through everything
/// inside it), so the progress names it, and one locked folder doesn't send a whole group
/// down the slow path. Deleting permanently goes item by item.
fn recycle_units(items: &[String], permanent: bool) -> Vec<Vec<String>> {
    let mut units: Vec<Vec<String>> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for p in items {
        let is_dir = fs::symlink_metadata(io(p)).map(|m| m.is_dir()).unwrap_or(false);
        if permanent || is_dir {
            if !files.is_empty() {
                units.push(std::mem::take(&mut files));
            }
            units.push(vec![p.clone()]);
        } else {
            files.push(p.clone());
            if files.len() == RECYCLE_BATCH {
                units.push(std::mem::take(&mut files));
            }
        }
    }
    if !files.is_empty() {
        units.push(files);
    }
    units
}

/// Turns the Recycle Bin's vague failures ("Some operations were aborted") into a
/// reason the user can act on, by probing the item directly.
fn explain_delete_failure(path: &Path, name: &str, raw: &str) -> String {
    if fs::symlink_metadata(path).is_err() {
        return format!("“{name}” no longer exists");
    }
    if path.is_file() {
        if let Err(e) = fs::OpenOptions::new().read(true).write(true).open(path) {
            match e.raw_os_error() {
                Some(32) | Some(33) => return format!("“{name}” is open in another program"),
                Some(5) => return format!("Access denied: you can't delete “{name}”"),
                _ => {}
            }
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                return format!("“{name}” is read-only or protected");
            }
        }
    } else if path.is_dir() {
        // A locked file somewhere inside usually blocks recycling the folder.
        return format!("Couldn't recycle “{name}”: something inside it is open in another program");
    }
    format!("Couldn't recycle “{name}”: {raw}")
}

/// The most recently recycled item for each original path.
#[cfg(windows)]
fn latest_in_trash(items: &[String]) -> Res<Vec<trash::TrashItem>> {
    let wanted: std::collections::HashSet<String> = items.iter().map(|p| paths::key(&paths::long_form(p))).collect();
    let mut all = trash::os_limited::list().map_err(err)?;
    all.sort_by_key(|i| std::cmp::Reverse(i.time_deleted));
    let mut seen = std::collections::HashSet::new();
    let pick: Vec<_> = all
        .into_iter()
        .filter(|i| {
            let k = paths::key(&i.original_path().to_string_lossy());
            wanted.contains(&k) && seen.insert(k)
        })
        .collect();
    if pick.is_empty() {
        return Err("Nothing to restore: the items are no longer in the Recycle Bin".into());
    }
    Ok(pick)
}

/// Undo for "move to Recycle Bin": restores the most recently deleted item for each original path.
#[tauri::command]
pub async fn restore_from_trash(items: Vec<String>) -> Res<Vec<String>> {
    // A server's trash (items a copy replaced there).
    if items.iter().any(|p| paths::is_remote(p)) {
        return crate::remote::fs::restore_by_original(&items).await;
    }
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(windows)]
        {
            let pick = latest_in_trash(&items)?;
            let original = |i: &trash::TrashItem| paths::normalize(&i.original_path().to_string_lossy());
            let mut restored: Vec<String> = pick.iter().map(original).collect();
            // Restoring into a protected folder needs the elevated path.
            if let Some(dir) = pick.iter().map(|i| paths::normalize(&i.original_parent.to_string_lossy())).find(|d| crate::shellops::needs_admin_write(d)) {
                return Err(format!("elevate:You'll need administrator permission to restore into “{}”", file_name(&dir)));
            }
            // The shell can't restore to a path this long; those go back directly.
            let (long, pick): (Vec<_>, Vec<_>) = pick.into_iter().partition(|i| crate::shellops::too_long_for_shell(&original(i)));
            for i in &long {
                let to = crate::shellops::restore_direct(&paths::normalize(&i.id.to_string_lossy()), &original(i))?;
                if let Some(r) = restored.iter_mut().find(|r| paths::key(r) == paths::key(&original(i))) {
                    *r = to;
                }
            }
            if !pick.is_empty() {
                trash::os_limited::restore_all(pick).map_err(err)?;
            }
            log::info!("restored from Recycle Bin: {}", crate::logging::paths(&restored));
            Ok(restored)
        }
        #[cfg(not(windows))]
        {
            let _ = items;
            Err("Not supported".into())
        }
    })
    .await
    .map_err(err)?
}

/// Recycle Bin item paths (`…\$Recycle.Bin\…\$R…`) for the latest deletion of each original path.
#[tauri::command]
pub async fn find_in_trash(items: Vec<String>) -> Res<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(windows)]
        {
            Ok(latest_in_trash(&items)?.iter().map(|i| i.id.to_string_lossy().into_owned()).collect())
        }
        #[cfg(not(windows))]
        {
            let _ = items;
            Err("Not supported".into())
        }
    })
    .await
    .map_err(err)?
}

// ---------- properties ----------

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ItemInfo {
    path: String,
    name: String,
    is_dir: bool,
    size: u64,
    created: Option<i64>,
    modified: Option<i64>,
    accessed: Option<i64>,
    readonly: bool,
    hidden: bool,
    system: bool,
    is_symlink: bool,
    link_target: Option<String>,
}

/// A server item's details (SFTP knows no creation time or Windows attributes).
async fn remote_item_info(p: String) -> Res<ItemInfo> {
    let (s, linux) = crate::remote::fs::session_of(&p).await?;
    let lm = s.sftp.symlink_metadata(linux.clone()).await.map_err(|e| crate::remote::fs::sftp_err(&p, e))?;
    let m = if lm.is_symlink() { s.sftp.metadata(linux.clone()).await.unwrap_or(lm.clone()) } else { lm.clone() };
    let t = |s: Option<u32>| s.map(|v| v as i64 * 1000);
    Ok(ItemInfo {
        name: file_name(&p),
        is_dir: m.is_dir(),
        size: if m.is_dir() { 0 } else { m.size.unwrap_or(0) },
        created: None,
        modified: t(m.mtime),
        accessed: t(m.atime),
        readonly: m.permissions.is_some_and(|p| p & 0o200 == 0),
        hidden: file_name(&p).starts_with('.'),
        system: false,
        is_symlink: lm.is_symlink(),
        link_target: if lm.is_symlink() { s.sftp.read_link(linux).await.ok() } else { None },
        path: p,
    })
}

#[tauri::command]
pub async fn item_info(path: String) -> Res<ItemInfo> {
    let p = paths::normalize(&path);
    if paths::is_remote(&p) {
        return remote_item_info(p).await;
    }
    let lm = fs::symlink_metadata(io(&p)).map_err(err)?;
    let m = fs::metadata(io(&p)).unwrap_or(lm.clone());
    let t = |r: std::io::Result<std::time::SystemTime>| {
        r.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64)
    };
    #[cfg(windows)]
    let attrs = {
        use std::os::windows::fs::MetadataExt;
        lm.file_attributes()
    };
    #[cfg(not(windows))]
    let attrs = 0u32;
    Ok(ItemInfo {
        name: file_name(&p),
        is_dir: m.is_dir(),
        size: if m.is_dir() { 0 } else { m.len() },
        created: t(m.created()),
        modified: t(m.modified()),
        accessed: t(m.accessed()),
        readonly: m.permissions().readonly(),
        hidden: attrs & 0x2 != 0,
        system: attrs & 0x4 != 0,
        is_symlink: lm.file_type().is_symlink(),
        link_target: fs::read_link(io(&p)).ok().map(|t| t.to_string_lossy().into_owned()),
        path: p,
    })
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    files: u64,
    dirs: u64,
    bytes: u64,
    /// Stopped early (too many items or unreadable folders).
    partial: bool,
}

/// Recursive size/count for the properties dialog and status bar.
#[tauri::command]
pub async fn folder_stats(items: Vec<String>) -> Res<Stats> {
    if crate::remote::ops::involves(&items, None) {
        let (files, dirs, bytes, partial) = crate::remote::ops::stats(&items).await;
        return Ok(Stats { files, dirs, bytes, partial });
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut st = Stats::default();
        let mut stack: Vec<PathBuf> = Vec::new();
        for p in &items {
            let path = io(p);
            match fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() => {
                    st.dirs += 1;
                    stack.push(path);
                }
                Ok(m) => {
                    st.files += 1;
                    st.bytes += m.len();
                }
                Err(_) => st.partial = true,
            }
        }
        let start = std::time::Instant::now();
        while let Some(dir) = stack.pop() {
            if start.elapsed().as_secs() > 60 {
                st.partial = true;
                break;
            }
            let Ok(rd) = fs::read_dir(&dir) else {
                st.partial = true;
                continue;
            };
            for e in rd.flatten() {
                let Ok(m) = e.metadata() else { continue };
                if m.is_dir() {
                    st.dirs += 1;
                    stack.push(e.path());
                } else {
                    st.files += 1;
                    st.bytes += m.len();
                }
            }
        }
        // The roots themselves aren't "contained" folders.
        st.dirs = st.dirs.saturating_sub(items.iter().filter(|p| io(p).is_dir()).count() as u64);
        Ok(st)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub fn set_attributes(path: String, readonly: bool, hidden: bool) -> Res<()> {
    if paths::is_remote(&path) {
        return Err("Files on a server don't have these Windows attributes".into());
    }
    log::info!("set attributes on \"{path}\": readonly={readonly}, hidden={hidden}");
    let p = io(&path);
    let mut perms = fs::metadata(&p).map_err(err)?.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(readonly);
    fs::set_permissions(&p, perms).map_err(err)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let flag = if hidden { "+h" } else { "-h" };
        std::process::Command::new("attrib")
            .arg(flag)
            .arg(&p)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status()
            .map_err(err)?;
    }
    #[cfg(not(windows))]
    let _ = hidden;
    Ok(())
}

// ---------- previews ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextPreview {
    text: String,
    truncated: bool,
    binary: bool,
    encoding: String,
    size: u64,
    /// Lines in the whole file when only its start was read (counted up to TEXT_COUNT_MAX).
    lines: Option<u64>,
}

/// Files up to this size get their lines counted when the preview shows only their start.
const TEXT_COUNT_MAX: u64 = 512 * 1024 * 1024;

fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    if sample.is_empty() {
        return false;
    }
    let nul = sample.iter().filter(|&&b| b == 0).count();
    let ctrl = sample.iter().filter(|&&b| b < 9 || (b > 13 && b < 32 && b != 27)).count();
    nul > 0 || ctrl * 20 > sample.len()
}

/// Reads up to `max_bytes` of a file as text (UTF-8/UTF-16 with BOM, Windows-1252 fallback).
#[tauri::command]
pub async fn read_text(path: String, max_bytes: Option<u64>) -> Res<TextPreview> {
    let max = max_bytes.unwrap_or(2 * 1024 * 1024);
    // From a server: only the start comes over (its lines aren't counted).
    if paths::is_remote(&path) {
        let (buf, size) = crate::remote::fs::read_head(&path, max).await?;
        return Ok(decode_text(buf, size, size > max, None));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut f = fs::File::open(io(&path)).map_err(err)?;
        let size = f.metadata().map_err(err)?.len();
        let mut buf = Vec::with_capacity(max.min(size) as usize);
        (&mut f).take(max).read_to_end(&mut buf).map_err(err)?;
        let truncated = size > max;
        Ok(decode_text(buf, size, truncated, Some(&mut f)))
    })
    .await
    .map_err(err)?
}

/// A file's start as text (`rest`: the remainder, to count its lines).
fn decode_text(buf: Vec<u8>, size: u64, truncated: bool, rest: Option<&mut dyn Read>) -> TextPreview {
    {
        let (enc, bom) = encoding_rs::Encoding::for_bom(&buf).unwrap_or((encoding_rs::UTF_8, 0));
        if bom == 0 && looks_binary(&buf) {
            return TextPreview { text: String::new(), truncated, binary: true, encoding: "binary".into(), size, lines: None };
        }
        let (text, encoding) = if bom > 0 {
            let (t, _, _) = enc.decode(&buf);
            (t.into_owned(), enc.name().to_string())
        } else {
            // A cut-off multi-byte char at the end is expected when truncated.
            match std::str::from_utf8(&buf) {
                Ok(s) => (s.to_string(), "UTF-8".into()),
                Err(e) if truncated && e.valid_up_to() + 4 >= buf.len() => {
                    (String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned(), "UTF-8".into())
                }
                Err(_) => {
                    let (t, _, _) = encoding_rs::WINDOWS_1252.decode(&buf);
                    (t.into_owned(), "Windows-1252".into())
                }
            }
        };
        // Cut short: the rest is only counted, so the preview can say how long the file is.
        // (Byte counting fits UTF-8 and the one-byte encodings, not UTF-16.)
        let lines = rest.filter(|_| truncated && bom == 0 && size <= TEXT_COUNT_MAX).map(|f| {
            let mut n = memchr::memchr_iter(b'\n', &buf).count() as u64;
            let mut last = buf.last().copied();
            let mut chunk = vec![0u8; 1 << 20];
            while let Ok(got) = f.read(&mut chunk) {
                if got == 0 {
                    break;
                }
                n += memchr::memchr_iter(b'\n', &chunk[..got]).count() as u64;
                last = Some(chunk[got - 1]);
            }
            n + u64::from(last.is_some_and(|b| b != b'\n'))
        });
        TextPreview { text, truncated, binary: false, encoding, size, lines }
    }
}

/// First `count` bytes as a hex dump source for the fallback preview.
#[tauri::command]
pub async fn read_bytes(path: String, count: Option<u64>) -> Res<Vec<u8>> {
    if paths::is_remote(&path) {
        return crate::remote::fs::read_head(&path, count.unwrap_or(512)).await.map(|(b, _)| b);
    }
    let f = fs::File::open(io(&path)).map_err(err)?;
    let mut buf = Vec::new();
    f.take(count.unwrap_or(512)).read_to_end(&mut buf).map_err(err)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(validate_name("ok.txt").is_ok());
        assert!(validate_name("a:b").is_err());
        assert!(validate_name("CON.txt").is_err());
        assert!(validate_name("trail.").is_err());
        assert!(validate_name("  ").is_err());
    }

    #[test]
    fn unique_names() {
        let dir = std::env::temp_dir().join(format!("ferret-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().to_string();
        fs::write(dir.join("a.txt"), "x").unwrap();
        assert_eq!(unique_name(&d, "b.txt", false), "b.txt");
        assert_eq!(unique_name(&d, "a.txt", false), "a (2).txt");
        assert_eq!(unique_name(&d, "a.txt", true), "a - Copy.txt");
        fs::write(dir.join("a - Copy.txt"), "x").unwrap();
        assert_eq!(unique_name(&d, "a.txt", true), "a - Copy (2).txt");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn volumes() {
        assert!(same_volume(r"C:\a", r"c:\b\c"));
        assert!(!same_volume(r"C:\a", r"D:\a"));
        assert!(same_volume(r"\\wsl.localhost\Ubuntu\a", r"\\wsl.localhost\Ubuntu\b"));
        assert!(!same_volume(r"\\wsl.localhost\Ubuntu\a", r"\\wsl.localhost\Arch\b"));
    }
}
