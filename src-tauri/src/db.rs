use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths;

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS view_settings (
             path_key TEXT PRIMARY KEY, path TEXT NOT NULL, json TEXT NOT NULL,
             recursive INTEGER NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS favorites (
             path_key TEXT PRIMARY KEY, path TEXT NOT NULL, name TEXT NOT NULL,
             is_dir INTEGER NOT NULL, color TEXT, position INTEGER NOT NULL);
         DROP TABLE IF EXISTS quick_slots;
         CREATE TABLE IF NOT EXISTS recents (
             path_key TEXT PRIMARY KEY, path TEXT NOT NULL,
             last_opened INTEGER NOT NULL, count INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS folder_sizes (
             path_key TEXT PRIMARY KEY, path TEXT NOT NULL, bytes INTEGER NOT NULL, files INTEGER,
             partial INTEGER NOT NULL, counted_at INTEGER NOT NULL, stale INTEGER NOT NULL DEFAULT 0);",
    )?;
    // Sizes of folders not looked at for two months aren't worth keeping.
    conn.execute("DELETE FROM folder_sizes WHERE counted_at < ?1", [now() - 60 * 24 * 3_600_000])?;
    Ok(conn)
}

/// A folder size saved by an earlier count (in any window).
pub struct SavedSize {
    pub path: String,
    pub bytes: u64,
    pub files: Option<u64>,
    pub partial: bool,
    /// Unix milliseconds.
    pub counted_at: i64,
    /// Something inside changed since.
    pub stale: bool,
}

pub fn folder_sizes(conn: &Connection, keys: &[String]) -> rusqlite::Result<Vec<(String, SavedSize)>> {
    let mut out = Vec::new();
    for chunk in keys.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut st = conn.prepare(&format!(
            "SELECT path_key, path, bytes, files, partial, counted_at, stale FROM folder_sizes WHERE path_key IN ({marks})"
        ))?;
        let rows = st.query_map(rusqlite::params_from_iter(chunk), |r| {
            Ok((r.get::<_, String>(0)?, SavedSize {
                path: r.get(1)?,
                bytes: r.get::<_, i64>(2)? as u64,
                files: r.get::<_, Option<i64>>(3)?.map(|f| f as u64),
                partial: r.get::<_, i64>(4)? != 0,
                counted_at: r.get(5)?,
                stale: r.get::<_, i64>(6)? != 0,
            }))
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

/// The outermost folders inside `key` with a saved size counted since `since` (Unix
/// milliseconds) and not out of date: looked up a level at a time, going further down only
/// below folders whose own size is out of date or older.
pub fn recent_folder_sizes_inside(conn: &Connection, key: &str, since: i64) -> rusqlite::Result<Vec<(String, SavedSize)>> {
    // Keys inside `base` sort between "base\" and "base]" ("]" follows "\" in ASCII); those
    // right inside have no "\" after that.
    let mut st = conn.prepare_cached(
        "SELECT path_key, path, bytes, files, partial, counted_at, stale FROM folder_sizes
         WHERE path_key > ?1 AND path_key < ?2 AND instr(substr(path_key, ?3), '\\') = 0",
    )?;
    let mut out = Vec::new();
    let mut open = vec![key.trim_end_matches('\\').to_string()];
    while let Some(base) = open.pop() {
        let from = format!("{base}\\");
        let rest = from.chars().count() as i64 + 1;
        let rows = st.query_map(params![from, format!("{base}]"), rest], |r| {
            Ok((r.get::<_, String>(0)?, SavedSize {
                path: r.get(1)?,
                bytes: r.get::<_, i64>(2)? as u64,
                files: r.get::<_, Option<i64>>(3)?.map(|f| f as u64),
                partial: r.get::<_, i64>(4)? != 0,
                counted_at: r.get(5)?,
                stale: r.get::<_, i64>(6)? != 0,
            }))
        })?;
        for row in rows {
            let (k, s) = row?;
            if !s.stale && s.counted_at >= since {
                out.push((k, s));
            } else {
                open.push(k);
            }
        }
    }
    Ok(out)
}

pub fn put_folder_size(conn: &Connection, key: &str, s: &SavedSize) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO folder_sizes (path_key, path, bytes, files, partial, counted_at, stale)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)
         ON CONFLICT(path_key) DO UPDATE SET path = excluded.path, bytes = excluded.bytes, files = excluded.files,
             partial = excluded.partial, counted_at = excluded.counted_at, stale = 0",
        params![key, s.path, s.bytes as i64, s.files.map(|f| f as i64), s.partial as i64, s.counted_at],
    )?;
    Ok(())
}

/// Marks saved sizes out of date: these exact folders, and everything below `below`.
pub fn stale_folder_sizes(conn: &Connection, exact: &[String], below: &[String]) -> rusqlite::Result<()> {
    for k in exact {
        conn.execute("UPDATE folder_sizes SET stale = 1 WHERE path_key = ?1", [k])?;
    }
    for k in below {
        // Keys inside `k` sort between "k\" and "k]" ("]" follows "\" in ASCII).
        let base = k.trim_end_matches('\\');
        conn.execute(
            "UPDATE folder_sizes SET stale = 1 WHERE path_key > ?1 AND path_key < ?2",
            params![format!("{base}\\"), format!("{base}]")],
        )?;
    }
    Ok(())
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- kv / settings ----------

pub fn kv_get(conn: &Connection, key: &str) -> Option<Value> {
    conn.query_row("SELECT json FROM kv WHERE key = ?1", [key], |r| r.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn kv_set(conn: &Connection, key: &str, v: &Value) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO kv (key, json) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET json = excluded.json",
        params![key, v.to_string()],
    )?;
    Ok(())
}

pub fn default_view() -> Value {
    json!({
        "mode": "list", "sortBy": "name", "sortDir": "asc", "groupBy": "none",
        "foldersFirst": true, "showHidden": false, "iconSize": 96, "previewOpen": false
    })
}

pub fn default_settings() -> Value {
    json!({
        "theme": "dark",
        "accent": "#e2457f",
        "backdrop": "mica",
        "density": "comfortable",
        "doubleClickOpen": true,
        "rememberPerFolder": true,
        "inheritToSubfolders": false,
        "restoreSession": true,
        "hiddenPlaces": [],
        "hiddenSections": [],
        // Ctrl+K: queries used recently (newest first) and the ones saved to the sidebar.
        "searchHistory": [],
        "savedSearches": [],
        "globalView": default_view(),
        "profiles": {
            "media":     { "enabled": true, "view": { "mode": "grid", "sortBy": "modified", "sortDir": "desc", "groupBy": "date" } },
            "code":      { "enabled": true, "view": { "mode": "list", "sortBy": "name", "sortDir": "asc", "groupBy": "none", "foldersFirst": true, "showHidden": true } },
            "downloads": { "enabled": true, "view": { "mode": "list", "sortBy": "modified", "sortDir": "desc", "groupBy": "date" } }
        }
    })
}

pub fn merge_deep(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(bv) if bv.is_object() && v.is_object() => merge_deep(bv, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

pub fn load_settings(conn: &Connection) -> Value {
    let mut s = default_settings();
    if let Some(saved) = kv_get(conn, "settings") {
        merge_deep(&mut s, &saved);
    }
    s
}

// ---------- remembered views ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedView {
    pub view: Value,
    /// "folder" | "inherited" | "profile" | "global"
    pub source: &'static str,
    pub source_path: Option<String>,
    pub profile: Option<&'static str>,
}

/// folder override → ancestor override (recursive, or any if inheritance is on)
/// → folder-type profile → global default.
pub fn resolve_view(conn: &Connection, settings: &Value, path: &str, profile: Option<&'static str>) -> ResolvedView {
    let mut view = settings["globalView"].clone();
    let get = |k: &str| -> Option<(String, Value, bool)> {
        conn.query_row(
            "SELECT path, json, recursive FROM view_settings WHERE path_key = ?1",
            [k],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0)),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|(p, j, rec)| serde_json::from_str(&j).ok().map(|v| (p, v, rec)))
    };

    if let Some((p, v, _)) = get(&paths::key(path)) {
        merge_deep(&mut view, &v);
        return ResolvedView { view, source: "folder", source_path: Some(p), profile };
    }

    let inherit_all = settings["inheritToSubfolders"].as_bool().unwrap_or(false);
    let mut cur = paths::parent(path);
    while let Some(p) = cur {
        if let Some((sp, v, rec)) = get(&paths::key(&p)) {
            if rec || inherit_all {
                merge_deep(&mut view, &v);
                return ResolvedView { view, source: "inherited", source_path: Some(sp), profile };
            }
        }
        cur = paths::parent(&p);
    }

    if let Some(name) = profile {
        let prof = &settings["profiles"][name];
        if prof["enabled"].as_bool().unwrap_or(false) {
            merge_deep(&mut view, &prof["view"]);
            return ResolvedView { view, source: "profile", source_path: None, profile };
        }
    }
    ResolvedView { view, source: "global", source_path: None, profile }
}

pub fn set_folder_view(conn: &Connection, path: &str, view: &Value, recursive: bool) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO view_settings (path_key, path, json, recursive, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(path_key) DO UPDATE SET path = excluded.path, json = excluded.json,
             recursive = excluded.recursive, updated_at = excluded.updated_at",
        params![paths::key(path), paths::normalize(path), view.to_string(), recursive as i64, now()],
    )?;
    Ok(())
}

pub fn clear_folder_view(conn: &Connection, path: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM view_settings WHERE path_key = ?1", [paths::key(path)])?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderView {
    path: String,
    view: Value,
    recursive: bool,
    updated_at: i64,
}

pub fn list_folder_views(conn: &Connection) -> rusqlite::Result<Vec<FolderView>> {
    let mut st = conn.prepare("SELECT path, json, recursive, updated_at FROM view_settings ORDER BY updated_at DESC")?;
    let rows = st.query_map([], |r| {
        Ok(FolderView {
            path: r.get(0)?,
            view: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(Value::Null),
            recursive: r.get::<_, i64>(2)? != 0,
            updated_at: r.get(3)?,
        })
    })?;
    rows.collect()
}

// ---------- favorites ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Favorite {
    path: String,
    name: String,
    is_dir: bool,
    color: Option<String>,
}

pub fn favorites(conn: &Connection) -> rusqlite::Result<Vec<Favorite>> {
    let mut st = conn.prepare("SELECT path, name, is_dir, color FROM favorites ORDER BY position")?;
    let rows = st.query_map([], |r| {
        Ok(Favorite { path: r.get(0)?, name: r.get(1)?, is_dir: r.get::<_, i64>(2)? != 0, color: r.get(3)? })
    })?;
    rows.collect()
}

pub fn add_favorite(conn: &Connection, path: &str, name: &str, is_dir: bool) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO favorites (path_key, path, name, is_dir, color, position)
         VALUES (?1, ?2, ?3, ?4, NULL, (SELECT COALESCE(MAX(position), 0) + 1 FROM favorites))",
        params![paths::key(path), paths::normalize(path), name, is_dir as i64],
    )?;
    Ok(())
}

pub fn remove_favorite(conn: &Connection, path: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM favorites WHERE path_key = ?1", [paths::key(path)])?;
    Ok(())
}

pub fn update_favorite(conn: &Connection, path: &str, name: &str, color: Option<&str>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE favorites SET name = ?2, color = ?3 WHERE path_key = ?1",
        params![paths::key(path), name, color],
    )?;
    Ok(())
}

pub fn reorder_favorites(conn: &mut Connection, ordered: &[String]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    for (i, p) in ordered.iter().enumerate() {
        tx.execute("UPDATE favorites SET position = ?2 WHERE path_key = ?1", params![paths::key(p), i as i64])?;
    }
    tx.commit()
}

// ---------- recents ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recent {
    path: String,
    last_opened: i64,
    count: i64,
}

/// Folders you use most come first: visits weighted by how recent they are (like Explorer's
/// frequent folders), so the list stays steady instead of reshuffling on every visit.
pub fn recents(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<Recent>> {
    let mut st = conn.prepare("SELECT path, last_opened, count FROM recents")?;
    let rows = st.query_map([], |r| Ok(Recent { path: r.get(0)?, last_opened: r.get(1)?, count: r.get(2)? }))?;
    let mut all = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let now = now();
    all.sort_by(|a, b| frecency(b, now).total_cmp(&frecency(a, now)).then(b.last_opened.cmp(&a.last_opened)));
    all.truncate(limit.max(0) as usize);
    Ok(all)
}

/// `recents` without folders that were deleted or moved away: on a drive that's there, a folder
/// that isn't. Their rows stay (an unplugged drive's folders come back with it); network and WSL
/// folders aren't checked (slow, and looking would start a stopped distro).
pub fn recents_present(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<Recent>> {
    let gone = |p: &str| match p.get(..3) {
        Some(root) if root.as_bytes()[1] == b':' && root.ends_with('\\') => {
            std::path::Path::new(root).exists() && !std::path::Path::new(&paths::for_io(p)).exists()
        }
        _ => false,
    };
    let mut all = recents(conn, 200)?;
    all.retain(|r| !gone(&r.path));
    all.truncate(limit.max(0) as usize);
    Ok(all)
}

fn frecency(r: &Recent, now: i64) -> f64 {
    const HOUR: i64 = 3_600_000;
    let age = (now - r.last_opened).max(0);
    let weight = match age {
        a if a < HOUR => 4.0,
        a if a < 24 * HOUR => 2.0,
        a if a < 7 * 24 * HOUR => 0.5,
        _ => 0.25,
    };
    r.count as f64 * weight
}

pub fn push_recent(conn: &Connection, path: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO recents (path_key, path, last_opened, count) VALUES (?1, ?2, ?3, 1)
         ON CONFLICT(path_key) DO UPDATE SET path = excluded.path,
             last_opened = excluded.last_opened, count = count + 1",
        params![paths::key(path), paths::normalize(path), now()],
    )?;
    // Keep the table small.
    conn.execute(
        "DELETE FROM recents WHERE path_key NOT IN
         (SELECT path_key FROM recents ORDER BY last_opened DESC LIMIT 200)",
        [],
    )?;
    Ok(())
}

pub fn remove_recent(conn: &Connection, path: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM recents WHERE path_key = ?1", [paths::key(path)])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recents_rank_by_use_and_recency() {
        let file = std::env::temp_dir().join(format!("ferret-db-{}.db", std::process::id()));
        let conn = open(&file).unwrap();
        let day = 24 * 3_600_000;
        let add = |path: &str, age: i64, count: i64| {
            conn.execute(
                "INSERT INTO recents (path_key, path, last_opened, count) VALUES (?1, ?1, ?2, ?3)",
                params![path, now() - age, count],
            )
            .unwrap();
        };
        add(r"c:\passed-through-just-now", 60_000, 1); // 1 × 4
        add(r"c:\work", 2 * day, 20); // 20 × 0.5
        add(r"c:\yesterday", day / 2, 3); // 3 × 2
        add(r"c:\old-project", 30 * day, 8); // 8 × 0.25
        let order: Vec<String> = recents(&conn, 10).unwrap().into_iter().map(|r| r.path).collect();
        assert_eq!(order, [r"c:\work", r"c:\yesterday", r"c:\passed-through-just-now", r"c:\old-project"]);
        assert_eq!(recents(&conn, 2).unwrap().len(), 2);
        drop(conn);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", file.display()));
        }
    }

    #[test]
    fn folder_sizes_are_saved_and_go_stale() {
        let file = std::env::temp_dir().join(format!("ferret-sizes-{}.db", std::process::id()));
        let conn = open(&file).unwrap();
        let put = |key: &str, bytes: u64| {
            let s = SavedSize { path: key.into(), bytes, files: Some(1), partial: false, counted_at: now(), stale: false };
            put_folder_size(&conn, key, &s).unwrap();
        };
        for (k, b) in [(r"c:\", 1), (r"c:\a", 2), (r"c:\a\b", 3), (r"c:\a\b\c", 4), (r"c:\ab", 5), (r"d:\x", 6)] {
            put(k, b);
        }
        put(r"c:\a", 20); // counted again
        let stale = |keys: &[&str]| -> Vec<(String, u64, bool)> {
            let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
            let mut v: Vec<_> = folder_sizes(&conn, &keys).unwrap().into_iter().map(|(k, s)| (k, s.bytes, s.stale)).collect();
            v.sort();
            v
        };
        let all = [r"c:\", r"c:\a", r"c:\a\b", r"c:\a\b\c", r"c:\ab", r"d:\x", r"e:\never"];
        assert_eq!(stale(&all).len(), 6);
        assert_eq!(stale(&[r"c:\a"]), [(r"c:\a".to_string(), 20, false)]);

        // A change in c:\a\b: it and the folders containing it; with `deep`, everything inside too.
        stale_folder_sizes(&conn, &[r"c:\a\b".into(), r"c:\a".into(), r"c:\".into()], &[r"c:\a\b".into()]).unwrap();
        let marked: Vec<String> = stale(&all).into_iter().filter(|s| s.2).map(|s| s.0).collect();
        assert_eq!(marked, [r"c:\", r"c:\a", r"c:\a\b", r"c:\a\b\c"]);
        put(r"c:\a", 21);
        assert!(!stale(&[r"c:\a"])[0].2);

        // The outermost recent ones inside a folder that aren't out of date (c:\ab isn't inside c:\a).
        let inside = |key: &str, since: i64| -> Vec<String> {
            let mut v: Vec<String> = recent_folder_sizes_inside(&conn, key, since).unwrap().into_iter().map(|(k, _)| k).collect();
            v.sort();
            v
        };
        assert!(inside(r"c:\a", 0).is_empty());
        put(r"c:\a\b", 3);
        assert_eq!(inside(r"c:\a", 0), [r"c:\a\b"]);
        assert_eq!(inside(r"c:\", 0), [r"c:\a", r"c:\ab"]);
        assert!(inside(r"c:\a", now() + 60_000).is_empty());
        // Too old: the ones inside it are looked at instead.
        let old = SavedSize { path: r"c:\a".into(), bytes: 1, files: Some(1), partial: false, counted_at: 1, stale: false };
        put_folder_size(&conn, r"c:\a", &old).unwrap();
        assert_eq!(inside(r"c:\", now() - 60_000), [r"c:\a\b", r"c:\ab"]);
        drop(conn);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", file.display()));
        }
    }
}
