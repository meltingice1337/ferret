//! File jobs that involve a server: copy and move between this PC and servers (and between
//! servers), delete to the server's trash or for good, and their undo. They run like the local
//! ones (fileops.rs): in the jobs panel, with progress, Try again / Skip / Skip all for an item
//! that fails, and exactly what they did reported back for undo.

use tauri::AppHandle;

use super::block;
use super::fs::{self as rfs, NOT_TRASHABLE};
use super::ssh::{self, Hold};
use crate::fileops::{per_item, Conflict, Policy};
use crate::jobs::{self, JobCtx};
use crate::listing::join;
use crate::paths;
use crate::vfs;

type Res<T> = Result<T, String>;

pub fn involves(paths: &[String], dest: Option<&str>) -> bool {
    paths.iter().any(|p| paths::is_remote(p)) || dest.is_some_and(paths::is_remote)
}

fn name(p: &str) -> String {
    let n = paths::normalize(p);
    n.rsplit('\\').next().unwrap_or(&n).to_string()
}

/// Keeps every server session a job uses open until it ends.
fn hold_all(paths: &[&str]) -> Res<Vec<Hold>> {
    let mut ids: Vec<String> = paths.iter().filter_map(|p| paths::remote_parts(&paths::normalize(p)).map(|(id, _)| id.to_string())).collect();
    ids.sort();
    ids.dedup();
    ids.into_iter().map(|id| block(ssh::session(&id)).map(|s| s.hold())).collect()
}

fn same_parent(p: &str, dir: &str) -> bool {
    paths::parent(p).map(|x| paths::key(&x)) == Some(paths::key(dir))
}

/// Items whose name is already in `dest` (as fileops::check_conflicts, on any file system).
pub fn conflicts(sources: &[String], dest: &str) -> Vec<Conflict> {
    let dest = paths::normalize(dest);
    let to = vfs::of(&dest);
    sources
        .iter()
        .filter_map(|s| {
            let s = paths::normalize(s);
            if same_parent(&s, &dest) {
                return None;
            }
            let target = join(&dest, &name(&s));
            let tm = to.stat(&target).ok()?;
            let sm = vfs::of(&s).stat(&s).ok()?;
            Some(Conflict::new(name(&s), s, target, &sm, &tm))
        })
        .collect()
}

/// Copy or move `sources` into `dest` when a server is on either side.
pub fn transfer(app: &AppHandle, moving: bool, sources: Vec<String>, dest: String, policy: Policy) -> Res<u64> {
    let dest = paths::normalize(&dest);
    let all: Vec<&str> = sources.iter().map(String::as_str).chain([dest.as_str()]).collect();
    let holds = hold_all(&all)?;
    let to = vfs::of(&dest);
    if !to.stat(&dest).is_ok_and(|m| m.is_dir) {
        return Err(format!("{} is not a folder", name(&dest)));
    }
    for s in &sources {
        let (k, dk) = (paths::key(s), paths::key(&dest));
        if dk == k || dk.starts_with(&format!("{k}\\")) {
            return Err(format!("Can't {} “{}” into itself", if moving { "move" } else { "copy" }, name(s)));
        }
    }
    let label = format!(
        "{} {} to {}",
        if moving { "Moving" } else { "Copying" },
        if sources.len() == 1 { format!("“{}”", name(&sources[0])) } else { format!("{} items", sources.len()) },
        name(&dest)
    );
    log::info!("{} to \"{dest}\" ({policy:?}): {}", if moving { "move" } else { "copy" }, crate::logging::paths(&sources));
    Ok(jobs::spawn(app, if moving { "move" } else { "copy" }, &label, move |ctx| {
        let _holds = holds;
        let mut skip_all = false;
        let mut pending: Vec<(String, String)> = Vec::new();
        for s in sources.iter().map(|s| paths::normalize(s)) {
            let from = vfs::of(&s);
            let same_dir = same_parent(&s, &dest);
            if moving && same_dir {
                continue;
            }
            let n = name(&s);
            let mut target = join(&dest, &n);
            let exists = vfs::exists(&*to, &target);
            if (exists && policy == Policy::KeepBoth) || (!moving && same_dir) {
                target = join(&dest, &vfs::unique_name(&*to, &dest, &n, !moving && same_dir));
            } else if exists && policy == Policy::Skip {
                ctx.skipped();
                continue;
            }
            // On the same server (or disk), a move is a rename.
            if moving && from.same(&s, &dest) {
                let src_is_dir = from.lstat(&s).is_ok_and(|m| m.is_dir);
                if !vfs::exists(&*to, &target) || (policy == Policy::Replace && !src_is_dir) {
                    ctx.set_current(&n);
                    let moved = per_item(ctx, &mut skip_all, &s, || {
                        if vfs::exists(&*to, &target) {
                            vfs::displace(&*to, &target, ctx)?;
                        }
                        to.rename(&s, &target).map_err(|e| format!("Couldn't move “{n}”: {e}"))
                    })?;
                    if moved {
                        ctx.pair(s.clone(), target.clone());
                        ctx.created(target);
                    }
                    continue;
                }
            }
            pending.push((s, target));
        }
        if pending.is_empty() {
            ctx.set_totals(0, 0);
            return Ok(());
        }
        let (mut bytes, mut files) = (0u64, 0u64);
        for (s, _) in &pending {
            vfs::totals(&*vfs::of(s), s, &mut bytes, &mut files, ctx)?;
        }
        ctx.set_totals(bytes, files);
        for (s, target) in pending {
            let from = vfs::of(&s);
            let mut copied = false;
            per_item(ctx, &mut skip_all, &s, || {
                if !copied {
                    let src_is_dir = from.lstat(&s).map_err(|e| e.to_string())?.is_dir;
                    if policy == Policy::Replace && vfs::exists(&*to, &target) && (!src_is_dir || !to.stat(&target).is_ok_and(|m| m.is_dir)) {
                        vfs::displace(&*to, &target, ctx)?;
                    }
                    if !server_copy(&s, &target, ctx)? {
                        vfs::copy_tree(&*from, &s, &*to, &target, policy, ctx, true)?;
                    }
                    copied = true;
                }
                if moving {
                    from.remove(&s).map_err(|e| format!("Copied, but couldn't remove the original “{}”: {e}", name(&s)))?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }))
}

/// A copy inside one server that runs commands: `cp -a` there, instead of every byte coming
/// here and going back. Only for a new item (merging into a folder goes item by item).
fn server_copy(src: &str, target: &str, ctx: &JobCtx) -> Res<bool> {
    let (Some((a, from)), Some((b, to))) = (paths::remote_parts(src), paths::remote_parts(target)) else { return Ok(false) };
    if a != b {
        return Ok(false);
    }
    let Some(s) = ssh::connected(a).filter(|s| s.arch.is_some()) else { return Ok(false) };
    if block(rfs::exists(&s, &to)) {
        return Ok(false);
    }
    ctx.set_current(&name(src));
    let mut bytes = 0;
    let mut files = 0;
    vfs::totals(&*vfs::of(src), src, &mut bytes, &mut files, ctx)?;
    let cmd = format!("cp -a -- {} {}", ssh::quote(&from), ssh::quote(&to));
    block(s.run(&cmd, std::time::Duration::from_secs(24 * 3600))).map_err(|e| format!("Couldn't copy “{}” on the server ({e})", name(src)))?;
    ctx.add_bytes(bytes);
    (0..files).for_each(|_| ctx.file_done());
    ctx.created(target.to_string());
    ctx.pair(src.to_string(), target.to_string());
    Ok(true)
}

/// Delete on a server: to its trash (each item reported as a pair, original → in the trash, for
/// undo), or for good. An item that can't go to the trash fails with NOT_TRASHABLE, without
/// asking: the window then offers to delete those for good.
pub fn delete(app: &AppHandle, items: Vec<String>, permanent: bool) -> Res<u64> {
    let all: Vec<&str> = items.iter().map(String::as_str).collect();
    let holds = hold_all(&all)?;
    let label = if items.len() == 1 {
        format!("{} “{}”", if permanent { "Deleting" } else { "Moving to the trash" }, name(&items[0]))
    } else {
        format!("{} {} items", if permanent { "Deleting" } else { "Moving to the trash" }, items.len())
    };
    log::info!("{} on a server: {}", if permanent { "delete permanently" } else { "trash" }, crate::logging::paths(&items));
    Ok(jobs::spawn(app, if permanent { "delete" } else { "recycle" }, &label, move |ctx| {
        let _holds = holds;
        ctx.set_totals(0, items.len() as u64);
        let mut skip_all = false;
        for p in items.iter().map(|p| paths::normalize(p)) {
            ctx.check()?;
            ctx.set_current(&name(&p));
            let (id, linux) = rfs::split(&p)?;
            let s = block(ssh::session(&id))?;
            if permanent {
                per_item(ctx, &mut skip_all, &p, || block(rfs::remove_all(&s, &linux)).map_err(|e| format!("Couldn't delete “{}”: {e}", name(&p))))?;
            } else {
                match block(rfs::trash_one(&s, &id, &linux)) {
                    Ok(at) => ctx.pair(p.clone(), at),
                    Err(e) if e.starts_with(NOT_TRASHABLE) => ctx.failed(p.clone(), e),
                    Err(e) => {
                        per_item(ctx, &mut skip_all, &p, || Err(e.clone()))?;
                    }
                }
            }
            ctx.file_done();
        }
        Ok(())
    }))
}

/// Undo of a move or a delete to the trash on a server: each (original, now) goes back.
pub fn undo_moves(app: &AppHandle, pairs: Vec<(String, String)>) -> Res<u64> {
    let all: Vec<&str> = pairs.iter().flat_map(|(a, b)| [a.as_str(), b.as_str()]).collect();
    let holds = hold_all(&all)?;
    let label = if pairs.len() == 1 { format!("Moving “{}” back", name(&pairs[0].0)) } else { format!("Moving {} items back", pairs.len()) };
    Ok(jobs::spawn(app, "move", &label, move |ctx| {
        let _holds = holds;
        ctx.set_totals(0, pairs.len() as u64);
        let mut blocked = Vec::new();
        for (from, to) in pairs.iter().rev() {
            ctx.check()?;
            ctx.set_current(&name(from));
            let (src, dst) = (vfs::of(to), vfs::of(from));
            if !vfs::exists(&*src, to) {
                continue; // gone since
            }
            if vfs::exists(&*dst, from) {
                blocked.push(name(from));
                continue;
            }
            // Out of the server's trash: its .trashinfo goes too.
            if let (Some((id, t)), Some((id2, f))) = (paths::remote_parts(to), paths::remote_parts(from))
                && id == id2
                && t.contains("/.local/share/Trash/files/")
            {
                let s = block(ssh::session(id))?;
                block(rfs::untrash(&s, &t, &f))?;
            } else if src.same(to, from) {
                dst.rename(to, from).map_err(|e| e.to_string())?;
            } else {
                vfs::copy_tree(&*src, to, &*dst, from, Policy::KeepBoth, ctx, false)?;
                src.remove(to).map_err(|e| e.to_string())?;
            }
            ctx.created(from.clone());
            ctx.file_done();
        }
        if blocked.is_empty() { Ok(()) } else { Err(format!("Something with the same name is back at the original location: {}", blocked.join(", "))) }
    }))
}

/// Files, folders and bytes inside server items (the status bar, properties), walked over SFTP
/// for at most 20 seconds.
pub async fn stats(items: &[String]) -> (u64, u64, u64, bool) {
    let (mut files, mut dirs, mut bytes, mut partial) = (0, 0, 0, false);
    let start = std::time::Instant::now();
    for p in items {
        let Ok((s, linux)) = rfs::session_of(p).await else {
            partial = true;
            continue;
        };
        let mut stack = vec![linux];
        let mut root = true;
        while let Some(dir) = stack.pop() {
            if start.elapsed().as_secs() > 20 {
                return (files, dirs, bytes, true);
            }
            let m = match s.sftp.symlink_metadata(dir.clone()).await {
                Ok(m) => m,
                Err(_) => {
                    partial = true;
                    continue;
                }
            };
            if !m.is_dir() {
                files += 1;
                bytes += m.size.unwrap_or(0);
                continue;
            }
            if !root {
                dirs += 1;
            }
            root = false;
            match s.sftp.read_dir(dir.clone()).await {
                Ok(rd) => {
                    for e in rd {
                        let em = e.metadata();
                        let full = if dir.ends_with('/') { format!("{dir}{}", e.file_name()) } else { format!("{dir}/{}", e.file_name()) };
                        if em.is_dir() {
                            stack.push(full);
                        } else {
                            files += 1;
                            bytes += em.size.unwrap_or(0);
                        }
                    }
                }
                Err(_) => partial = true,
            }
        }
    }
    (files, dirs, bytes, partial)
}
