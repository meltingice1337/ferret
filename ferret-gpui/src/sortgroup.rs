//! Sorting and grouping a folder's items, as src/lib/sortgroup.ts does: the same sort keys,
//! natural name order (numbers by value, case and accents ignored), the same groups (date, type,
//! size, first letter) and the same group order rules. And the view settings they come from,
//! as ferret-core stores them (per folder, inherited, by profile, or the global default).

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    List,
    Grid,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    #[default]
    Name,
    Modified,
    Created,
    Size,
    Type,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    #[default]
    None,
    Date,
    Type,
    Size,
    Letter,
}

/// A folder's view (`ViewSettings` in src/lib/types.ts; ferret-core stores it as JSON).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ViewSettings {
    pub mode: Mode,
    pub sort_by: SortBy,
    pub sort_dir: SortDir,
    pub group_by: GroupBy,
    pub folders_first: bool,
    pub show_hidden: bool,
    pub icon_size: u32,
    pub preview_open: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            mode: Mode::List,
            sort_by: SortBy::Name,
            sort_dir: SortDir::Asc,
            group_by: GroupBy::None,
            folders_first: true,
            show_hidden: false,
            icon_size: 96,
            preview_open: false,
        }
    }
}

impl ViewSettings {
    pub fn from_json(v: &serde_json::Value) -> Self {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

/// What sorting and grouping need to know about an item.
pub trait Item {
    fn is_dir(&self) -> bool;
    fn name(&self) -> &str;
    /// The name as `fold` makes it.
    fn folded(&self) -> &[char];
    fn modified(&self) -> Option<i64>;
    fn created(&self) -> Option<i64>;
    /// Bytes (a folder's counted size when known).
    fn size(&self) -> u64;
    fn type_label(&self) -> &str;
    /// "Folders", "Images"… for grouping by type (`KIND_LABEL`), with a rank: folders first.
    fn kind_label(&self) -> &str;
}

/// Lowercase, accents dropped (NFD without combining marks): "sensitivity: base".
pub fn fold(s: &str) -> Vec<char> {
    s.nfd().filter(|c| !('\u{300}'..='\u{36f}').contains(c)).flat_map(char::to_lowercase).collect()
}

/// Natural order like `Intl.Collator(undefined, { numeric: true, sensitivity: "base" })`:
/// "file 2" before "file 10"; case and accents ignored.
pub fn collate(a: &str, b: &str) -> Ordering {
    collate_folded(&fold(a), &fold(b))
}

/// `collate` on names already folded (rows keep theirs: sorting 100k names folds each once).
pub fn collate_folded(a: &[char], b: &[char]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let (na, nb): (String, String) = (a[si..i].iter().collect(), b[sj..j].iter().collect());
            let (na, nb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
            let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            let ord = a[i].cmp(&b[j]);
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// Sorts in place, folders first when the view says so.
pub fn sort<T: Item>(items: &mut [T], v: &ViewSettings) {
    let dir = if v.sort_dir == SortDir::Asc { Ordering::Less } else { Ordering::Greater };
    items.sort_by(|a, b| {
        if v.folders_first && a.is_dir() != b.is_dir() {
            return if a.is_dir() { Ordering::Less } else { Ordering::Greater };
        }
        let r = match v.sort_by {
            SortBy::Modified => a.modified().unwrap_or(0).cmp(&b.modified().unwrap_or(0)),
            SortBy::Created => a.created().unwrap_or(0).cmp(&b.created().unwrap_or(0)),
            SortBy::Size => a.size().cmp(&b.size()),
            SortBy::Type => collate(a.type_label(), b.type_label()),
            SortBy::Name => Ordering::Equal,
        }
        .then_with(|| collate_folded(a.folded(), b.folded()));
        if dir == Ordering::Less { r } else { r.reverse() }
    });
}

/// A group of contiguous items after grouping.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub label: String,
    pub start: usize,
    pub len: usize,
}

struct Bucket {
    key: String,
    label: String,
    rank: f64,
}

const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

const SIZE_BUCKETS: [(f64, &str); 6] = [
    (0., "Empty"),
    (16. * 1024., "Tiny (< 16 KB)"),
    (1_048_576., "Small (< 1 MB)"),
    (128. * 1_048_576., "Medium (< 128 MB)"),
    (1_073_741_824., "Large (< 1 GB)"),
    (f64::INFINITY, "Huge (> 1 GB)"),
];

fn date_bucket(ms: Option<i64>, now: chrono::DateTime<chrono::Local>) -> Bucket {
    use chrono::{Datelike, Duration, Local, TimeZone};
    let Some(ms) = ms else { return Bucket { key: "unknown".into(), label: "Unknown date".into(), rank: f64::NEG_INFINITY } };
    let day = 86_400_000i64;
    let at = |y: i32, m: u32, d: u32| Local.with_ymd_and_hms(y, m, d, 0, 0, 0).single().map_or(0, |t| t.timestamp_millis());
    let today = at(now.year(), now.month(), now.day());
    let weekday = now.weekday().num_days_from_monday() as i64;
    let week = today - weekday * day;
    let month = at(now.year(), now.month(), 1);
    let prev = now.with_day(1).unwrap_or(now) - Duration::days(1);
    let prev_month = at(prev.year(), prev.month(), 1);
    let b = |key: &str, label: &str, rank: i64| Bucket { key: key.into(), label: label.into(), rank: rank as f64 };
    if ms >= today {
        return b("today", "Today", today);
    }
    if ms >= today - day {
        return b("yesterday", "Yesterday", today - day);
    }
    if ms >= week {
        return b("week", "Earlier this week", week);
    }
    if ms >= week - 7 * day {
        return b("lastweek", "Last week", week - 7 * day);
    }
    if ms >= month {
        return b("month", "Earlier this month", month);
    }
    if ms >= prev_month {
        return b("lastmonth", "Last month", prev_month);
    }
    let d = Local.timestamp_millis_opt(ms).single().unwrap_or(now);
    if d.year() == now.year() {
        return b(&format!("m{}", d.month0()), MONTHS[d.month0() as usize], at(d.year(), d.month(), 1));
    }
    b(&format!("y{}", d.year()), &d.year().to_string(), at(d.year(), 1, 1))
}

fn bucket_of<T: Item>(e: &T, v: &ViewSettings, now: chrono::DateTime<chrono::Local>) -> Bucket {
    match v.group_by {
        GroupBy::Date => date_bucket(if v.sort_by == SortBy::Created { e.created() } else { e.modified() }, now),
        GroupBy::Type => {
            if e.is_dir() {
                Bucket { key: "folder".into(), label: "Folders".into(), rank: f64::INFINITY }
            } else {
                Bucket { key: e.kind_label().into(), label: e.kind_label().into(), rank: 0. }
            }
        }
        GroupBy::Size => {
            if e.is_dir() {
                return Bucket { key: "folder".into(), label: "Folders".into(), rank: -1. };
            }
            let size = e.size() as f64;
            let i = SIZE_BUCKETS.iter().position(|(max, _)| if *max == 0. { size == 0. } else { size < *max }).unwrap_or(5);
            Bucket { key: format!("s{i}"), label: SIZE_BUCKETS[i].1.into(), rank: i as f64 }
        }
        GroupBy::Letter => {
            let c = e.name().chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or('#');
            if c.is_ascii_uppercase() {
                Bucket { key: c.to_string(), label: c.to_string(), rank: -(c as u32 as f64) }
            } else if c.is_ascii_digit() {
                Bucket { key: "0-9".into(), label: "0–9".into(), rank: 1. }
            } else {
                Bucket { key: "#".into(), label: "#".into(), rank: 2. }
            }
        }
        GroupBy::None => Bucket { key: "all".into(), label: String::new(), rank: 0. },
    }
}

/// Groups an already-sorted list: reorders it so each group's items are together (keeping their
/// sort order inside), and returns the groups in the app's order.
pub fn group<T: Item>(sorted: Vec<T>, v: &ViewSettings) -> (Vec<T>, Vec<Group>) {
    if v.group_by == GroupBy::None {
        let len = sorted.len();
        return (sorted, vec![Group { label: String::new(), start: 0, len }]);
    }
    let now = chrono::Local::now();
    let mut buckets: Vec<(Bucket, Vec<T>)> = Vec::new();
    for e in sorted {
        let b = bucket_of(&e, v, now);
        match buckets.iter_mut().find(|(x, _)| x.key == b.key) {
            Some((_, items)) => items.push(e),
            None => buckets.push((b, vec![e])),
        }
    }
    // Newest dates first, small sizes first, A→Z, folders first for type (sortgroup.ts).
    let mut desc = matches!(v.group_by, GroupBy::Date | GroupBy::Type | GroupBy::Letter);
    if v.group_by == GroupBy::Date && matches!(v.sort_by, SortBy::Modified | SortBy::Created) {
        desc = v.sort_dir == SortDir::Desc;
    }
    if v.group_by == GroupBy::Size && v.sort_by == SortBy::Size {
        desc = v.sort_dir == SortDir::Desc;
    }
    if v.group_by == GroupBy::Letter && v.sort_by == SortBy::Name {
        desc = v.sort_dir == SortDir::Asc;
    }
    buckets.sort_by(|(a, _), (b, _)| {
        if v.group_by == GroupBy::Type {
            if a.rank != b.rank {
                return b.rank.total_cmp(&a.rank);
            }
            return collate(&a.label, &b.label);
        }
        if desc { b.rank.total_cmp(&a.rank) } else { a.rank.total_cmp(&b.rank) }
    });
    let mut out = Vec::new();
    let mut groups = Vec::new();
    for (b, items) in buckets {
        groups.push(Group { label: b.label, start: out.len(), len: items.len() });
        out.extend(items);
    }
    (out, groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct E {
        name: &'static str,
        folded: Vec<char>,
        dir: bool,
        size: u64,
        modified: i64,
    }

    impl Item for E {
        fn is_dir(&self) -> bool { self.dir }
        fn name(&self) -> &str { self.name }
        fn folded(&self) -> &[char] { &self.folded }
        fn modified(&self) -> Option<i64> { Some(self.modified) }
        fn created(&self) -> Option<i64> { None }
        fn size(&self) -> u64 { self.size }
        fn type_label(&self) -> &str { if self.dir { "Folder" } else { "File" } }
        fn kind_label(&self) -> &str { if self.dir { "Folder" } else { "File" } }
    }

    fn e(name: &'static str, dir: bool, size: u64) -> E {
        E { name, folded: fold(name), dir, size, modified: 0 }
    }

    #[test]
    fn natural_order_ignores_case_and_accents() {
        assert_eq!(collate("file 2", "file 10"), Ordering::Less);
        assert_eq!(collate("File", "file"), Ordering::Equal);
        assert_eq!(collate("café", "cafe"), Ordering::Equal);
        assert_eq!(collate("a", "B"), Ordering::Less);
        assert_eq!(collate("img007", "img7"), Ordering::Equal);
    }

    #[test]
    fn folders_first_then_by_size_descending() {
        let mut v = vec![e("b.txt", false, 5), e("a", true, 0), e("c.txt", false, 50)];
        let view = ViewSettings { sort_by: SortBy::Size, sort_dir: SortDir::Desc, ..Default::default() };
        sort(&mut v, &view);
        assert_eq!(v.iter().map(|x| x.name).collect::<Vec<_>>(), ["a", "c.txt", "b.txt"]);
    }

    #[test]
    fn size_groups_in_order() {
        let v = vec![e("big", false, 2 << 30), e("empty", false, 0), e("small", false, 1000)];
        let view = ViewSettings { group_by: GroupBy::Size, ..Default::default() };
        let (items, groups) = group(v, &view);
        assert_eq!(groups.iter().map(|g| g.label.as_str()).collect::<Vec<_>>(), ["Empty", "Tiny (< 16 KB)", "Huge (> 1 GB)"]);
        assert_eq!(items[0].name, "empty");
    }

    #[test]
    fn view_settings_read_the_app_json() {
        let v = ViewSettings::from_json(&serde_json::json!({ "mode": "grid", "sortBy": "modified", "sortDir": "desc", "groupBy": "date" }));
        assert_eq!((v.mode, v.sort_by, v.sort_dir, v.group_by, v.folders_first), (Mode::Grid, SortBy::Modified, SortDir::Desc, GroupBy::Date, true));
    }
}
