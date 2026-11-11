//! The folder filter (Ctrl+F), as src/lib/filter.ts: words in the name (all of them, accents
//! ignored), `!word` to leave out, "quoted words" together, and `type:image`, `type:folder`,
//! `ext:pdf,docx` (types and extensions add up). A type that isn't known yet is ignored.

use std::collections::HashSet;

use ferret_core::listing::Entry;

use crate::kinds::{self, Found};

/// Lower case without accents, so `cafe` finds "Café" (as search does).
pub fn fold(s: &str) -> String {
    let l = s.to_lowercase();
    if l.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return l;
    }
    use unicode_normalization::UnicodeNormalization;
    l.nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(|c| match c {
            'ß' => "ss".chars().collect::<Vec<_>>(),
            'æ' => "ae".chars().collect(),
            'œ' => "oe".chars().collect(),
            'ø' => vec!['o'],
            'ł' => vec!['l'],
            'đ' => vec!['d'],
            'ı' => vec!['i'],
            'þ' => "th".chars().collect(),
            c => vec![c],
        })
        .collect()
}

pub struct Filter {
    words: Vec<String>,
    not: Vec<String>,
    exts: HashSet<String>,
    folders: bool,
    files: bool,
    typed: bool,
}

impl Filter {
    /// None when the text filters nothing.
    pub fn parse(text: &str) -> Option<Filter> {
        let mut f = Filter { words: Vec::new(), not: Vec::new(), exts: HashSet::new(), folders: false, files: false, typed: false };
        let mut rest = text;
        while !rest.trim_start().is_empty() {
            rest = rest.trim_start();
            if let Some(q) = rest.strip_prefix('"') {
                let end = q.find('"').unwrap_or(q.len());
                let phrase = &q[..end];
                if !phrase.trim().is_empty() {
                    f.words.push(fold(phrase));
                }
                rest = q.get(end + 1..).unwrap_or("");
                continue;
            }
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let tok = &rest[..end];
            rest = &rest[end..];
            let lower = tok.to_lowercase();
            if let Some((key, values)) = lower.split_once(':').filter(|(k, _)| matches!(*k, "type" | "kind" | "ext")) {
                for v in values.split([',', ';', '|']).map(|s| s.trim().trim_start_matches('.')).filter(|s| !s.is_empty()) {
                    if key == "ext" {
                        f.exts.insert(v.to_string());
                        f.typed = true;
                        continue;
                    }
                    match kinds::find_type(v) {
                        Some(Found::Folders) => {
                            f.folders = true;
                            f.typed = true;
                        }
                        Some(Found::Files) => {
                            f.files = true;
                            f.typed = true;
                        }
                        Some(Found::Exts(exts)) => {
                            f.exts.extend(exts);
                            f.typed = true;
                        }
                        None => {}
                    }
                }
                continue;
            }
            if tok.len() > 1 && tok.starts_with('!') {
                f.not.push(fold(&tok[1..]));
            } else {
                f.words.push(fold(tok));
            }
        }
        (f.typed || !f.words.is_empty() || !f.not.is_empty()).then_some(f)
    }

    pub fn matches(&self, e: &Entry) -> bool {
        if self.typed && !((self.folders && e.is_dir) || (self.files && !e.is_dir) || (!e.is_dir && self.exts.contains(&e.ext.to_lowercase()))) {
            return false;
        }
        let name = fold(&e.name);
        self.words.iter().all(|w| name.contains(w.as_str())) && !self.not.iter().any(|w| name.contains(w.as_str()))
    }
}

/// The filter's `type:` value, if it has one (the "Show only" button lights up).
pub fn type_of(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|t| t.strip_prefix("type:").map(str::to_string))
}

/// The filter text with its `type:` replaced (or removed with None).
pub fn with_type(text: &str, ty: Option<&str>) -> String {
    let mut words: Vec<String> = text.split_whitespace().filter(|t| !t.starts_with("type:")).map(str::to_string).collect();
    if let Some(t) = ty {
        words.push(format!("type:{t}"));
    }
    let mut s = words.join(" ");
    if !s.is_empty() && ty.is_none() {
        s.push(' ');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> Entry {
        let ext = if is_dir { String::new() } else { name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default() };
        Entry {
            name: name.into(),
            path: format!("C:\\x\\{name}"),
            is_dir,
            is_symlink: false,
            hidden: false,
            size: 0,
            modified: None,
            created: None,
            ext,
            in_archive: false,
            origin: None,
        }
    }

    #[test]
    fn words_types_and_exclusions() {
        let f = Filter::parse("cafe !draft").unwrap();
        assert!(f.matches(&entry("Café menu.pdf", false)));
        assert!(!f.matches(&entry("cafe draft.pdf", false)));
        let f = Filter::parse("type:image").unwrap();
        assert!(f.matches(&entry("a.JPG", false)));
        assert!(!f.matches(&entry("a.pdf", false)));
        let f = Filter::parse("type:folder ext:pdf").unwrap();
        assert!(f.matches(&entry("docs", true)));
        assert!(f.matches(&entry("x.pdf", false)));
        assert!(!f.matches(&entry("x.txt", false)));
        assert!(Filter::parse("  ").is_none());
        let f = Filter::parse("\"two words\"").unwrap();
        assert!(f.matches(&entry("has two words.txt", false)));
        assert!(!f.matches(&entry("two.txt", false)));
    }
}
