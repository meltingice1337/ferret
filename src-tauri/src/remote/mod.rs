//! Connections to other computers' files: SSH servers for now (FTP or SMB could follow as other
//! `Connection::kind`s). A server's folders show as `\\ssh:\<connection>\…` and work like
//! folders here: listing, previews, copy and move both ways, rename, the server's trash, folder
//! sizes and search (with Ferret's helper where the server runs Linux), and files opened from
//! it are uploaded back when saved (edit.rs).
//!
//! A connection only connects when the user opens one of its folders; background work (folder
//! sizes, search, refreshing a shown folder) uses a session that's open and never opens one.

pub mod config;
pub mod edit;
pub mod fs;
pub mod known_hosts;
pub mod ops;
pub mod prompt;
pub mod secrets;
pub mod ssh;

use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::AppState;
use crate::{db, paths};
pub use config::Connection;

type Res<T> = Result<T, String>;

/// What this module needs from the app: telling the window things, and the database. Set once
/// at startup, behind a trait object: code the tests run (connecting, listing) then doesn't
/// pull in the window runtime, whose dialogs a test program can't load.
trait AppLink: Send + Sync {
    fn emit(&self, event: &str, payload: serde_json::Value);
    fn db(&self, f: &mut dyn FnMut(&rusqlite::Connection));
}

struct Tauri(AppHandle);

impl AppLink for Tauri {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.0.emit(event, payload);
    }
    fn db(&self, f: &mut dyn FnMut(&rusqlite::Connection)) {
        if let Some(state) = self.0.try_state::<AppState>()
            && let Ok(conn) = state.db.lock()
        {
            f(&conn);
        }
    }
}

static LINK: OnceLock<Box<dyn AppLink>> = OnceLock::new();

pub fn init(app: &AppHandle) {
    let _ = LINK.set(Box::new(Tauri(app.clone())));
    std::thread::spawn(poll_shown);
}

/// A window can be told things (and asked: prompt.rs).
pub fn has_window() -> bool {
    LINK.get().is_some()
}

/// Sends an event to the window.
pub fn emit(event: &str, payload: impl Serialize) {
    if let Some(link) = LINK.get() {
        link.emit(event, serde_json::to_value(payload).unwrap_or_default());
    }
}

/// Runs async remote work from blocking code: job threads, the file scheme, folder-size workers.
/// (On one of the runtime's own threads it steps aside first, as `block_in_place` does.)
pub fn block<F: Future>(f: F) -> F::Output {
    let handle = tauri::async_runtime::handle();
    tokio::task::block_in_place(|| handle.block_on(f))
}

// ---------- saved connections ----------

const KEY: &str = "remote_connections";

fn with_db<T>(f: impl FnOnce(&rusqlite::Connection) -> T) -> Option<T> {
    let link = LINK.get()?;
    let mut f = Some(f);
    let mut out = None;
    link.db(&mut |c| out = f.take().map(|f| f(c)));
    out
}

pub fn connections() -> Vec<Connection> {
    with_db(|c| db::kv_get(c, KEY)).flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

fn store(all: &[Connection]) -> Res<()> {
    let v = serde_json::to_value(all).map_err(|e| e.to_string())?;
    with_db(|c| db::kv_set(c, KEY, &v)).ok_or("no database")?.map_err(|e| e.to_string())
}

pub fn find_connection(id: &str) -> Option<Connection> {
    // The connection form's "Test" uses one that isn't saved yet.
    if let Some(c) = testing().lock().unwrap().get(id) {
        return Some(c.clone());
    }
    connections().into_iter().find(|c| c.id == id)
}

fn testing() -> &'static Mutex<HashMap<String, Connection>> {
    static T: OnceLock<Mutex<HashMap<String, Connection>>> = OnceLock::new();
    T.get_or_init(Default::default)
}

/// A server path typed in the path bar, as a Ferret path: `\\ssh:\home.local\srv`,
/// `home.local:/srv` (as scp writes it) or `ssh://[user@]home.local[:port]/srv`. The server is
/// a connection's id, name or host.
pub fn parse_input(s: &str) -> Option<String> {
    if paths::is_remote(s) {
        return Some(paths::normalize(s));
    }
    let (server, linux) = if let Some(rest) = s.strip_prefix("ssh://") {
        let (server, path) = rest.split_once('/').map(|(a, b)| (a, format!("/{b}"))).unwrap_or((rest, "/".into()));
        let server = server.rsplit_once('@').map_or(server, |(_, h)| h);
        (server.rsplit_once(':').filter(|(_, p)| p.parse::<u16>().is_ok()).map_or(server, |(h, _)| h), path)
    } else {
        // `C:/x` is a drive: a server name is longer than one letter.
        let (server, path) = s.split_once(':')?;
        if server.len() < 2 || server.contains(['\\', '/']) || !(path.starts_with('/') || path.starts_with('~') || path.is_empty()) {
            return None;
        }
        (server, path.to_string())
    };
    let c = connections().into_iter().find(|c| {
        c.id.eq_ignore_ascii_case(server) || c.name.eq_ignore_ascii_case(server) || c.host.eq_ignore_ascii_case(server)
            || c.ssh_alias.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(server))
    })?;
    // `~` and paths without a slash are in the home folder: resolved once connected (kind()).
    let linux = if linux.is_empty() || linux == "~" { "/~".to_string() } else { linux.replacen("~/", "/~/", 1) };
    Some(paths::remote_path(&c.id, &linux))
}

/// How Windows' `ssh` reaches a connection: its ~/.ssh/config alias (with everything the config
/// says), else `user@host` with its port and key file. (Destination, options before it.)
pub fn ssh_destination(id: &str) -> Option<(String, Vec<String>)> {
    let c = find_connection(id)?;
    let mut args = Vec::new();
    let dest = match &c.ssh_alias {
        Some(alias) => alias.clone(),
        None => {
            if c.port != 22 {
                args.extend(["-p".to_string(), c.port.to_string()]);
            }
            if c.user.is_empty() { c.host.clone() } else { format!("{}@{}", c.user, c.host) }
        }
    };
    if let Some(key) = c.identity_file.filter(|k| !k.trim().is_empty()) {
        args.extend(["-i".to_string(), key]);
    }
    Some((dest, args))
}

/// The display name of a connection (for paths shown to the user).
pub fn name_of(id: &str) -> String {
    find_connection(id).map(|c| c.name).unwrap_or_else(|| id.to_string())
}

/// Per-session state elsewhere (the helper's placement, the search index) is dropped.
pub fn forget_session(id: &str) {
    crate::linux::forget_ssh(id);
    crate::search::remote::forget(id);
}

#[tauri::command]
pub fn remote_connections() -> Vec<Connection> {
    connections()
}

/// Adds or updates a connection (a new one gets its id here).
#[tauri::command]
pub async fn remote_save_connection(mut connection: Connection) -> Res<Vec<Connection>> {
    connection.name = connection.name.trim().to_string();
    connection.host = connection.host.trim().to_string();
    if connection.name.is_empty() {
        connection.name = connection.ssh_alias.clone().filter(|a| !a.is_empty()).unwrap_or_else(|| connection.host.clone());
    }
    if connection.host.is_empty() && connection.ssh_alias.is_none() {
        return Err("Enter the server's address".into());
    }
    let mut all = connections();
    match all.iter_mut().find(|c| c.id == connection.id && !connection.id.is_empty()) {
        Some(c) => {
            // Settings that change how it logs in or where it opens take effect at the next connect.
            *c = connection.clone();
        }
        None => {
            let taken: Vec<String> = all.iter().map(|c| c.id.clone()).collect();
            connection.id = config::new_id(&connection.name, &taken);
            all.push(connection.clone());
        }
    }
    store(&all)?;
    log::info!("remote: saved connection {} ({})", connection.name, connection.id);
    notify_changed();
    Ok(all)
}

#[tauri::command]
pub async fn remote_delete_connection(id: String) -> Res<Vec<Connection>> {
    ssh::disconnect(&id).await;
    let mut all = connections();
    if let Some(c) = all.iter().find(|c| c.id == id) {
        let host = if c.host.is_empty() { c.ssh_alias.clone().unwrap_or_default() } else { c.host.clone() };
        secrets::forget(&secrets::password_target(&c.user, &host, c.port));
    }
    all.retain(|c| c.id != id);
    store(&all)?;
    notify_changed();
    Ok(all)
}

/// Other windows pick up the list when they get the focus; this one at once.
fn notify_changed() {
    emit("remote-connections", connections());
}

/// The hosts ~/.ssh/config names, for the connection form to start from.
#[tauri::command]
pub fn remote_config_hosts() -> Vec<config::HostConfig> {
    config::config_hosts()
}

/// Opens a connection (if it isn't open) and gives back the folder to show: its start folder,
/// else the user's home.
#[tauri::command]
pub async fn remote_connect(id: String) -> Res<String> {
    let s = ssh::session(&id).await?;
    let start = find_connection(&id).and_then(|c| c.start_dir).filter(|d| d.starts_with('/'));
    Ok(paths::remote_path(&id, start.as_deref().unwrap_or(&s.home)))
}

#[tauri::command]
pub async fn remote_disconnect(id: String) {
    ssh::disconnect(&id).await;
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    home: String,
    /// What the server runs ("x86_64"), or None (SFTP only: no helper).
    arch: Option<String>,
}

/// The connection form's Test: connects with what's in the form, then disconnects.
#[tauri::command]
pub async fn remote_test(mut connection: Connection) -> Res<TestResult> {
    connection.id = format!("test-{}", fs::now_ms());
    if connection.name.trim().is_empty() {
        connection.name = connection.host.clone();
    }
    let id = connection.id.clone();
    testing().lock().unwrap().insert(id.clone(), connection);
    let res = ssh::session(&id).await.map(|s| TestResult { home: s.home.clone(), arch: s.arch.clone() });
    ssh::disconnect(&id).await;
    testing().lock().unwrap().remove(&id);
    res
}

/// Connects to a ~/.ssh/config host, for tests run against a real server; its id.
#[cfg(test)]
pub fn test_connection(alias: &str) -> Res<String> {
    let cfg = config::resolve(alias);
    // Each test its own session (one disconnecting mustn't end another's).
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let c = Connection {
        id: format!("test-{alias}-{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)),
        name: alias.to_string(),
        kind: "ssh".into(),
        host: cfg.host_name.clone().unwrap_or_else(|| alias.to_string()),
        port: cfg.port.unwrap_or(22),
        user: cfg.user.clone().unwrap_or_default(),
        ssh_alias: Some(alias.to_string()),
        auth: "auto".into(),
        identity_file: None,
        start_dir: None,
        search: true,
    };
    let id = c.id.clone();
    testing().lock().unwrap().insert(id.clone(), c);
    block(ssh::session(&id)).map(|_| id)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionState {
    id: String,
    connected: bool,
}

#[tauri::command]
pub fn remote_status() -> Vec<ConnectionState> {
    let open = ssh::connected_ids();
    connections().into_iter().map(|c| ConnectionState { connected: open.contains(&c.id), id: c.id }).collect()
}

/// A local copy of a server file for a preview (or None: too big; video streams instead).
#[tauri::command]
pub async fn remote_materialize(path: String) -> Res<String> {
    const MAX: u64 = 512 << 20;
    fs::materialize(&path, MAX).await
}

// ---------- refreshing shown folders ----------

/// The server folders tabs show (from `watch_dirs`), with what they looked like last.
fn shown() -> &'static Mutex<HashMap<String, u64>> {
    static S: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// The folders tabs show now (local ones are watched by watch.rs).
pub fn set_shown(dirs: &[String]) {
    let mut s = shown().lock().unwrap();
    s.retain(|d, _| dirs.contains(d));
    for d in dirs {
        s.entry(d.clone()).or_insert(0);
    }
}

/// A tab showing a server's folder keeps its session open.
pub fn touch_shown() {
    let dirs: Vec<String> = shown().lock().unwrap().keys().cloned().collect();
    for d in dirs {
        if let Some((id, _)) = paths::remote_parts(&d)
            && let Some(s) = ssh::connected(id)
        {
            s.touch();
        }
    }
}

/// Servers can't tell Ferret about changes the way Windows does: every few seconds, the shown
/// folders of open sessions are listed again and `fs-changed` is sent for those that changed.
fn poll_shown() {
    loop {
        std::thread::sleep(Duration::from_secs(3));
        let dirs: Vec<String> = shown().lock().unwrap().keys().cloned().collect();
        let mut changed = Vec::new();
        for dir in dirs {
            let Some((id, linux)) = paths::remote_parts(&dir).map(|(i, l)| (i.to_string(), l)) else { continue };
            let Some(s) = ssh::connected(&id) else { continue };
            let Ok(rd) = block(async { tokio::time::timeout(Duration::from_secs(10), s.sftp.read_dir(linux)).await }) else { continue };
            let Ok(rd) = rd else { continue };
            let mut items: Vec<(String, u64, u32)> = rd.map(|e| { let m = e.metadata(); (e.file_name(), m.size.unwrap_or(0), m.mtime.unwrap_or(0)) }).collect();
            items.sort();
            let sig = crate::archive::fnv1a(format!("{items:?}").as_bytes());
            let mut all = shown().lock().unwrap();
            if let Some(v) = all.get_mut(&dir) {
                if *v != 0 && *v != sig {
                    changed.push(dir.clone());
                }
                *v = sig;
            }
        }
        if !changed.is_empty() {
            emit("fs-changed", changed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn typed_paths() {
        assert_eq!(parse_input(r"\\ssh:\home.local\srv"), Some(r"\\ssh:\home.local\srv".into()));
        // Without saved connections, nothing else is a server.
        assert_eq!(parse_input("C:/Users"), None);
        assert_eq!(parse_input("home.local:/srv"), None);
    }

    /// A server's files for real, in a folder of its own in the user's home (removed at the end):
    /// `FERRET_TEST_SSH=<a ~/.ssh/config host> cargo test --lib ssh_files -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn ssh_files() {
        let Ok(alias) = std::env::var("FERRET_TEST_SSH") else { return };
        let id = test_connection(&alias).expect("connects");
        let s = block(ssh::session(&id)).unwrap();
        println!("{}: home {}, {}", s.name, s.home, s.arch.as_deref().unwrap_or("SFTP only"));
        let name = format!("ferret-remote-test-{}", std::process::id());
        let home = paths::remote_path(&id, &s.home);
        let dir = block(fs::create_folder(&home, &name)).unwrap();
        assert_eq!(dir, paths::remote_path(&id, &format!("{}/{name}", s.home)));

        // Files: made, read, listed, renamed (a taken name is refused).
        let a = block(fs::create_file(&dir, "a.txt", Some("hello".into()))).unwrap();
        assert_eq!(block(fs::read_head(&a, 100)).unwrap(), (b"hello".to_vec(), 5));
        assert_eq!(fs::read_range(&a, 1, 3).unwrap().0, b"ell");
        let (listed, entries) = block(fs::list(&dir)).unwrap();
        assert_eq!(listed, dir);
        assert!(entries.iter().any(|e| e.name == "a.txt" && e.size == 5 && !e.is_dir && e.ext == "txt"));
        block(fs::create_file(&dir, "c.txt", None)).unwrap();
        assert!(block(fs::rename(&a, "c.txt")).is_err());
        let b = block(fs::rename(&a, "b.txt")).unwrap();

        // The copy engine's view: write, read back, times kept.
        let rfs = fs::RemoteFs::shared(&id);
        let big = crate::listing::join(&dir, "big.bin");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let mut w = rfs.create(&big).unwrap();
        w.write_all(&data).unwrap();
        drop(w);
        let mut back = Vec::new();
        rfs.open(&big).unwrap().read_to_end(&mut back).unwrap();
        assert!(back == data, "{} bytes back of {}", back.len(), data.len());
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        rfs.finish(&big, &crate::vfs::Meta { modified: Some(when), ..Default::default() });
        assert_eq!(rfs.stat(&big).unwrap().modified, Some(when));

        // The server's trash, and back.
        let (_, lb) = fs::split(&b).unwrap();
        let at = block(fs::trash_one(&s, &id, &lb)).unwrap();
        assert!(at.contains(r"\.local\share\Trash\files\"), "{at}");
        assert!(!block(fs::exists(&s, &lb)));
        let (_, lat) = fs::split(&at).unwrap();
        let info = format!("{}/info/{}.trashinfo", lat.rsplit_once("/files/").unwrap().0, lat.rsplit('/').next().unwrap());
        let text = String::from_utf8(block(s.sftp.read(info.clone())).unwrap()).unwrap();
        assert!(text.starts_with("[Trash Info]\nPath=") && text.contains(&name), "{text}");
        block(fs::untrash(&s, &lat, &lb)).unwrap();
        assert!(block(fs::exists(&s, &lb)) && !block(fs::exists(&s, &info)));

        // Gone for good.
        let (_, ldir) = fs::split(&dir).unwrap();
        block(fs::remove_all(&s, &ldir)).unwrap();
        assert!(!block(fs::exists(&s, &ldir)));
        assert!(block(fs::remove_all(&s, &s.home)).is_err(), "never the home folder");
        block(ssh::disconnect(&id));
    }
}
