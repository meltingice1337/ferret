// What the search box can work out from its text alone: filters whose value search can't read
// (search/query.rs ignores those silently), and how results and names are laid out.

const SIZE = /^(?:\d+\.?\d*|\.\d+)\s*(?:b|k|kb|kib|m|mb|mib|g|gb|gib|t|tb|tib)?$/i;
const AGE = /^\d+(?:s|secs?|m|mins?|h|hrs?|hours?|d|days?|w|wk|weeks?|mo|months?|y|yr|years?)$/i;
const DATE_WORDS = new Set(["today", "yesterday", "week", "thisweek", "month", "thismonth", "year", "thisyear"]);

function validDate(v: string): boolean {
  const parts = v.trim().split(/[-/.]/);
  if (parts.length > 3 || !parts.every((p) => /^\d+$/.test(p))) return false;
  const [y, m = 1, d = 1] = parts.map(Number);
  if (y < 1601 || m < 1 || m > 12 || d < 1) return false;
  return d <= new Date(y, m, 0).getDate();
}
const dropCompare = (v: string) => v.replace(/^(?:>=|<=|>|<|=)\s*/, "");

export function validSizeFilter(value: string): boolean {
  const v = value.trim();
  if (v.toLowerCase() === "empty") return true;
  const range = v.split("..");
  if (range.length === 2) return SIZE.test(range[0].trim()) && SIZE.test(range[1].trim());
  return SIZE.test(dropCompare(v));
}

export function validDateFilter(value: string): boolean {
  const v = value.trim().toLowerCase();
  if (DATE_WORDS.has(v)) return true;
  const range = v.split("..");
  if (range.length === 2) return validDate(range[0]) && validDate(range[1]);
  const rest = dropCompare(v);
  return AGE.test(rest) || validDate(rest);
}

/** The first `size:` or `modified:` filter search can't read (it would ignore it). A value still
 *  empty ("size:", "size:>") is one being typed. */
export function badFilter(query: string): { raw: string; kind: "size" | "date"; example: string } | null {
  for (const raw of query.split(/\s+/)) {
    const m = /^(size|modified|mod|dm|date|datemodified):(.*)$/i.exec(raw);
    if (!m || !dropCompare(m[2])) continue;
    if (m[1].toLowerCase() === "size") {
      if (!validSizeFilter(m[2])) return { raw, kind: "size", example: "size:>100mb" };
    } else if (!validDateFilter(m[2])) {
      return { raw, kind: "date", example: `${m[1]}:<7d` };
    }
  }
  return null;
}

const FILTER = /(^|\s)(?:type|kind|ext|size|modified|mod|dm|date|datemodified|in|scope|content):(?:"[^"]*"?|\S*)/gi;

/** The box without its filters: what "Remove filters" leaves. */
export function withoutFilters(query: string): string {
  return query.replace(FILTER, "$1").replace(/\s+/g, " ").trim();
}

/** The box ends with a filter that has no value yet ("ext:", "size:>", `in:"`): there's
 *  nothing to give a verdict on. */
export function awaitingValue(query: string): boolean {
  return /(?:^|\s)!?(?:type|kind|ext|size|modified|mod|dm|date|datemodified|in|scope|content):(?:[<>]=?|=|")?$/i.test(query.trim());
}

/** The folder an `in:` names. A quote still open runs to the next filter, if there is one. */
export function inFolderPath(query: string): string | null {
  const m = /(?:^|\s)(?:in|scope):(?:"([^"]+)("?)|(\S+))/i.exec(query);
  if (!m) return null;
  const dir = m[3] ?? (m[2] ? m[1] : m[1].replace(/\s+!?\w+:.*$/, ""));
  return dir.trim() || null;
}

/** The folder an `in:` names, as it should read in a chip or a sentence. */
export function inFolderName(query: string): string | null {
  const dir = inFolderPath(query)?.replace(/[\\/]+$/, "");
  if (!dir) return null;
  return dir.split(/[\\/]/).pop() || dir;
}

const fold = (s: string) => s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();

/** A fuzzy hit: some word of the search appears neither in the name nor in the path, only as
 *  scattered letters. Search ranks these last. Wildcards and paths are matched exactly. */
export function isLooseMatch(path: string, query: string): boolean {
  const where = fold(path);
  return withoutFilters(query)
    .split(/\s+/)
    .filter((w) => w && !w.startsWith("!") && !/[*?"\\/]/.test(w))
    .some((w) => !where.includes(fold(w)));
}

/** The middle one of the bigger names: one very long name shouldn't take the paths' room from
 *  every row (it shortens instead). */
export function nameColumn(widths: number[]): number {
  if (!widths.length) return 120;
  const sorted = [...widths].sort((a, b) => a - b);
  return Math.min(260, Math.max(120, sorted[Math.floor((sorted.length - 1) * 0.75)]));
}

/** A long name in two parts, so it can shorten in the middle: names that differ only at the
 *  end (or by extension) stay different. */
export function splitName(name: string): [string, string] {
  if (name.length <= 24) return [name, ""];
  const dot = name.lastIndexOf(".");
  const ext = dot > 0 ? name.length - dot : 0;
  let at = name.length - Math.min(Math.max(10, ext + 6), Math.floor(name.length / 2));
  // Not between the two halves of an emoji.
  const c = name.charCodeAt(at);
  if (c >= 0xdc00 && c <= 0xdfff) at--;
  return [name.slice(0, at), name.slice(at)];
}

/** Results from the open folder first, then the rest without those. */
export function mergeNear<T extends { path: string }>(near: T[], all: T[]): { hit: T; near: boolean }[] {
  const seen = new Set(near.map((h) => h.path.toLowerCase()));
  return [
    ...near.map((hit) => ({ hit, near: true })),
    ...all.filter((h) => !seen.has(h.path.toLowerCase())).map((hit) => ({ hit, near: false })),
  ];
}
