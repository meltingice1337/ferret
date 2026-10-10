//! Custom URI schemes that hand file bytes to the webview:
//! - `fs://localhost/<pct-encoded path>`: raw file with HTTP Range support (video
//!   seeking, PDFs, images). On Windows WebView2 this is `http://fs.localhost/…`.
//! - `thumb://localhost/<pct-encoded path>?s=256&icon=0`: shell thumbnail/icon PNG,
//!   cached on disk by path + size + mtime.

use percent_encoding::percent_decode_str;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;
use tauri::http::{header, Request, Response, StatusCode};

use crate::paths;

const MAX_CHUNK: u64 = 4 * 1024 * 1024;
const MAX_WHOLE: u64 = 64 * 1024 * 1024;

fn decode_path(req: &Request<Vec<u8>>) -> String {
    let raw = req.uri().path().trim_start_matches('/');
    paths::normalize(&percent_decode_str(raw).decode_utf8_lossy())
}

fn query(req: &Request<Vec<u8>>, key: &str) -> Option<String> {
    req.uri().query()?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| percent_decode_str(v).decode_utf8_lossy().into_owned())
    })
}

fn status(code: StatusCode, msg: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(code)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(msg.as_bytes().to_vec())
        .unwrap()
}

fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    let spec = h.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (len.saturating_sub(n), len.saturating_sub(1))
    } else {
        let s: u64 = a.parse().ok()?;
        let e = if b.is_empty() { len.saturating_sub(1) } else { b.parse::<u64>().ok()?.min(len.saturating_sub(1)) };
        (s, e)
    };
    (start <= end && start < len).then_some((start, end))
}

pub fn serve_file(req: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = decode_path(&req);
    let io = PathBuf::from(paths::for_io(&path));
    let Ok(mut file) = fs::File::open(&io) else {
        return status(StatusCode::NOT_FOUND, "not found");
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mime = mime_guess::from_path(&path).first_or_octet_stream().essence_str().to_string();
    let base = || {
        Response::builder()
            .header(header::CONTENT_TYPE, &mime)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(header::CACHE_CONTROL, "no-cache")
    };

    let range = req.headers().get(header::RANGE).and_then(|h| h.to_str().ok()).map(str::to_string);
    let (start, end, partial) = match range.as_deref().map(|r| parse_range(r, len)) {
        Some(Some((s, e))) => (s, e.min(s + MAX_CHUNK - 1), true),
        Some(None) => {
            return base()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(vec![])
                .unwrap()
        }
        // No Range header: whole file if reasonable, otherwise the first chunk as a 206.
        None if len <= MAX_WHOLE => (0, len.saturating_sub(1), false),
        None => (0, MAX_CHUNK - 1, true),
    };

    let mut buf = Vec::with_capacity((end + 1 - start) as usize);
    if len > 0 {
        if file.seek(SeekFrom::Start(start)).is_err() || (&mut file).take(end + 1 - start).read_to_end(&mut buf).is_err() {
            return status(StatusCode::INTERNAL_SERVER_ERROR, "read failed");
        }
    }
    let mut res = base().header(header::CONTENT_LENGTH, buf.len());
    if partial {
        res = res.status(StatusCode::PARTIAL_CONTENT).header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    } else {
        res = res.status(StatusCode::OK);
    }
    res.body(buf).unwrap()
}

fn thumb_cache_dir() -> Option<PathBuf> {
    let d = dirs::cache_dir()?.join("Ferret").join("thumbs");
    fs::create_dir_all(&d).ok()?;
    Some(d)
}

fn hash(s: &str) -> u64 {
    // FNV-1a: stable across runs (unlike DefaultHasher's random keys).
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn serve_thumb(req: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = decode_path(&req);
    let size: u32 = query(&req, "s").and_then(|s| s.parse().ok()).unwrap_or(256).clamp(16, 1024);
    let icon_only = query(&req, "icon").as_deref() == Some("1");
    let io = PathBuf::from(paths::for_io(&path));
    let meta = fs::metadata(&io).ok();
    let stamp = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = format!("{}|{}|{}|{}|{}", paths::key(&path), size, icon_only, stamp, meta.map(|m| m.len()).unwrap_or(0));
    let cache = thumb_cache_dir().map(|d| d.join(format!("{:016x}.png", hash(&key))));

    let png = |bytes: Vec<u8>| {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(header::CACHE_CONTROL, "max-age=3600")
            .body(bytes)
            .unwrap()
    };
    if let Some(c) = &cache {
        if let Ok(bytes) = fs::read(c) {
            return png(bytes);
        }
    }
    match crate::shell::thumbnail_png(&path, size, icon_only) {
        Ok(bytes) => {
            if let Some(c) = &cache {
                let _ = fs::write(c, &bytes);
            }
            png(bytes)
        }
        // No thumbnail: a transparent 1×1 PNG (the UI falls back to an icon) instead
        // of a 404, which would log a console error for every icon-only file.
        Err(_) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header("X-Ferret-Thumb", "none")
            .body(EMPTY_PNG.to_vec())
            .unwrap(),
    }
}

/// 1×1 fully transparent PNG.
const EMPTY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01,
    0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41,
    0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
    0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=50-500", 100), Some((50, 99)));
        assert_eq!(parse_range("bytes=200-", 100), None);
    }
}

/// Tiny counting semaphore limiting concurrent thumbnail extractions.
pub struct Slots {
    used: std::sync::Mutex<usize>,
    cv: std::sync::Condvar,
    max: usize,
}

pub struct Permit(&'static Slots);

impl Slots {
    pub fn acquire(&'static self) -> Permit {
        let mut used = self.used.lock().unwrap();
        while *used >= self.max {
            used = self.cv.wait(used).unwrap();
        }
        *used += 1;
        Permit(self)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        *self.0.used.lock().unwrap() -= 1;
        self.0.cv.notify_one();
    }
}

pub static THUMB_SLOTS: Slots = Slots { used: std::sync::Mutex::new(0), cv: std::sync::Condvar::new(), max: 6 };
