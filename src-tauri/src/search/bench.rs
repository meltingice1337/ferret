//! Synthetic 3M-entry benchmark.
//!
//! `cargo test --release --lib search::bench -- --ignored --nocapture`

use super::engine::{Engine, SearchRequest, Volume};
use super::index::{flags::*, Index};
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[self.below(v.len())]
    }
}

const WORDS: &[&str] = &[
    "src", "lib", "test", "tests", "index", "main", "util", "utils", "config", "app", "build", "dist", "assets",
    "images", "docs", "report", "invoice", "photo", "music", "video", "project", "backup", "data", "temp", "cache",
    "logs", "user", "admin", "server", "client", "api", "core", "common", "shared", "module", "component",
    "service", "model", "view", "controller", "helper", "style", "script", "font", "icon", "license", "changelog",
    "vendor", "plugin", "theme", "layout", "page", "public", "private", "local", "global", "system", "windows",
    "program", "files", "microsoft", "google", "chrome", "firefox", "steam", "games", "download", "desktop",
    "document", "picture", "summer", "holiday", "family", "work", "school", "budget", "tax", "resume", "letter",
    "contract", "meeting", "notes", "draft", "final", "old", "new", "copy", "version", "release", "debug",
    "resources", "locale", "fonts", "runtime", "framework", "driver", "update", "setup", "install", "package",
    "manifest", "settings", "profile", "session", "history", "bookmark", "extension", "worker", "handler",
    "parser", "lexer", "render", "engine", "network", "socket", "stream", "buffer", "queue", "event", "store",
    "router", "schema", "migration", "fixture", "mock", "spec", "bench", "example", "sample", "template",
    "widget", "button", "dialog", "window", "panel", "editor", "viewer", "player", "camera", "screen", "audio",
    "facebook", "factory", "facade", "surface", "interface", "nodejs", "anode", "diode", "episode", "unicode",
];
const PKGS: &[&str] = &[
    "lodash", "react", "react-dom", "express", "webpack", "babel-core", "typescript", "eslint", "chalk",
    "commander", "node-hid", "debug", "ms", "semver", "glob", "minimatch", "rimraf", "mkdirp", "uuid", "axios", "moment",
    "node-fetch", "node-gyp", "nodemon", "@types", "core-js", "tslib", "rxjs", "vue", "jest", "mocha", "yargs",
];
const EXTS: &[&str] = &[
    "js", "js", "js", "ts", "json", "md", "txt", "dll", "dll", "exe", "png", "png", "jpg", "pdf", "docx", "xlsx",
    "mp3", "mp4", "h", "c", "cpp", "rs", "py", "html", "css", "xml", "log", "dat", "bin", "sys", "ini", "cfg",
    "manifest", "mui", "cat", "zip", "map", "d.ts", "svg", "gif", "wav", "lnk", "tmp", "db", "yml",
];

fn camel(r: &mut Rng, n: usize) -> String {
    (0..n)
        .map(|_| {
            let w = r.pick(WORDS);
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect()
}

fn file_name(r: &mut Rng) -> String {
    let ext = r.pick(EXTS);
    match r.below(100) {
        0..=2 => "package.json".into(),
        3 => "index.js".into(),
        4 => "index.ts".into(),
        5 => "README.md".into(),
        6 => ".gitignore".into(),
        7..=11 => format!("IMG_{:04}.JPG", r.below(10_000)),
        12..=14 => format!("{}-{}.{ext}", r.pick(WORDS), r.pick(WORDS)),
        15..=29 => {
            let n = 1 + r.below(3);
            format!("{}.{ext}", camel(r, n))
        }
        30..=44 => format!("{}_{}.{ext}", r.pick(WORDS), r.pick(WORDS)),
        45..=52 => format!("{:x}.{ext}", r.next() & 0xffff_ffff_ffff),
        53..=58 => format!(
            "amd64_microsoft-windows-{}-{}_31bf3856ad364e35_10.0.{}.{}_none_{:x}.manifest",
            r.pick(WORDS),
            r.pick(WORDS),
            19041 + r.below(3000),
            r.below(5000),
            r.next() & 0xffff_ffff
        ),
        59..=63 => format!("{} {}-{:02}-{:02}.{ext}", r.pick(WORDS), 2015 + r.below(12), 1 + r.below(12), 1 + r.below(28)),
        64..=66 => format!("Invoice {} {}.pdf", r.below(9999), r.pick(WORDS)),
        67..=70 => format!("{}.{}.{}.{ext}", r.pick(WORDS), r.below(20), r.below(100)),
        71 => "node.exe".into(),
        72..=73 => format!("{}.min.js", r.pick(PKGS).trim_start_matches('@')),
        _ => format!("{}.{ext}", r.pick(WORDS)),
    }
}

fn dir_name(r: &mut Rng) -> String {
    match r.below(100) {
        0..=5 => "node_modules".into(),
        6..=14 => r.pick(PKGS).into(),
        15..=17 => "src".into(),
        18 => "node".into(),
        19..=24 => format!("{{{:08x}-{:04x}-{:04x}}}", r.next() as u32, r.next() as u16, r.next() as u16),
        25..=34 => {
            let n = 1 + r.below(2);
            camel(r, n)
        }
        35..=40 => format!("{}-{}.{}.{}", r.pick(PKGS), r.below(10), r.below(30), r.below(10)),
        _ => r.pick(WORDS).into(),
    }
}

/// `n` entries, ~12% folders, up to 15 levels deep, recent-folder locality.
pub(crate) fn synth(root: &str, n: usize, seed: u64) -> Index {
    let mut r = Rng(seed | 1);
    let mut ix = Index::with_capacity(root, true, n + 1, n * 30);
    let mut dirs: Vec<(u32, u8)> = vec![(0, 0)];
    for _ in 0..n {
        let (p, d) = if r.below(10) < 8 {
            let k = dirs.len().min(48);
            dirs[dirs.len() - 1 - r.below(k)]
        } else {
            dirs[r.below(dirs.len())]
        };
        let is_dir = d < 15 && r.below(100) < 12;
        let fl = if is_dir { DIR } else { 0 } | if r.below(50) == 0 { HIDDEN } else { 0 };
        let name = if is_dir { dir_name(&mut r) } else { file_name(&mut r) };
        let idx = ix.insert(p, &name, fl, r.below(50_000_000) as u64, 1_500_000_000 + r.below(300_000_000) as u32);
        if is_dir {
            dirs.push((idx, d + 1));
        }
    }
    ix.shrink_to_fit();
    ix
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

#[test]
#[ignore]
fn bench_3m() {
    let t = Instant::now();
    // Two "drives": 2.4M + 0.6M entries.
    let c = synth(r"C:\", 2_400_000, 0x9E37_79B9_7F4A_7C15);
    let d = synth(r"D:\", 600_000, 0xD1B5_4A32_D192_ED03);
    let build = t.elapsed();
    let entries = c.live() + d.live();
    let bytes = c.heap_bytes() + d.heap_bytes();
    let max_depth = (0..c.slots() as u32).map(|i| c.depth(i)).max().unwrap();
    println!(
        "built {entries} entries in {:.2}s; heap {:.1} MB = {:.1} B/entry; scan buffer {:.1} MB; max depth {max_depth}; threads {}",
        build.as_secs_f64(),
        bytes as f64 / 1e6,
        bytes as f64 / entries as f64,
        (c.scan_bytes() + d.scan_bytes()) as f64 / 1e6,
        rayon::current_num_threads()
    );
    let eng = Engine::new();
    eng.add_volume(Volume::new(c, true));
    eng.add_volume(Volume::new(d, true));
    let req = |q: &str| SearchRequest { query: q.into(), limit: 50, ..Default::default() };

    let queries = [
        "node", "nod", "package.json", "readme md", ".gitignore", "fac", "zqxj", "n", "invoice 2024", "ext:pdf invoice",
        "ext:pdf", "type:folder node", "*.min.js", r"node_modules\lodash", "size:>40mb ext:mp4", "modified:<30d fac",
        "node-hid package.json", r"package.json !node_modules\", "src index.ts", r"readme !C:\Windows", "lodash index",
    ];
    println!("\n| query | total | top hit | p50 ms | p95 ms |");
    println!("|---|---:|---|---:|---:|");
    for q in queries {
        let mut v = Vec::new();
        let mut last = None;
        for i in 0..43 {
            eng.clear_cache();
            let t = Instant::now();
            let r = eng.search(&req(q));
            let ms = t.elapsed().as_secs_f64() * 1e3;
            if i >= 3 {
                v.push(ms);
            }
            last = Some(r);
        }
        let r = last.unwrap();
        let top = r.results.first().map(|h| h.name.clone()).unwrap_or_default();
        println!("| `{q}` | {} | {} | {:.2} | {:.2} |", r.total, top, pct(&mut v, 0.5), pct(&mut v, 0.95));
    }

    // Typing sequence with incremental refinement (cache cleared per sequence).
    let seq = ["n", "no", "nod", "node", "node_", "node_m"];
    let mut per: Vec<Vec<f64>> = vec![Vec::new(); seq.len()];
    for round in 0..33 {
        eng.clear_cache();
        for (i, q) in seq.iter().enumerate() {
            let t = Instant::now();
            let _ = eng.search(&req(q));
            if round >= 3 {
                per[i].push(t.elapsed().as_secs_f64() * 1e3);
            }
        }
    }
    println!("\n| typing step | p50 ms | p95 ms |");
    println!("|---|---:|---:|");
    for (i, q) in seq.iter().enumerate() {
        println!("| `{q}` | {:.2} | {:.2} |", pct(&mut per[i], 0.5), pct(&mut per[i], 0.95));
    }
}
