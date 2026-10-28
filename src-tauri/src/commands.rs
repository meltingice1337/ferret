use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::sync::Mutex;
use tauri::{State, WebviewWindow};

use crate::{db, listing, paths, places, recycle, watch::DirWatcher};

pub struct AppState {
    pub db: Mutex<Connection>,
    pub watcher: Mutex<DirWatcher>,
}

type Res<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn with_db<T>(state: &State<'_, AppState>, f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> Res<T> {
    let mut conn = state.db.lock().map_err(err)?;
    f(&mut conn).map_err(err)
}

// ---------- bootstrap ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    settings: Value,
    session: Option<Value>,
    places: places::Places,
    favorites: Vec<db::Favorite>,
    recents: Vec<db::Recent>,
    connections: Vec<crate::remote::Connection>,
}

#[tauri::command]
pub async fn bootstrap(state: State<'_, AppState>) -> Res<Bootstrap> {
    let places = tauri::async_runtime::spawn_blocking(places::all).await.map_err(err)?;
    let connections = crate::remote::connections();
    with_db(&state, |c| {
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
    settings: Value,
    favorites: Vec<db::Favorite>,
    recents: Vec<db::Recent>,
    connections: Vec<crate::remote::Connection>,
}

/// What other Ferret windows may have changed (re-read when a window gets focus).
#[tauri::command]
pub fn shared_state(state: State<'_, AppState>) -> Res<SharedState> {
    let connections = crate::remote::connections();
    with_db(&state, |c| Ok(SharedState { settings: db::load_settings(c), favorites: db::favorites(c)?, recents: db::recents_present(c, 12)?, connections }))
}

#[tauri::command]
pub async fn get_places() -> Res<places::Places> {
    tauri::async_runtime::spawn_blocking(places::all).await.map_err(err)
}

// ---------- listing ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    path: String,
    entries: Vec<listing::Entry>,
    resolved: db::ResolvedView,
}

#[tauri::command]
pub async fn list_dir(state: State<'_, AppState>, path: String) -> Res<DirListing> {
    if recycle::is_recycle(&path) {
        let entries = tauri::async_runtime::spawn_blocking(recycle::list).await.map_err(err)??;
        let resolved = with_db(&state, |c| {
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
        let entries = tauri::async_runtime::spawn_blocking(move || crate::search::list_matches(&q)).await.map_err(err)?;
        let path = format!("{}{query}", crate::search::SEARCH_PREFIX);
        let resolved = with_db(&state, |c| Ok(db::resolve_view(c, &db::load_settings(c), &path, None)))?;
        return Ok(DirListing { path, entries, resolved });
    }
    // A server's folder (connecting first if need be: the user opened it).
    if paths::is_remote(&path) {
        let (path, entries) = crate::remote::fs::list(&path).await?;
        let profile = listing::detect_profile(&path, &entries);
        let resolved = with_db(&state, |c| Ok(db::resolve_view(c, &db::load_settings(c), &path, profile)))?;
        return Ok(DirListing { path, entries, resolved });
    }
    if let Some(server) = listing::server_root(&path).map(str::to_string) {
        let path = format!(r"\\{server}");
        let entries = tauri::async_runtime::spawn_blocking(move || listing::list_server(&server)).await.map_err(err)??;
        let resolved = with_db(&state, |c| Ok(db::resolve_view(c, &db::load_settings(c), &path, None)))?;
        return Ok(DirListing { path, entries, resolved });
    }
    // Long form so 8.3 aliases (MELTIN~1) share remembered views/recents with the real path.
    let path = paths::long_form(&path);
    let p = path.clone();
    let (path, entries) = tauri::async_runtime::spawn_blocking(move || {
        // Archives (and folders inside them) list like ordinary folders.
        let is_dir = std::fs::metadata(paths::for_io(&p)).map(|m| m.is_dir()).unwrap_or(false);
        if !is_dir && crate::archive::split_virtual(&p).is_some() {
            return crate::archive::list_children(&p).map(|e| (p, e));
        }
        // Compatibility junctions open their target; truly protected folders say "denied:".
        listing::list_following(&p)
    })
    .await
    .map_err(err)??;
    let profile = listing::detect_profile(&path, &entries);
    let resolved = with_db(&state, |c| {
        let settings = db::load_settings(c);
        Ok(db::resolve_view(c, &settings, &path, profile))
    })?;
    Ok(DirListing { path, entries, resolved })
}

#[derive(Serialize)]
pub struct ResolvedInput {
    path: String,
    /// "dir" | "file" | "missing"
    kind: &'static str,
}

/// Turns whatever was typed in the path bar into a real path: `~`, `%VAR%`,
/// Linux paths (`/home/me` → current or default WSL distro), `\\wsl$`.
#[tauri::command]
pub async fn resolve_input(input: String, current: Option<String>) -> Res<ResolvedInput> {
    // A server's folder: `\\ssh:\home.local\srv`, `home.local:/srv`, `ssh://home.local/srv`.
    if let Some(path) = crate::remote::parse_input(input.trim().trim_matches('"')) {
        let (path, kind) = crate::remote::fs::resolve(&path).await?;
        return Ok(ResolvedInput { path, kind });
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut s = input.trim().trim_matches('"').to_string();
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
            Err(_) => match crate::archive::split_virtual(&path) {
                Some(_) => match crate::archive::list_children(&path) {
                    Ok(_) => "dir",
                    Err(e) if e.contains("password") => "dir",
                    Err(_) => "file",
                },
                None => "missing",
            },
        };
        Ok(ResolvedInput { path, kind })
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub fn open_path(path: String) -> Res<()> {
    // Server files open through remote_open (downloaded, then uploaded back when saved).
    if paths::is_remote(&path) {
        return Err("Open server files with remote_open".into());
    }
    tauri_plugin_opener::open_path(paths::for_io(&path), None::<&str>).map_err(err)
}

#[tauri::command]
pub fn watch_dirs(state: State<'_, AppState>, paths: Vec<String>) -> Res<()> {
    // Servers can't be watched from here: their shown folders are listed again now and then.
    let (remote, local): (Vec<String>, Vec<String>) = paths.into_iter().partition(|p| crate::paths::is_remote(p));
    crate::remote::set_shown(&remote);
    state.watcher.lock().map_err(err)?.set(local);
    Ok(())
}

// ---------- settings / views ----------

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, settings: Value) -> Res<()> {
    with_db(&state, |c| db::kv_set(c, "settings", &settings))
}

#[tauri::command]
pub fn save_session(state: State<'_, AppState>, session: Value) -> Res<()> {
    with_db(&state, |c| db::kv_set(c, "session", &session))
}

#[tauri::command]
pub fn set_folder_view(state: State<'_, AppState>, path: String, view: Value, recursive: bool) -> Res<()> {
    with_db(&state, |c| db::set_folder_view(c, &path, &view, recursive))
}

#[tauri::command]
pub fn clear_folder_view(state: State<'_, AppState>, path: String) -> Res<()> {
    with_db(&state, |c| db::clear_folder_view(c, &path))
}

#[tauri::command]
pub fn list_folder_views(state: State<'_, AppState>) -> Res<Vec<db::FolderView>> {
    with_db(&state, |c| db::list_folder_views(c))
}

#[tauri::command]
pub fn clear_all_folder_views(state: State<'_, AppState>) -> Res<()> {
    with_db(&state, |c| c.execute("DELETE FROM view_settings", []).map(|_| ()))
}

// ---------- favorites / recents ----------

#[tauri::command]
pub fn add_favorite(state: State<'_, AppState>, path: String, name: String, is_dir: bool) -> Res<Vec<db::Favorite>> {
    with_db(&state, |c| {
        db::add_favorite(c, &path, &name, is_dir)?;
        db::favorites(c)
    })
}

#[tauri::command]
pub fn remove_favorite(state: State<'_, AppState>, path: String) -> Res<Vec<db::Favorite>> {
    with_db(&state, |c| {
        db::remove_favorite(c, &path)?;
        db::favorites(c)
    })
}

#[tauri::command]
pub fn update_favorite(state: State<'_, AppState>, path: String, name: String, color: Option<String>) -> Res<Vec<db::Favorite>> {
    with_db(&state, |c| {
        db::update_favorite(c, &path, &name, color.as_deref())?;
        db::favorites(c)
    })
}

#[tauri::command]
pub fn reorder_favorites(state: State<'_, AppState>, paths: Vec<String>) -> Res<Vec<db::Favorite>> {
    with_db(&state, |c| {
        db::reorder_favorites(c, &paths)?;
        db::favorites(c)
    })
}

#[tauri::command]
pub fn push_recent(state: State<'_, AppState>, path: String) -> Res<Vec<db::Recent>> {
    with_db(&state, |c| {
        db::push_recent(c, &path)?;
        db::recents_present(c, 12)
    })
}

#[tauri::command]
pub fn remove_recent(state: State<'_, AppState>, path: String) -> Res<Vec<db::Recent>> {
    with_db(&state, |c| {
        db::remove_recent(c, &path)?;
        db::recents_present(c, 12)
    })
}

#[tauri::command]
pub fn clear_recents(state: State<'_, AppState>) -> Res<Vec<db::Recent>> {
    with_db(&state, |c| {
        c.execute("DELETE FROM recents", [])?;
        Ok(vec![])
    })
}

/// PNG on disk to show under the cursor while dragging files out to other apps.
#[tauri::command]
pub async fn drag_icon(path: Option<String>) -> Res<String> {
    tauri::async_runtime::spawn_blocking(move || {
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
            std::fs::write(&out, include_bytes!("../icons/32x32.png")).map_err(err)?;
        }
        Ok(out.to_string_lossy().into_owned())
    })
    .await
    .map_err(err)?
}

// ---------- window ----------

/// Applies the window backdrop. Returns false if the OS refused (e.g. Mica on
/// Windows 10), in which case the UI paints a solid background.
#[tauri::command]
pub fn set_backdrop(window: WebviewWindow, mode: String, dark: bool) -> bool {
    #[cfg(windows)]
    {
        use window_vibrancy::{apply_acrylic, apply_mica, clear_acrylic, clear_mica};
        let _ = clear_mica(&window);
        let _ = clear_acrylic(&window);
        match mode.as_str() {
            "mica" => apply_mica(&window, Some(dark)).is_ok(),
            "acrylic" => {
                let tint = if dark { (18, 12, 20, 140) } else { (245, 240, 245, 120) };
                apply_acrylic(&window, Some(tint)).is_ok()
            }
            _ => false,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (window, mode, dark);
        false
    }
}
