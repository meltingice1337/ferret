//! File types in search: `type:image`, `kind:photos`, `type:video,audio`. The list is shared
//! with the app (src/lib/filetypes.json). Types become plain `ext:` lists before a query runs,
//! which every engine understands, the indexer service included (whatever its version).

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Deserialize)]
struct Table {
    /// Icon kinds (finer than types): their extensions, space-separated.
    kinds: HashMap<String, String>,
    types: Vec<FileType>,
}

#[derive(Deserialize)]
struct FileType {
    id: String,
    kinds: Vec<String>,
    #[serde(default)]
    also: String,
    words: Vec<String>,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| serde_json::from_str(include_str!("../../../src/lib/filetypes.json")).expect("filetypes.json"))
}

/// Extensions of a file type, named by its id or any of its words (`image`, `photos`), or of
/// an icon kind (`pdf`, `sheet`). None for anything else, `folder` and `file` included.
pub fn extensions(word: &str) -> Option<Vec<&'static str>> {
    let w = word.trim().to_ascii_lowercase();
    let t = table();
    let exts = |kind: &str| t.kinds.get(kind).into_iter().flat_map(|s| s.split_whitespace());
    let mut out: Vec<&str> = match t.types.iter().find(|ty| ty.id == w || ty.words.iter().any(|x| *x == w)) {
        Some(ty) => ty.kinds.iter().flat_map(|k| exts(k)).chain(ty.also.split_whitespace()).collect(),
        None => exts(&w).collect(),
    };
    let mut seen = std::collections::HashSet::new();
    out.retain(|e| seen.insert(*e));
    (!out.is_empty()).then_some(out)
}

/// `type:image` (or `kind:photos`, `type:image,video`) becomes the matching `ext:` list. Folder
/// and file types, and words that aren't types, are left as typed. Quoted text is untouched.
pub fn expand(query: &str) -> String {
    split(query)
        .into_iter()
        .flat_map(|tok| {
            let Some((key, val)) = tok.split_once(':').filter(|_| !tok.contains('"')) else { return vec![tok] };
            if !matches!(key.to_ascii_lowercase().as_str(), "type" | "kind") {
                return vec![tok];
            }
            let (mut exts, mut keep) = (Vec::new(), Vec::new());
            for part in val.split([',', ';', '|']).filter(|p| !p.trim().is_empty()) {
                match extensions(part) {
                    Some(e) => exts.extend(e.into_iter().filter(|x| !exts.contains(x)).collect::<Vec<_>>()),
                    None => keep.push(part),
                }
            }
            if exts.is_empty() {
                return vec![tok];
            }
            let mut out = vec![format!("ext:{}", exts.join(","))];
            if !keep.is_empty() {
                out.push(format!("{key}:{}", keep.join(",")));
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits on whitespace outside double quotes, keeping the quotes.
fn split(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    for c in s.chars() {
        if c == '"' {
            quoted = !quoted;
        }
        if c.is_whitespace() && !quoted {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_become_extension_lists() {
        let image = expand("type:image");
        assert!(image.starts_with("ext:jpg,png,gif,"), "{image}");
        assert_eq!(expand("type:photos"), image);
        assert_eq!(expand("holiday KIND:Images"), format!("holiday {image}"));
        assert_eq!(expand("type:pdf"), "ext:pdf");
        // A type made of several kinds, and one with extensions of its own.
        let docs = expand("type:documents");
        assert!(docs.starts_with("ext:pdf,docx,") && docs.contains("xlsx"), "{docs}");
        assert!(expand("type:text").ends_with(",csv,tsv"));
        assert_eq!(expand("type:apps"), "ext:exe,msi,appx,msix,appxbundle,msixbundle");
        // Several at once; no extension twice.
        let both = expand("type:text,doc");
        assert_eq!(both.matches(",csv").count(), 1, "{both}");
    }

    #[test]
    fn leaves_everything_else_alone() {
        for q in ["type:folder", "type:file", "type:imgz", "report  ext:pdf", "\"type:image\"", "!type:image", "in:C:\\dev"] {
            assert_eq!(expand(q), q.replace("  ", " "), "{q}");
        }
        assert_eq!(expand("a \"b  c\" d"), "a \"b  c\" d");
        assert!(expand("type:image,folder").ends_with(",psd type:folder"));
    }

    #[test]
    fn typescript_is_code_not_video() {
        for ts in ["ts", "mts"] {
            assert!(!extensions("video").unwrap().contains(&ts));
            assert!(extensions("code").unwrap().contains(&ts));
        }
    }

    #[test]
    fn every_type_has_extensions() {
        for ty in &table().types {
            assert!(extensions(&ty.id).is_some_and(|e| !e.is_empty()), "{}", ty.id);
            for k in &ty.kinds {
                assert!(table().kinds.contains_key(k), "{}: unknown kind {k}", ty.id);
            }
        }
    }
}
