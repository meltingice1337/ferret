//! Volumes + query orchestration shared by the app (walk provider) and the
//! indexer service (MFT provider).

use super::index::{lower, FastMap, Index};
use super::query::{self, QueryCache, RunOpts};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Instant;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    /// Unix milliseconds.
    pub modified: Option<i64>,
    pub score: f32,
    /// UTF-16 offsets into `name` to highlight.
    pub matches: Vec<u32>,
    /// Searches inside files (`content:`): where the text is (content.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<LineMatch>,
}

/// The first line of a file that has the text searched for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LineMatch {
    /// 1-based.
    pub line: u32,
    /// The line, or the part of it around the match ("…" where it was cut).
    pub text: String,
    /// UTF-16 offsets of the match in `text`.
    pub start: u32,
    pub end: u32,
    /// Matches in the whole file (counting stops at a few thousand).
    pub count: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub results: Vec<Hit>,
    /// Matches before `limit` was applied.
    pub total: u64,
    pub took_us: u64,
    /// "service" | "walk" | "content" | "none"
    pub source: String,
    /// The index is still being built (results may be incomplete).
    pub partial: bool,
    /// Searches inside files: they stop at `limit` files, so there may be more than `total`.
    #[serde(default)]
    pub more: bool,
    /// Searches inside files: how many files were read.
    #[serde(default)]
    pub scanned: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub query: String,
    pub limit: u32,
    pub scope: Option<String>,
    pub frecency: Option<HashMap<String, f32>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VolumeStatus {
    pub root: String,
    pub entries: u64,
    pub ready: bool,
    pub progress: f32,
}

pub struct Volume {
    pub root: String,
    pub index: RwLock<Index>,
    /// MFT volumes: every folder's size (sizes.rs), once the file table has been read.
    pub sizes: RwLock<Option<super::sizes::Tree>>,
    pub ready: AtomicBool,
    progress: AtomicU32,
    /// Live entry count published by builders (readable without the lock).
    entries: AtomicU64,
}

impl Volume {
    pub fn new(index: Index, ready: bool) -> Arc<Self> {
        let root = index.root_path.clone();
        let n = index.live() as u64;
        Arc::new(Volume {
            root,
            index: RwLock::new(index),
            sizes: RwLock::new(None),
            ready: AtomicBool::new(ready),
            progress: AtomicU32::new(if ready { 1f32.to_bits() } else { 0 }),
            entries: AtomicU64::new(n),
        })
    }

    pub fn set_progress(&self, p: f32) {
        self.progress.store(p.clamp(0.0, 1.0).to_bits(), Relaxed);
    }

    pub fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Relaxed))
    }

    pub fn set_entries(&self, n: u64) {
        self.entries.store(n, Relaxed);
    }

    pub fn status(&self) -> VolumeStatus {
        // Avoid blocking on a writer: fall back to the published count.
        let entries = match self.index.try_read() {
            Some(ix) => {
                let n = ix.live() as u64;
                self.entries.store(n, Relaxed);
                n
            }
            None => self.entries.load(Relaxed),
        };
        let ready = self.ready.load(Relaxed);
        VolumeStatus { root: self.root.clone(), entries, ready, progress: if ready { 1.0 } else { self.progress() } }
    }
}

#[derive(Default)]
pub struct Engine {
    volumes: RwLock<Vec<Arc<Volume>>>,
    cache: Mutex<Option<Arc<QueryCache>>>,
}

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 5_000;

impl Engine {
    pub fn new() -> Arc<Self> {
        Arc::new(Engine::default())
    }

    pub fn volumes(&self) -> Vec<Arc<Volume>> {
        self.volumes.read().clone()
    }

    pub fn set_volumes(&self, v: Vec<Arc<Volume>>) {
        *self.volumes.write() = v;
        *self.cache.lock() = None;
    }

    pub fn add_volume(&self, v: Arc<Volume>) {
        let mut vols = self.volumes.write();
        vols.retain(|x| !x.root.eq_ignore_ascii_case(&v.root));
        vols.push(v);
        *self.cache.lock() = None;
    }

    pub fn clear(&self) {
        self.set_volumes(Vec::new());
    }

    pub fn clear_cache(&self) {
        *self.cache.lock() = None;
    }

    pub fn status(&self) -> Vec<VolumeStatus> {
        self.volumes().iter().map(|v| v.status()).collect()
    }

    pub fn building(&self) -> bool {
        self.volumes().iter().any(|v| !v.ready.load(Relaxed))
    }

    pub fn search(&self, req: &SearchRequest) -> SearchResults {
        let t0 = Instant::now();
        let mut q = query::parse(&req.query);
        if let Some(s) = req.scope.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            q.scopes.push(crate::paths::normalize(s));
        }
        // Index paths are long names; `in:C:\Users\MELTIN~1` must still resolve.
        for s in q.scopes.iter_mut() {
            *s = super::walk::long_path(s);
        }
        let limit = if req.limit == 0 { DEFAULT_LIMIT } else { req.limit.min(MAX_LIMIT) } as usize;
        let vols = self.volumes();
        let partial = vols.iter().any(|v| !v.ready.load(Relaxed));
        if q.is_empty() || vols.is_empty() {
            return SearchResults { took_us: t0.elapsed().as_micros() as u64, partial, ..Default::default() };
        }
        let guards: Vec<_> = vols.iter().map(|v| v.index.read()).collect();
        let ixs: Vec<&Index> = guards.iter().map(|g| &**g).collect();
        let frec = resolve_frecency(&ixs, req.frecency.as_ref());
        let ids: Vec<(usize, u64)> =
            vols.iter().zip(&ixs).map(|(v, ix)| (Arc::as_ptr(v) as usize, ix.generation)).collect();
        let cached = self.cache.lock().clone().filter(|c| c.usable_for(&q, &ids));
        let out = query::run(
            &ixs,
            &q,
            &RunOpts { limit, frecency: &frec, fuzzy: true },
            cached.as_ref().map(|c| c.cands.as_slice()),
        );
        if let Some(cands) = out.cands {
            *self.cache.lock() = Some(Arc::new(QueryCache { query: q.clone(), vols: ids, cands: Arc::new(cands) }));
        }
        let results = out
            .hits
            .iter()
            .map(|h| {
                let ix = ixs[h.vol];
                let (size, mtime) = ix.meta(h.idx);
                let is_dir = ix.is_dir(h.idx);
                Hit {
                    path: ix.path(h.idx),
                    name: ix.name(h.idx).to_string(),
                    is_dir,
                    size: if is_dir { None } else { size },
                    modified: mtime.map(|m| m as i64 * 1000),
                    score: h.score as f32,
                    matches: query::highlight(ix, h.idx, &q, h.fuzzy),
                    line: None,
                }
            })
            .collect();
        SearchResults {
            results,
            total: out.total,
            took_us: t0.elapsed().as_micros() as u64,
            source: String::new(),
            partial: partial || out.truncated,
            ..Default::default()
        }
    }
}

/// Maps caller-supplied `path → score` to per-volume `entry → boost`.
fn resolve_frecency(ixs: &[&Index], frec: Option<&HashMap<String, f32>>) -> Vec<FastMap<u32, i32>> {
    let mut out = vec![FastMap::default(); ixs.len()];
    let Some(frec) = frec else { return out };
    let roots: Vec<Vec<u8>> = ixs.iter().map(|ix| ix.root_key()).collect();
    for (path, &score) in frec {
        if !(score > 0.0) {
            continue;
        }
        let path = &super::walk::long_path(&crate::paths::normalize(path));
        let key = lower(path);
        // Longest matching root wins (walk roots may nest under drive roots).
        let Some(v) = roots
            .iter()
            .enumerate()
            .filter(|(_, r)| key.starts_with(r) && (key.len() == r.len() || key[r.len()] == b'\\'))
            .max_by_key(|(_, r)| r.len())
            .map(|(i, _)| i)
        else {
            continue;
        };
        if let Some(idx) = ixs[v].resolve(path) {
            let boost = (900.0 * (1.0 + score).ln()).min(4_000.0) as i32;
            out[v].insert(idx, boost);
        }
    }
    out
}
