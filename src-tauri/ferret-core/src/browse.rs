use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{db, listing, paths, places, recycle};

type Res<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn with_db<T>(f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> Res<T> {
    let mut conn = crate::app::try_state()?.db.lock().map_err(err)?;
    f(&mut conn).map_err(err)
}

// ---------- bootstrap ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub settings: Value,
    pub session: Option<Value>,
    pub places: places::Places,
    pub favorites: Vec<db::Favorite>,
    pub recents: Vec<db::Recent>,
    pub connections: Vec<crate::remote::Connection>,
}

pub async fn bootstrap() -> Res<Bootstrap> {
    let places = crate::rt::spawn_blocking(places::all).await.map_err(err)?;
    let connections = crate::remote::connections();
    with_db(|c| {
        Ok(Bootstrap {
            settings: db::load_settings(c),
            session: db::kv_get(c, "session"),
            places,
            favorites: db::favorites(c)?,
            recents: db::recents_present(c, 12)?,
            connections,
        })
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedState {
    pub settings: Value,
    pub favorites: Vec<db::Favorite>,
    pub recents: Vec<db::Recent>,
    pub connections: Vec<crate::remote::Connection>,
}

/// What other Ferret windows may have changed (re-read when a window gets focus).
pub fn shared_state() -> Res<SharedState> {
    let connections = crate::remote::connections();
    with_db(|c| Ok(SharedState { settings: db::load_settings(c), favorites: db::favorites(c)?, recents: db::recents_present(c, 12)?, connections }))
}

pub async fn get_places() -> Res<places::Places> {
    crate::rt::spawn_blocking(places::all).await.map_err(err)
}

// ---------- listing ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    pub path: String,
    pub entries: Vec<listing::Entry>,
    pub resolved: db::ResolvedView,
}

pub async fn list_dir(path: String) -> Res<DirListing> {
    if recycle::is_recycle(&path) {
        let entries = crate::rt::spawn_blocking(recycle::list).await.map_err(err)??;
        let resolved = with_db(|c| {
            let settings = db::load_settings(c);
            let mut r = db::resolve_view(c, &settings, recycle::PATH, None);
            // Like Explorer: most recently deleted first unless the user chose otherwise.
            if r.source == "global" {
                r.view["sortBy"] = "modified".into();
                r.view["sortDir"] = "desc".into();
            }
            Ok(r)
        })?;
        return Ok(DirListing { path: recycle::PATH.into(), entries, resolved });
    }
    if let Some(query) = crate::search::search_query_of(&path).map(str::to_string) {
        let q = query.clone();
        let entries = crate::rt::spawn_blocking(move || crate::search::list_matches(&q)).await.map_err(err)?;
        let path = format!("{}{query}", crate::search::SEARCH_PREFIX);
        let resolved = with_db(|c| Ok(db::resolve_view(c, &db::load_settings(c), &path, None)))?;
        return Ok(DirListing { path, entries, resolved });
    }
    // A server's folder (connecting first if need be: the user opened it).
    if paths::is_remote(&path) {
        let (path, entries) = crate::remote::fs::list(&path).await?;
        let profile = listing::detect_profile(&path, &entries);
        let resolved = with_db(|c| Ok(db::resolve_view(c, &db::load_settings(c), &path, profile)))?;
        return Ok(DirListing { path, entries, resolved });
    }
    if let Some(server) = listing::server_root(&path).map(str::to_string) {
        let path = format!(r"\\{server}");
        let p = path.clone();
        let entries = crate::rt::spawn_blocking(move || listing::list_server(&server).map_err(|e| crate::network::explain(&p, e))).await.map_err(err)??;
        let resolved = with_db(|c| Ok(db::resolve_view(c, &db::load_settings(c), &path, None)))?;
        return Ok(DirListing { path, entries, resolved });
    }
    // Long form so 8.3 aliases (MELTIN~1) share remembered views/recents with the real path.
    let path = paths::long_form(&path);
    let p = path.clone();
    let (path, entries) = crate::rt::spawn_blocking(move || {
        // Archives (and folders inside them) list like ordinary folders.
        let is_dir = std::fs::metadata(paths::for_io(&p)).map(|m| m.is_dir()).unwrap_or(false);
        if !is_dir && crate::archive::split_virtual(&p).is_some() {
            return crate::archive::list_children(&p).map(|e| (p, e));
        }
        // Compatibility junctions open their target; truly protected folders say "denied:".
        // A network folder's failure says what it means: it may just need a sign-in.
        listing::list_following(&p).map_err(|e| crate::network::explain(&p, e))
    })
    .await
    .map_err(err)??;
    let profile = listing::detect_profile(&path, &entries);
    let resolved = with_db(|c| {
        let settings = db::load_settings(c);
        Ok(db::resolve_view(c, &settings, &path, profile))
    })?;
    Ok(DirListing { path, entries, resolved })
}

#[derive(Serialize)]
pub struct ResolvedInput {
    pub path: String,
    /// "dir" | "file" | "missing"
    pub kind: &'static str,
}

/// Turns whatever was typed in the path bar into a real path: `~`, `%VAR%`,
/// Linux paths (`/home/me` → current or default WSL distro), `\\wsl$`.
pub async fn resolve_input(input: String, current: Option<String>) -> Res<ResolvedInput> {
    // A server's folder: `\\ssh:\home.local\srv`, `home.local:/srv`, `ssh://home.local/srv`.
    if let Some(path) = crate::remote::parse_input(input.trim().trim_matches('"')) {
        let (path, kind) = crate::remote::fs::resolve(&path).await?;
        return Ok(ResolvedInput { path, kind });
    }
    crate::rt::spawn_blocking(move || {
        let mut s = input.trim().trim_matches('"').to_string();
        // How a NAS's own pages write its address.
        if s.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("smb://")) {
            s = format!(r"\\{}", s[6..].replace('/', "\\"));
        }
        if recycle::is_recycle(&s) {
            return Ok(ResolvedInput { path: recycle::PATH.into(), kind: "dir" });
        }
        if let Some(q) = crate::search::search_query_of(&s) {
            return Ok(ResolvedInput { path: format!("{}{q}", crate::search::SEARCH_PREFIX), kind: "dir" });
        }
        if s == "~" || s.starts_with("~/") || s.starts_with("~\\") {
            let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
            s = format!("{home}{}", &s[1..]);
        }
        // %VAR% expansion
        while let Some(start) = s.find('%') {
            let Some(len) = s[start + 1..].find('%') else { break };
            let var = &s[start + 1..start + 1 + len];
            let Ok(val) = std::env::var(var) else { break };
            s = format!("{}{}{}", &s[..start], val, &s[start + 2 + len..]);
        }
        // Git-Bash `/c/Users` and WSL `/mnt/c/Users` style paths → the Windows drive.
        let drive_style = {
            let rest = s.strip_prefix("/mnt/").unwrap_or_else(|| s.strip_prefix('/').unwrap_or(""));
            let mut it = rest.splitn(2, '/');
            match (it.next(), it.next()) {
                (Some(d), tail) if s.starts_with('/') && d.len() == 1 && d.chars().all(|c| c.is_ascii_alphabetic())
                    && std::path::Path::new(&format!("{d}:\\")).exists() =>
                {
                    Some(format!("{}:\\{}", d.to_ascii_uppercase(), tail.unwrap_or("").replace('/', "\\")))
                }
                _ => None,
            }
        };
        if let Some(win) = drive_style {
            s = win;
        } else if s.starts_with('/') {
            let distro = current
                .as_deref()
                .and_then(paths::wsl_distro)
                .or_else(places::default_distro)
                .ok_or("No WSL distro installed")?;
            s = format!("{}{}", paths::wsl_root(&distro), s.replace('/', "\\"));
        }
        // A server on its own (`\\nas`, `\\wsl.localhost`) lists its shared folders.
        if let Some(server) = listing::server_root(&s) {
            return Ok(ResolvedInput { path: format!(r"\\{server}"), kind: "dir" });
        }
        let path = paths::long_form(&s);
        let kind = match std::fs::metadata(paths::for_io(&path)) {
            Ok(m) if m.is_dir() => "dir",
            // Archives (and paths inside them) browse like folders.
            Ok(_) if crate::archive::archive_kind(&path).is_some() => "dir",
            Ok(_) => "file",
            Err(e) => match crate::archive::split_virtual(&path) {
                Some(_) => match crate::archive::list_children(&path) {
                    Ok(_) => "dir",
                    Err(e) if e.contains("password") => "dir",
                    Err(_) => "file",
                },
                // A NAS that wants a sign-in, or isn't reachable: opening it asks, or says so.
                None if crate::network::worth_opening(&path, &e) => "dir",
                None => "missing",
            },
        };
        Ok(ResolvedInput { path, kind })
    })
    .await
    .map_err(err)?
}

pub fn open_path(path: String) -> Res<()> {
    // Server files open through remote_open (downloaded, then uploaded back when saved).
    if paths::is_remote(&path) {
        return Err("Open server files with remote_open".into());
    }
    crate::shell::open_path(&paths::for_io(&path))
}

/// Off the main thread: watching a folder opens it, which waits on a WSL distro that's starting
/// or a network share that's gone (20–30 s), and the window would stop answering meanwhile.
pub fn watch_dirs(paths: Vec<String>) -> Res<()> {
    static LATEST: AtomicU64 = AtomicU64::new(0);
    let this = LATEST.fetch_add(1, Ordering::Relaxed) + 1;
    // Servers can't be watched from here: their shown folders are listed again now and then.
    let (remote, local): (Vec<String>, Vec<String>) = paths.into_iter().partition(|p| crate::paths::is_remote(p));
    let mut watcher = crate::app::try_state()?.watcher.lock().map_err(err)?;
    // A newer list came while this one waited for a slow one: it wins.
    if LATEST.load(Ordering::Relaxed) != this {
        return Ok(());
    }
    crate::remote::set_shown(&remote);
    watcher.set(local);
    Ok(())
}

// ---------- settings / views ----------

pub fn save_settings(settings: Value) -> Res<()> {
    with_db(|c| db::kv_set(c, "settings", &settings))
}

pub fn save_session(session: Value) -> Res<()> {
    with_db(|c| db::kv_set(c, "session", &session))
}

pub fn set_folder_view(path: String, view: Value, recursive: bool) -> Res<()> {
    with_db(|c| db::set_folder_view(c, &path, &view, recursive))
}

pub fn clear_folder_view(path: String) -> Res<()> {
    with_db(|c| db::clear_folder_view(c, &path))
}

pub fn list_folder_views() -> Res<Vec<db::FolderView>> {
    with_db(|c| db::list_folder_views(c))
}

pub fn clear_all_folder_views() -> Res<()> {
    with_db(|c| c.execute("DELETE FROM view_settings", []).map(|_| ()))
}

// ---------- favorites / recents ----------

pub fn add_favorite(path: String, name: String, is_dir: bool) -> Res<Vec<db::Favorite>> {
    with_db(|c| {
        db::add_favorite(c, &path, &name, is_dir)?;
        db::favorites(c)
    })
}

pub fn remove_favorite(path: String) -> Res<Vec<db::Favorite>> {
    with_db(|c| {
        db::remove_favorite(c, &path)?;
        db::favorites(c)
    })
}

pub fn update_favorite(path: String, name: String, color: Option<String>) -> Res<Vec<db::Favorite>> {
    with_db(|c| {
        db::update_favorite(c, &path, &name, color.as_deref())?;
        db::favorites(c)
    })
}

pub fn reorder_favorites(paths: Vec<String>) -> Res<Vec<db::Favorite>> {
    with_db(|c| {
        db::reorder_favorites(c, &paths)?;
        db::favorites(c)
    })
}

pub fn push_recent(path: String) -> Res<Vec<db::Recent>> {
    with_db(|c| {
        db::push_recent(c, &path)?;
        db::recents_present(c, 12)
    })
}

pub fn remove_recent(path: String) -> Res<Vec<db::Recent>> {
    with_db(|c| {
        db::remove_recent(c, &path)?;
        db::recents_present(c, 12)
    })
}

pub fn clear_recents() -> Res<Vec<db::Recent>> {
    with_db(|c| {
        c.execute("DELETE FROM recents", [])?;
        Ok(vec![])
    })
}

/// PNG on disk to show under the cursor while dragging files out to other apps.
pub async fn drag_icon(path: Option<String>) -> Res<String> {
    crate::rt::spawn_blocking(move || {
        let dir = dirs::cache_dir().ok_or("no cache dir")?.join("Ferret").join("drag");
        std::fs::create_dir_all(&dir).map_err(err)?;
        if let Some(p) = path {
            let png = crate::shell::thumbnail_png(&p, 96, false).or_else(|_| crate::shell::thumbnail_png(&p, 64, true));
            if let Ok(bytes) = png {
                let out = dir.join("current.png");
                std::fs::write(&out, bytes).map_err(err)?;
                return Ok(out.to_string_lossy().into_owned());
            }
        }
        let out = dir.join("ferret.png");
        if !out.exists() {
            std::fs::write(&out, include_bytes!("../../icons/32x32.png")).map_err(err)?;
        }
        Ok(out.to_string_lossy().into_owned())
    })
    .await
    .map_err(err)?
}
