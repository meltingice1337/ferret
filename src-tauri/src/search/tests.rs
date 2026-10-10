//! Engine-level tests: ranking, refinement, filters, live updates.

use super::engine::{Engine, SearchRequest, Volume};
use super::index::{flags::*, Index};
use super::query::{self, parse_at, run, RunOpts};
use std::collections::HashMap;

/// Builds an index from `\`-separated relative paths (trailing `\` = folder).
pub(crate) fn build(root: &str, paths: &[&str]) -> Index {
    let mut ix = Index::new(root, true);
    for p in paths {
        let dir = p.ends_with('\\');
        let parts: Vec<&str> = p.trim_end_matches('\\').split('\\').collect();
        let mut cur = 0u32;
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let lname = super::index::lower(part);
            cur = match ix.child(cur, &lname) {
                Some(c) => c,
                None => {
                    let fl = if !last || dir { DIR } else { 0 };
                    ix.insert(cur, part, fl, if fl == 0 { 1000 } else { 0 }, 1_700_000_000)
                }
            };
        }
    }
    ix
}

fn names(ix: &Index, q: &str) -> Vec<String> {
    let q = parse_at(q, 1_800_000_000);
    let out = run(&[ix], &q, &RunOpts { limit: 50, frecency: &[], fuzzy: true }, None);
    out.hits.iter().map(|h| ix.path(h.idx)).collect()
}

const TREE: &[&str] = &[
    r"dev\node\",
    r"dev\node.exe",
    r"dev\node_modules\left-pad\index.js",
    r"dev\anode.txt",
    r"dev\my-node-app\package.json",
    r"dev\nxoxdxe.log",
    r"docs\Invoice 2024.pdf",
    r"docs\invoice-old.docx",
    r"docs\README.md",
    r"docs\readme-notes.txt",
    r"music\cool track.mp3",
];

#[test]
fn search_ranking_order() {
    let ix = build(r"C:\", TREE);
    let r = names(&ix, "node");
    // exact (folder) > stem > prefix > word-boundary > substring > fuzzy
    assert_eq!(
        r,
        vec![
            r"C:\dev\node",
            r"C:\dev\node.exe",
            r"C:\dev\node_modules",
            r"C:\dev\my-node-app",
            r"C:\dev\anode.txt",
            r"C:\dev\nxoxdxe.log",
        ]
    );
    // Multi-token + ext
    assert_eq!(names(&ix, "readme md"), vec![r"C:\docs\README.md"]);
    assert_eq!(names(&ix, "ext:pdf invoice"), vec![r"C:\docs\Invoice 2024.pdf"]);
    assert_eq!(names(&ix, "zqxj"), Vec::<String>::new());
}

#[test]
fn search_fuzzy_only_when_scarce() {
    let ix = build(r"C:\", TREE);
    // "nde" is a substring of nothing; fuzzy finds node-ish names.
    let r = names(&ix, "nde");
    assert!(r.contains(&r"C:\dev\node".to_string()), "{r:?}");
    // plenty of substring hits → no fuzzy noise
    let r = names(&ix, "e");
    assert!(!r.is_empty());
}

#[test]
fn search_filters() {
    let ix = build(r"C:\", TREE);
    assert_eq!(names(&ix, "type:folder node"), vec![r"C:\dev\node", r"C:\dev\node_modules", r"C:\dev\my-node-app"]);
    assert!(names(&ix, "type:file node").iter().all(|p| !p.ends_with(r"\node")));
    assert_eq!(names(&ix, r"readme in:C:\docs"), vec![r"C:\docs\README.md", r"C:\docs\readme-notes.txt"]);
    assert_eq!(names(&ix, r"in:c:\dev\node_modules"), vec![r"C:\dev\node_modules\left-pad", r"C:\dev\node_modules\left-pad\index.js"]);
    assert!(names(&ix, r"node in:C:\nope").is_empty());
    let mut e = names(&ix, "ext:md,docx");
    e.sort();
    assert_eq!(e, vec![r"C:\docs\README.md", r"C:\docs\invoice-old.docx"]);
    assert_eq!(names(&ix, "readme !notes"), vec![r"C:\docs\README.md"]);
    assert_eq!(names(&ix, "*.mp3"), vec![r"C:\music\cool track.mp3"]);
    assert_eq!(names(&ix, r#""cool track""#), vec![r"C:\music\cool track.mp3"]);
    assert_eq!(names(&ix, r"docs\read"), vec![r"C:\docs\README.md", r"C:\docs\readme-notes.txt"]);
    // size / modified (index has metadata: files 1000 bytes, mtime 1.7e9)
    assert_eq!(names(&ix, "size:>1kb readme"), Vec::<String>::new());
    assert_eq!(names(&ix, "size:<1kb readme").len(), 2);
    assert_eq!(names(&ix, "modified:<7d readme").len(), 0); // now = 1.8e9
    assert_eq!(names(&ix, "modified:>7d readme").len(), 2);
    assert_eq!(names(&ix, "modified:>2023-01-01 readme").len(), 2);
    assert_eq!(names(&ix, "modified:<2023-01-01 readme").len(), 0);
}

#[test]
fn search_incremental_refinement_matches_full_scan() {
    let mut paths: Vec<String> = Vec::new();
    for i in 0..3000 {
        paths.push(format!(r"p{}\node{}\nodule-{}.js", i % 17, i % 7, i));
        paths.push(format!(r"p{}\no-{}\Nod_{}.txt", i % 13, i % 5, i));
    }
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let ix = build(r"C:\", &refs);
    let opts = RunOpts { limit: 40, frecency: &[], fuzzy: true };
    let mut cache: Option<(query::Query, Vec<Vec<u32>>)> = None;
    for qs in ["n", "no", "nod", "node", "node 1", "node 12", "nodu", "nod ext:js"] {
        let q = parse_at(qs, 0);
        let full = run(&[&ix], &q, &opts, None);
        let reuse = cache.as_ref().filter(|(cq, _)| q.refines(cq)).map(|(_, c)| c.as_slice());
        let inc = run(&[&ix], &q, &opts, reuse);
        assert_eq!(full.total, inc.total, "{qs}");
        assert_eq!(full.hits, inc.hits, "{qs}");
        if let Some(c) = inc.cands {
            cache = Some((q, c));
        }
    }
    // the engine-level cache reports reuse on the typing sequence
    let eng = Engine::new();
    eng.add_volume(Volume::new(ix, true));
    let req = |q: &str| SearchRequest { query: q.into(), limit: 10, ..Default::default() };
    let a = eng.search(&req("nod"));
    let b = eng.search(&req("node"));
    assert!(a.total >= b.total && b.total > 0);
}

#[test]
fn search_live_updates_and_frecency() {
    let ix = build(r"C:\", TREE);
    let eng = Engine::new();
    let vol = Volume::new(ix, true);
    eng.add_volume(vol.clone());
    let req = |q: &str| SearchRequest { query: q.into(), limit: 10, ..Default::default() };
    assert_eq!(eng.search(&req("left-pad")).results.len(), 1);
    {
        let mut ix = vol.index.write();
        let nm = ix.resolve(r"C:\dev\node_modules").unwrap();
        let lp = ix.resolve(r"C:\dev\node_modules\left-pad").unwrap();
        ix.rename(lp, 0, "right-pad");
        let x = ix.insert(nm, "brand-new.js", 0, 5, 1);
        assert_eq!(ix.path(x), r"C:\dev\node_modules\brand-new.js");
    }
    assert!(eng.search(&req("left-pad")).results.is_empty());
    let r = eng.search(&req("right-pad"));
    assert_eq!(r.results[0].path, r"C:\right-pad");
    assert_eq!(eng.search(&req("index.js")).results[0].path, r"C:\right-pad\index.js");
    assert_eq!(eng.search(&req("brand")).results[0].path, r"C:\dev\node_modules\brand-new.js");
    {
        let mut ix = vol.index.write();
        let d = ix.resolve(r"C:\right-pad").unwrap();
        ix.remove(d);
    }
    assert!(eng.search(&req("index.js")).results.is_empty());

    // frecency lifts a substring match above a prefix match
    let base = eng.search(&req("readme"));
    assert_eq!(base.results[0].path, r"C:\docs\README.md");
    let mut f = HashMap::new();
    f.insert("c:/docs/readme-notes.txt".to_string(), 50.0f32);
    let r = eng.search(&SearchRequest { query: "readme".into(), limit: 10, frecency: Some(f), ..Default::default() });
    assert_eq!(r.results[0].path, r"C:\docs\readme-notes.txt");

    // highlight offsets are UTF-16 indices into the name
    let r = eng.search(&req("2024"));
    assert_eq!(r.results[0].name, "Invoice 2024.pdf");
    assert_eq!(r.results[0].matches, vec![8, 9, 10, 11]);
}

#[test]
fn search_scope_param_and_multi_volume() {
    let eng = Engine::new();
    eng.add_volume(Volume::new(build(r"C:\", TREE), true));
    eng.add_volume(Volume::new(build(r"D:\", &[r"stuff\node", r"stuff\readme.md"]), false));
    let r = eng.search(&SearchRequest { query: "node".into(), limit: 50, ..Default::default() });
    assert!(r.partial);
    assert!(r.results.iter().any(|h| h.path == r"D:\stuff\node"));
    let r = eng.search(&SearchRequest { query: "readme".into(), limit: 50, scope: Some(r"D:\".into()), ..Default::default() });
    assert_eq!(r.results.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(), vec![r"D:\stuff\readme.md"]);
    assert!(eng.search(&SearchRequest { query: "".into(), limit: 50, ..Default::default() }).results.is_empty());
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn search_path_excludes() {
    let ix = build(
        r"C:\",
        &[
            r"proj\package.json",
            r"proj\node_modules\left-pad\package.json",
            r"proj\node_modules\package.json",
            r"Backup\old\package.json",
            r"Windows\System32\package.json",
            r"WindowsApps\x\package.json",
        ],
    );
    let all = [
        r"C:\Backup\old\package.json",
        r"C:\Windows\System32\package.json",
        r"C:\WindowsApps\x\package.json",
        r"C:\proj\node_modules\left-pad\package.json",
        r"C:\proj\node_modules\package.json",
        r"C:\proj\package.json",
    ];
    let without = |skip: &[&str]| sorted(all.iter().filter(|p| !skip.contains(p)).map(|p| p.to_string()).collect());
    // `!dir\` excludes everything below any such folder…
    assert_eq!(
        sorted(names(&ix, r"package.json !node_modules\")),
        without(&[r"C:\proj\node_modules\left-pad\package.json", r"C:\proj\node_modules\package.json"])
    );
    // …but not the folder itself, and `/` works like `\`.
    assert_eq!(names(&ix, r"node_modules !node_modules/"), vec![r"C:\proj\node_modules"]);
    assert_eq!(sorted(names(&ix, r"package.json !\backup\")), without(&[r"C:\Backup\old\package.json"]));
    // A drive-anchored pattern is a prefix that respects folder boundaries.
    assert_eq!(sorted(names(&ix, r"package.json !C:\Windows")), without(&[r"C:\Windows\System32\package.json"]));
    assert_eq!(sorted(names(&ix, r"package.json !c:\windows\")), without(&[r"C:\Windows\System32\package.json"]));
    assert_eq!(names(&ix, r"package.json !C:\"), Vec::<String>::new());
    // Plain `!word` still only looks at the name.
    assert_eq!(names(&ix, "package.json !modules").len(), all.len());
    assert_eq!(names(&ix, "package !json").len(), 0);

    // `!D:\` drops a whole volume.
    let eng = Engine::new();
    eng.add_volume(Volume::new(ix, true));
    eng.add_volume(Volume::new(build(r"D:\", &[r"stuff\package.json"]), true));
    let r = eng.search(&SearchRequest { query: r"package.json !D:\".into(), limit: 50, ..Default::default() });
    assert_eq!(r.total, all.len() as u64);
    assert!(r.results.iter().all(|h| h.path.starts_with(r"C:\")));
    let r = eng.search(&SearchRequest { query: r"package.json !D:".into(), limit: 50, ..Default::default() });
    assert_eq!(r.total, all.len() as u64);
}

#[test]
fn search_tokens_match_parent_folders() {
    let ix = build(
        r"C:\",
        &[
            r"dev\node_modules\node-hid\package.json",
            r"dev\node_modules\node-hid\src\index.ts",
            r"dev\node_modules\other\package.json",
            r"dev\node-hid-package.json",
            r"proj\src\index.ts",
            r"proj\src\components\index.ts",
            r"proj\lib\index.ts",
            r"proj\src-index.ts",
        ],
    );
    // All tokens in the name rank above a token found only in a parent folder.
    let want = vec![r"C:\dev\node-hid-package.json", r"C:\dev\node_modules\node-hid\package.json"];
    assert_eq!(names(&ix, "node-hid package.json"), want);
    assert_eq!(names(&ix, "package.json node-hid"), want);
    assert_eq!(names(&ix, "NODE-HID Package.JSON"), want);
    // A lone token still has to be in the name.
    assert_eq!(names(&ix, "node-hid"), vec![r"C:\dev\node_modules\node-hid", r"C:\dev\node-hid-package.json"]);
    // At least one token in the name; the rest anywhere up the chain.
    assert_eq!(
        names(&ix, "src index.ts"),
        vec![
            r"C:\proj\src-index.ts",
            r"C:\proj\src\index.ts",
            r"C:\proj\src\components\index.ts",
            r"C:\dev\node_modules\node-hid\src\index.ts",
        ]
    );
    assert_eq!(names(&ix, "node_modules dev"), vec![r"C:\dev\node_modules"]);
    assert_eq!(names(&ix, "proj dev"), Vec::<String>::new());
    // Path tokens combine with filters and excludes.
    assert_eq!(names(&ix, r"index.ts src !node_modules\ type:file").len(), 3);
    assert_eq!(names(&ix, "ext:ts src"), names(&ix, "src ext:ts"));

    // Highlights cover only what's in the name; total counts each hit once.
    let eng = Engine::new();
    eng.add_volume(Volume::new(ix, true));
    let r = eng.search(&SearchRequest { query: "node-hid package.json".into(), limit: 50, ..Default::default() });
    assert_eq!(r.total, 2);
    assert_eq!(r.results[1].name, "package.json");
    assert_eq!(r.results[1].matches, (0..12).collect::<Vec<u32>>());
    assert!(r.results[0].score > r.results[1].score);
}

#[test]
fn search_path_tokens_incremental_matches_full_scan() {
    let mut paths: Vec<String> = Vec::new();
    for i in 0..2000 {
        paths.push(format!(r"w{}\node-hid{}\package.json", i % 11, i % 7));
        paths.push(format!(r"w{}\pkg{}\node-hid-{}.json", i % 5, i % 3, i));
        paths.push(format!(r"w{}\src\index{}.ts", i % 13, i % 50));
    }
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let ix = build(r"C:\", &refs);
    let opts = RunOpts { limit: 30, frecency: &[], fuzzy: true };
    let mut cache: Option<(query::Query, Vec<Vec<u32>>)> = None;
    // Cached refinements check folders through path strings, fresh scans
    // through folder bit sets: both must agree (incl. path excludes).
    let seq = [
        "node-hid p", "node-hid pa", "node-hid pac", "node-hid pack", r"node-hid pack !w3\", r"node-hid packa !w3\",
        r"node-hid packag !w3\ !\pkg1\", r"node-hid package !w3\ !\pkg1\", r"pack !\w2", r"packa !\w2", "src i",
        "src in", "src ind", r"src ind !c:\w1", r"src inde !c:\w1", r"src index !c:\w1\", r"src index1 !c:\w1\",
    ];
    for qs in seq {
        let q = parse_at(qs, 0);
        let full = run(&[&ix], &q, &opts, None);
        let reuse = cache.as_ref().filter(|(cq, _)| q.refines(cq)).map(|(_, c)| c.as_slice());
        let inc = run(&[&ix], &q, &opts, reuse);
        assert_eq!(full.total, inc.total, "{qs}");
        assert_eq!(full.hits, inc.hits, "{qs}");
        if let Some(c) = inc.cands {
            cache = Some((q, c));
        }
    }
}
