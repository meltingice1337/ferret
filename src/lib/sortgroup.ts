import { KIND_LABEL, kindOf, typeLabel } from "./format";
import type { Entry, ViewSettings } from "./types";

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

export function sortEntries(list: Entry[], v: ViewSettings): Entry[] {
  const dir = v.sortDir === "asc" ? 1 : -1;
  const by = v.sortBy;
  const cmp = (a: Entry, b: Entry): number => {
    let r = 0;
    switch (by) {
      case "modified": r = (a.modified ?? 0) - (b.modified ?? 0); break;
      case "created": r = (a.created ?? 0) - (b.created ?? 0); break;
      case "size": r = a.size - b.size; break;
      case "type": r = collator.compare(typeLabel(a), typeLabel(b)); break;
    }
    if (r === 0) r = collator.compare(a.name, b.name);
    return r * dir;
  };
  return [...list].sort((a, b) => {
    if (v.foldersFirst && a.isDir !== b.isDir) return a.isDir ? -1 : 1;
    return cmp(a, b);
  });
}

export interface Group {
  key: string;
  label: string;
  items: Entry[];
}

interface Bucket {
  key: string;
  label: string;
  rank: number;
}

const MONTHS = ["January", "February", "March", "April", "May", "June", "July",
  "August", "September", "October", "November", "December"];

function dateBucketer(now = new Date()) {
  const day = 86_400_000;
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const weekday = (now.getDay() + 6) % 7; // Monday = 0
  const week = today - weekday * day;
  const month = new Date(now.getFullYear(), now.getMonth(), 1).getTime();
  const prevMonth = new Date(now.getFullYear(), now.getMonth() - 1, 1).getTime();
  const year = now.getFullYear();
  return (ms: number | null): Bucket => {
    if (ms == null) return { key: "unknown", label: "Unknown date", rank: -Infinity };
    if (ms >= today) return { key: "today", label: "Today", rank: today };
    if (ms >= today - day) return { key: "yesterday", label: "Yesterday", rank: today - day };
    if (ms >= week) return { key: "week", label: "Earlier this week", rank: week };
    if (ms >= week - 7 * day) return { key: "lastweek", label: "Last week", rank: week - 7 * day };
    if (ms >= month) return { key: "month", label: "Earlier this month", rank: month };
    if (ms >= prevMonth) return { key: "lastmonth", label: "Last month", rank: prevMonth };
    const d = new Date(ms);
    if (d.getFullYear() === year) {
      const start = new Date(year, d.getMonth(), 1).getTime();
      return { key: `m${d.getMonth()}`, label: MONTHS[d.getMonth()], rank: start };
    }
    return { key: `y${d.getFullYear()}`, label: String(d.getFullYear()), rank: new Date(d.getFullYear(), 0, 1).getTime() };
  };
}

const SIZE_BUCKETS: [number, string][] = [
  [0, "Empty"], [16 * 1024, "Tiny (< 16 KB)"], [1024 ** 2, "Small (< 1 MB)"],
  [128 * 1024 ** 2, "Medium (< 128 MB)"], [1024 ** 3, "Large (< 1 GB)"], [Infinity, "Huge (> 1 GB)"],
];

function bucketOf(e: Entry, v: ViewSettings, date: (ms: number | null) => Bucket): Bucket {
  switch (v.groupBy) {
    case "date":
      return date(v.sortBy === "created" ? e.created : e.modified);
    case "type": {
      const k = kindOf(e);
      if (k === "folder") return { key: "folder", label: "Folders", rank: Infinity };
      return { key: k, label: KIND_LABEL[k], rank: 0 };
    }
    case "size": {
      if (e.isDir) return { key: "folder", label: "Folders", rank: -1 };
      const i = SIZE_BUCKETS.findIndex(([max]) => (max === 0 ? e.size === 0 : e.size < max));
      return { key: `s${i}`, label: SIZE_BUCKETS[i][1], rank: i };
    }
    case "letter": {
      const c = e.name.charAt(0).toUpperCase();
      if (/[A-Z]/.test(c)) return { key: c, label: c, rank: -c.charCodeAt(0) };
      if (/[0-9]/.test(c)) return { key: "0-9", label: "0–9", rank: 1 };
      return { key: "#", label: "#", rank: 2 };
    }
    default:
      return { key: "all", label: "", rank: 0 };
  }
}

/** Groups an already-sorted list. Items keep their sort order within a group. */
export function groupEntries(sorted: Entry[], v: ViewSettings): Group[] {
  if (v.groupBy === "none") return [{ key: "all", label: "", items: sorted }];
  const date = dateBucketer();
  const map = new Map<string, Bucket & { items: Entry[] }>();
  for (const e of sorted) {
    const b = bucketOf(e, v, date);
    let g = map.get(b.key);
    if (!g) map.set(b.key, (g = { ...b, items: [] }));
    g.items.push(e);
  }
  const groups = [...map.values()];
  // Default group order: newest dates first, small sizes first, A→Z, folders first for type.
  let desc = v.groupBy === "date" || v.groupBy === "type" || v.groupBy === "letter";
  if (v.groupBy === "date" && (v.sortBy === "modified" || v.sortBy === "created")) desc = v.sortDir === "desc";
  if (v.groupBy === "size" && v.sortBy === "size") desc = v.sortDir === "desc";
  if (v.groupBy === "letter" && v.sortBy === "name") desc = v.sortDir === "asc";
  groups.sort((a, b) => {
    if (v.groupBy === "type" && a.rank !== b.rank) return b.rank - a.rank;
    if (v.groupBy === "type") return collator.compare(a.label, b.label);
    return desc ? b.rank - a.rank : a.rank - b.rank;
  });
  return groups.map(({ key, label, items }) => ({ key, label, items }));
}
