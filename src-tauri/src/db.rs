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
             last_opened INTEGER NOT NULL, count INTEGER NOT NULL);",
    )?;
    Ok(conn)
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
}
