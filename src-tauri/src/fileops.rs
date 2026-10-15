//! File operations: create, rename, copy/move with progress and conflict
//! policies, delete (Recycle Bin or permanent), restore from Recycle Bin (undo),
//! properties and small reads for previews.

use serde::{Deserialize, Serialize};
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
pub fn create_folder(dir: String, name: Option<String>) -> Res<String> {
    let name = name.unwrap_or_else(|| "New folder".into());
    validate_name(&name)?;
    let name = unique_name(&dir, &name, false);
    let p = join(&paths::normalize(&dir), &name);
    fs::create_dir(io(&p)).map_err(|e| {
        if denied(&e) && crate::shellops::needs_admin_write(&dir) { admin_err(format!("create a folder in “{}”", file_name(&dir))) } else { err(e) }
    })?;
    Ok(p)
}

#[tauri::command]
pub fn create_file(dir: String, name: String, content: Option<String>) -> Res<String> {
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
pub fn rename_item(path: String, new_name: String) -> Res<String> {
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

/// Items in `sources` whose name already exists in `dest` (ignores moving onto itself).
// Off the main thread: with thousands of items this can take a moment, and the window
// must keep answering meanwhile.
#[tauri::command(async)]
pub fn check_conflicts(sources: Vec<String>, dest: String) -> Vec<Conflict> {
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

#[derive(Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Policy {
    Replace,
    Skip,
    KeepBoth,
}

fn walk_totals(p: &Path, bytes: &mut u64, files: &mut u64, ctx: &JobCtx) -> Res<()> {
    ctx.check()?;
    let m = fs::symlink_metadata(p).map_err(err)?;
    if m.is_dir() {
        for e in fs::read_dir(p).map_err(err)?.flatten() {
            walk_totals(&e.path(), bytes, files, ctx)?;
        }
    } else {
        *bytes += m.len();
        *files += 1;
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path, ctx: &JobCtx) -> Res<()> {
    let meta = fs::metadata(src).map_err(err)?;
    let mut input = fs::File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let mut output = fs::File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let mut buf = vec![0u8; 1 << 20];
    let result = (|| -> Res<()> {
        loop {
            ctx.check()?;
            let n = input.read(&mut buf).map_err(err)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n]).map_err(err)?;
            ctx.add_bytes(n as u64);
        }
        if let Ok(t) = meta.modified() {
            let _ = output.set_modified(t);
        }
        Ok(())
    })();
    if result.is_err() {
        drop(output);
        let _ = fs::remove_file(dst);
    } else {
        let _ = fs::set_permissions(dst, meta.permissions());
    }
    result
}

/// Moves an item that is about to be overwritten to the Recycle Bin, so undo can bring it back.
/// Falls back to deleting it where there is no Recycle Bin (e.g. some network/WSL shares).
fn displace(p: &Path, ctx: &JobCtx) -> Res<()> {
    match trash::delete(p) {
        Ok(()) => {
            ctx.replaced(p.to_string_lossy().into_owned());
            Ok(())
        }
        Err(_) => remove_any(p).map_err(|e| format!("Couldn't replace {}: {e}", p.display())),
    }
}

/// Copies `src` to `dst`, merging into existing folders. Reports exactly what it
/// created (`report`), so undo never touches content that was already there, and
/// recycles anything it overwrites.
fn copy_tree(src: &Path, dst: &Path, policy: Policy, ctx: &JobCtx, report: bool) -> Res<()> {
    ctx.check()?;
    let m = fs::symlink_metadata(src).map_err(err)?;
    let s_str = src.to_string_lossy().into_owned();
    if m.is_dir() {
        let merging = dst.exists();
        if !merging {
            fs::create_dir(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
            if report {
                ctx.created(dst.to_string_lossy().into_owned());
                ctx.pair(s_str.clone(), dst.to_string_lossy().into_owned());
            }
        }
        for e in fs::read_dir(src).map_err(err)?.flatten() {
            // Inside a brand-new folder only the folder itself is reported.
            copy_tree(&e.path(), &dst.join(e.file_name()), policy, ctx, report && merging)?;
        }
        if let Ok(t) = m.modified() {
            if let Ok(f) = fs::File::open(dst) {
                let _ = f.set_modified(t);
            }
        }
        Ok(())
    } else {
        ctx.set_current(&src.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
        let mut target = dst.to_path_buf();
        if fs::symlink_metadata(dst).is_ok() {
            match policy {
                Policy::Skip => {
                    ctx.add_bytes(m.len());
                    ctx.file_done();
                    ctx.skipped();
                    return Ok(());
                }
                Policy::Replace => displace(dst, ctx)?,
                Policy::KeepBoth => {
                    let dir = dst.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                    target = Path::new(&dir).join(unique_name(&dir, &dst.file_name().unwrap().to_string_lossy(), false));
                }
            }
        }
        copy_file(src, &target, ctx)?;
        if report {
            ctx.created(target.to_string_lossy().into_owned());
            ctx.pair(s_str, target.to_string_lossy().into_owned());
        }
        ctx.file_done();
        Ok(())
    }
}

/// Whether a new folder can be created in `dir` (creates and removes a probe folder).
fn can_create_folder(dir: &str) -> bool {
    let probe = io(&join(dir, &format!(".ferret-probe-{}", std::process::id())));
    match fs::create_dir(&probe) {
        Ok(()) => fs::remove_dir(&probe).is_ok() || true,
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
                    if target_exists {
                        displace(&io(&target), ctx)?;
                    }
                    ctx.set_current(&name);
                    if fs::rename(io(s), io(&target)).is_ok() {
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
            walk_totals(&io(s), &mut bytes, &mut files, ctx)?;
        }
        ctx.set_totals(bytes, files);

        for (s, target) in pending {
            ctx.check()?;
            let tp = io(&target);
            // A file replacing a folder (or vice versa) can't merge: displace the whole item.
            if policy == Policy::Replace && tp.exists() && (io(&s).is_dir() != tp.is_dir() || !io(&s).is_dir()) {
                displace(&tp, ctx)?;
            }
            copy_tree(&io(&s), &tp, policy, ctx, true)?;
            if moving {
                remove_any(&io(&s)).map_err(|e| format!("Copied, but couldn't remove {s}: {e}"))?;
            }
        }
        Ok(())
    }))
}

/// Undo for a move: puts each moved item back where it came from.
// Off the main thread: with thousands of items this can take a moment, and the window
// must keep answering meanwhile.
#[tauri::command(async)]
pub fn undo_moves(app: AppHandle, pairs: Vec<(String, String)>) -> Res<u64> {
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
                copy_tree(&t, &f, Policy::KeepBoth, ctx, false)?;
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
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(windows)]
        {
            let pick = latest_in_trash(&items)?;
            let restored: Vec<String> = pick.iter().map(|i| paths::normalize(&i.original_path().to_string_lossy())).collect();
            // Restoring into a protected folder needs the elevated path.
            if let Some(dir) = pick.iter().map(|i| i.original_parent.to_string_lossy().into_owned()).find(|d| crate::shellops::needs_admin_write(d)) {
                return Err(format!("elevate:You'll need administrator permission to restore into “{}”", file_name(&dir)));
            }
            trash::os_limited::restore_all(pick).map_err(err)?;
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

#[tauri::command]
pub fn item_info(path: String) -> Res<ItemInfo> {
    let p = paths::normalize(&path);
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
}

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
    tauri::async_runtime::spawn_blocking(move || {
        let max = max_bytes.unwrap_or(2 * 1024 * 1024);
        let f = fs::File::open(io(&path)).map_err(err)?;
        let size = f.metadata().map_err(err)?.len();
        let mut buf = Vec::with_capacity(max.min(size) as usize);
        f.take(max).read_to_end(&mut buf).map_err(err)?;
        let truncated = size > max;

        let (enc, bom) = encoding_rs::Encoding::for_bom(&buf).unwrap_or((encoding_rs::UTF_8, 0));
        if bom == 0 && looks_binary(&buf) {
            return Ok(TextPreview { text: String::new(), truncated, binary: true, encoding: "binary".into(), size });
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
        Ok(TextPreview { text, truncated, binary: false, encoding, size })
    })
    .await
    .map_err(err)?
}

/// First `count` bytes as a hex dump source for the fallback preview.
#[tauri::command]
pub fn read_bytes(path: String, count: Option<u64>) -> Res<Vec<u8>> {
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
