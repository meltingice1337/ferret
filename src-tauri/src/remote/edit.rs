//! Files opened from a server: downloaded to a local copy (each in its own folder under
//! %LOCALAPPDATA%\Ferret\remote-edit), opened in their app, and uploaded back each time the app
//! saves. The upload asks first if the server's copy changed since it was opened (or last
//! uploaded), and goes through a temporary file renamed over the original, so a failed upload
//! never leaves half a file on the server. `remote-synced` tells the window how it went.

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use russh_sftp::client::fs::Metadata;
use russh_sftp::protocol::OpenFlags;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::AsyncWriteExt;

use super::fs::{cache_dir, session_of, sftp_err, split};
use super::prompt::{self, Prompt};

type Res<T> = Result<T, String>;

/// A file being edited.
struct Edit {
    /// The Ferret path on the server.
    remote: String,
    /// The server's copy when it was downloaded or last uploaded: (size, mtime).
    version: (u64, u32),
    /// The local copy as it was then.
    local: (u64, SystemTime),
    /// Its folder's watcher (dropped with the edit).
    _watcher: Option<RecommendedWatcher>,
}

fn edits() -> &'static Mutex<HashMap<PathBuf, Edit>> {
    static E: OnceLock<Mutex<HashMap<PathBuf, Edit>>> = OnceLock::new();
    E.get_or_init(Default::default)
}

/// Saves noticed, waiting for the app to finish writing (several events come per save).
fn pending() -> &'static Mutex<HashMap<PathBuf, Instant>> {
    static P: OnceLock<Mutex<HashMap<PathBuf, Instant>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn version(m: &Metadata) -> (u64, u32) {
    (m.size.unwrap_or(0), m.mtime.unwrap_or(0))
}

fn local_version(p: &Path) -> Option<(u64, SystemTime)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?))
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Synced {
    path: String,
    name: String,
    connection: String,
    error: Option<String>,
}

/// Opens a server file in its app (the double-click on it).
#[tauri::command]
pub async fn remote_open(path: String) -> Res<()> {
    let local = download(&path).await?;
    tauri_plugin_opener::open_path(local.to_string_lossy().into_owned(), None::<&str>).map_err(|e| e.to_string())
}

/// The file's local copy, downloaded (unless it's being edited already: then that copy, with
/// any changes not uploaded yet), and watched for saves.
async fn download(path: &str) -> Res<PathBuf> {
    let (s, linux) = session_of(path).await?;
    let (id, _) = split(path)?;
    let dir = cache_dir("remote-edit", &id)?.join(format!("{:016x}", crate::archive::fnv1a(linux.as_bytes())));
    let name = linux.rsplit('/').next().unwrap_or("file").to_string();
    let local = dir.join(&name);
    if edits().lock().unwrap().contains_key(&local) && local.is_file() {
        return Ok(local);
    }
    let m = s.sftp.metadata(linux.clone()).await.map_err(|e| sftp_err(path, e))?;
    if m.is_dir() {
        return Err(format!("“{name}” is a folder"));
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let _hold = s.hold();
    let mut src = s.sftp.open(linux).await.map_err(|e| sftp_err(path, e))?;
    let part = dir.join(".part");
    let mut out = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
    tokio::io::copy(&mut src, &mut out).await.map_err(|e| e.to_string())?;
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);
    std::fs::rename(&part, &local).map_err(|e| e.to_string())?;
    let watcher = watch(&dir);
    let lv = local_version(&local).ok_or("the copy vanished")?;
    edits().lock().unwrap().insert(local.clone(), Edit { remote: path.to_string(), version: version(&m), local: lv, _watcher: watcher });
    log::info!("remote: opened {} from {} for editing ({})", name, s.name, local.display());
    Ok(local)
}

/// Watches a copy's folder (apps often save through a temporary file renamed over it).
fn watch(dir: &Path) -> Option<RecommendedWatcher> {
    start_uploader();
    let mut w = notify::recommended_watcher(|res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        for p in ev.paths {
            if edits().lock().unwrap().contains_key(&p) {
                pending().lock().unwrap().insert(p, Instant::now());
            }
        }
    })
    .ok()?;
    w.watch(dir, RecursiveMode::NonRecursive).ok()?;
    Some(w)
}

/// Uploads saves once the app has been quiet for a moment.
fn start_uploader() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_millis(250));
        let due: Vec<PathBuf> = {
            let mut p = pending().lock().unwrap();
            let due: Vec<PathBuf> = p.iter().filter(|(_, t)| t.elapsed() >= Duration::from_millis(800)).map(|(k, _)| k.clone()).collect();
            due.iter().for_each(|k| {
                p.remove(k);
            });
            due
        };
        for local in due {
            tauri::async_runtime::spawn(async move { upload(local).await });
        }
    });
}

async fn upload(local: PathBuf) {
    let Some(now) = local_version(&local) else { return };
    let (remote, known) = {
        let e = edits().lock().unwrap();
        let Some(edit) = e.get(&local) else { return };
        // Not changed since the last upload (an app that only read it, or touched its folder).
        if edit.local == now {
            return;
        }
        (edit.remote.clone(), edit.version)
    };
    let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let connection = split(&remote).map(|(id, _)| super::name_of(&id)).unwrap_or_default();
    let res = send(&local, &remote, known, &name, &connection).await;
    let report = |error: Option<String>| {
        super::emit("remote-synced", Synced { path: remote.clone(), name: name.clone(), connection: connection.clone(), error });
    };
    match res {
        Ok(Some(v)) => {
            if let Some(e) = edits().lock().unwrap().get_mut(&local) {
                e.version = v;
                e.local = now;
            }
            log::info!("remote: uploaded {name} to {connection}");
            report(None);
        }
        // Kept here (the user chose not to replace the server's copy).
        Ok(None) => {
            if let Some(e) = edits().lock().unwrap().get_mut(&local) {
                e.local = now;
            }
        }
        Err(e) => {
            log::warn!("remote: couldn't upload {name} to {connection}: {e}");
            report(Some(e));
        }
    }
}

/// Uploads a saved copy; the server's new version, or None when the user kept the server's.
async fn send(local: &Path, remote: &str, known: (u64, u32), name: &str, connection: &str) -> Res<Option<(u64, u32)>> {
    let (s, linux) = session_of(remote).await?;
    let _hold = s.hold();
    let current = s.sftp.metadata(linux.clone()).await.ok();
    if let Some(m) = &current
        && version(m) != known
    {
        let q = Prompt::confirm(
            connection,
            format!("Replace “{name}” on {connection}?"),
            format!("The server's copy of “{name}” changed since you opened it. Uploading your saved changes replaces it."),
            "Upload mine",
        );
        if prompt::ask(Prompt { danger: true, ..q }).await.is_none() {
            return Ok(None);
        }
    }
    let bytes = tokio::fs::read(local).await.map_err(|e| e.to_string())?;
    let (dir, file) = linux.rsplit_once('/').ok_or("bad path")?;
    let dir = if dir.is_empty() { "/" } else { dir };
    let temp = format!("{}/.{file}.ferret-upload", dir.trim_end_matches('/'));
    let write = async {
        let mut f = s.sftp.open_with_flags(temp.clone(), OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE).await?;
        f.write_all(&bytes).await.map_err(|e| russh_sftp::client::error::Error::IO(e.to_string()))?;
        f.shutdown().await.map_err(|e| russh_sftp::client::error::Error::IO(e.to_string()))?;
        // The original's permissions (a script stays executable).
        if let Some(mode) = current.as_ref().and_then(|m| m.permissions) {
            let _ = s.sftp.set_metadata(temp.clone(), Metadata { permissions: Some(mode & 0o7777), ..Default::default() }).await;
        }
        // SFTP's rename won't replace a file: the original steps aside, and comes back if the
        // new one can't take its place.
        let old = format!("{}/.{file}.ferret-old", dir.trim_end_matches('/'));
        let aside = current.is_some() && s.sftp.rename(linux.clone(), old.clone()).await.is_ok();
        match s.sftp.rename(temp.clone(), linux.clone()).await {
            Ok(()) => {
                if aside {
                    let _ = s.sftp.remove_file(old).await;
                }
                Ok(())
            }
            Err(e) => {
                if aside {
                    let _ = s.sftp.rename(old, linux.clone()).await;
                }
                Err(e)
            }
        }
    };
    if let Err(e) = write.await {
        let _ = s.sftp.remove_file(temp.clone()).await;
        // A folder where no new file can be made: written in place instead.
        let mut f = s
            .sftp
            .open_with_flags(linux.clone(), OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE)
            .await
            .map_err(|_| sftp_err(remote, e))?;
        f.write_all(&bytes).await.map_err(|e| e.to_string())?;
        f.shutdown().await.map_err(|e| e.to_string())?;
    }
    let after = s.sftp.metadata(linux).await.map_err(|e| sftp_err(remote, e))?;
    Ok(Some(version(&after)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{block, fs, ssh};

    /// Saving an opened copy uploads it; a server copy changed meanwhile isn't overwritten without
    /// asking (no window to ask in a test: kept). In a folder of its own in the user's home there:
    /// `FERRET_TEST_SSH=<a ~/.ssh/config host> cargo test --lib ssh_edit_sync -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn ssh_edit_sync() {
        let Ok(alias) = std::env::var("FERRET_TEST_SSH") else { return };
        let id = crate::remote::test_connection(&alias).expect("connects");
        let s = block(ssh::session(&id)).unwrap();
        let dir = block(fs::create_folder(&crate::paths::remote_path(&id, &s.home), &format!("ferret-edit-test-{}", std::process::id()))).unwrap();
        let file = block(fs::create_file(&dir, "notes.txt", Some("first\n".into()))).unwrap();
        let (_, linux) = fs::split(&file).unwrap();
        let read = || String::from_utf8(block(s.sftp.read(linux.clone())).unwrap()).unwrap();
        let wait_for = |want: &str| {
            for _ in 0..40 {
                if read() == want {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            false
        };

        // Opened (downloaded and watched), then saved here: uploaded.
        let local = block(download(&file)).unwrap();
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "first\n");
        std::fs::write(&local, "second\n").unwrap();
        assert!(wait_for("second\n"), "not uploaded: {:?}", read());

        // Changed on the server too: the next save isn't uploaded over it.
        std::thread::sleep(Duration::from_secs(1));
        block(s.sftp.write(linux.clone(), b"from the server\n")).unwrap();
        std::fs::write(&local, "third\n").unwrap();
        std::thread::sleep(Duration::from_secs(3));
        assert_eq!(read(), "from the server\n");

        let (_, ldir) = fs::split(&dir).unwrap();
        block(fs::remove_all(&s, &ldir)).unwrap();
        let _ = std::fs::remove_dir_all(local.parent().unwrap());
        block(ssh::disconnect(&id));
    }
}
