// File types for search and the folder filter: `type:image`, `kind:photos`, `type:video,audio`.
// The list (filetypes.json) is shared with the backend's search.
import table from "./filetypes.json";

export interface FileType {
  id: string;
  label: string;
  /** Other names it goes by in `type:` ("photos", "pictures"). */
  words: string[];
  /** Plain search words that offer to become this type ("vacation photos"). */
  suggest: string[];
  exts: string[];
}

const split = (s = "") => s.split(/\s+/).filter(Boolean);

/** Icon kinds and their extensions (finer than types: Documents covers doc, pdf and sheet). */
export const KIND_EXTS: Record<string, string[]> = Object.fromEntries(Object.entries(table.kinds).map(([k, v]) => [k, split(v)]));

export const FILE_TYPES: FileType[] = table.types.map((t) => ({
  id: t.id,
  label: t.label,
  words: t.words,
  suggest: t.suggest,
  exts: [...new Set([...t.kinds.flatMap((k) => KIND_EXTS[k] ?? []), ...split((t as { also?: string }).also)])],
}));

export const FOLDER_WORDS = ["folder", "folders", "dir", "dirs", "directory"];
export const FILE_WORDS = ["file", "files"];
export const FOLDERS: FileType = { id: "folder", label: "Folders", words: FOLDER_WORDS, suggest: [], exts: [] };

/** Kinds that aren't types of their own but work in `type:` too. */
const KIND_LABELS: Record<string, string> = { pdf: "PDFs", sheet: "Spreadsheets", exe: "Programs and shortcuts" };

/** A type by its id or any of its words ("photos"), an icon kind ("pdf"), or folders. */
export function findType(word: string): FileType | undefined {
  const w = word.trim().toLowerCase().replace(/^\./, "");
  if (FOLDER_WORDS.includes(w)) return FOLDERS;
  const t = FILE_TYPES.find((t) => t.id === w || t.words.includes(w));
  if (t) return t;
  if (KIND_EXTS[w]) return { id: w, label: KIND_LABELS[w] ?? w.toUpperCase(), words: [], suggest: [], exts: KIND_EXTS[w] };
}

/** Does `type:` understand this word? */
export const knownType = (word: string) => !!findType(word) || FILE_WORDS.includes(word.trim().toLowerCase());

function distance(a: string, b: string): number {
  const row = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    let prev = row[0]++;
    for (let j = 1; j <= b.length; j++) {
      const cur = row[j];
      row[j] = Math.min(row[j] + 1, row[j - 1] + 1, prev + (a[i - 1] === b[j - 1] ? 0 : 1));
      prev = cur;
    }
  }
  return row[b.length];
}

/** The type closest to a misspelled word ("imgz", "vidoes"), by its main name, if any is close. */
export function didYouMean(word: string): string | undefined {
  const w = word.trim().toLowerCase();
  if (w.length < 2) return;
  // Every name a type goes by, with the name to suggest instead.
  const names: [string, string][] = [
    ...FILE_TYPES.flatMap((t) => [t.id, ...t.words].map((n): [string, string] => [n, t.id])),
    ...[...FOLDER_WORDS, ...FILE_WORDS, ...Object.keys(KIND_LABELS)].map((n): [string, string] => [n, FOLDER_WORDS.includes(n) ? "folder" : FILE_WORDS.includes(n) ? "file" : n]),
  ];
  let best: string | undefined;
  let bestD = Infinity;
  for (const [n, main] of names) {
    const d = n.startsWith(w) ? 0.5 : distance(w, n);
    if (d < bestD) { best = main; bestD = d; }
  }
  return bestD <= (w.length <= 4 ? 1 : 2) ? best : undefined;
}

/** `type:` and `kind:` filters in a query: each with its values. Quoted text doesn't count. */
export function typeTokens(query: string): { raw: string; values: string[] }[] {
  const out: { raw: string; values: string[] }[] = [];
  for (const m of query.matchAll(/"[^"]*"?|\S+/g)) {
    const t = /^(?:type|kind):(.*)$/i.exec(m[0]);
    if (t) out.push({ raw: m[0], values: t[1].split(/[,;|]/).map((v) => v.trim()).filter(Boolean) });
  }
  return out;
}

/** The query without its `type:` and `kind:` filters. */
export const withoutTypes = (query: string) =>
  typeTokens(query).reduce((q, t) => q.replace(t.raw, " "), query).replace(/\s+/g, " ").trim();

/** Extensions shown for a type, short: "jpg, png, gif +19". */
export function extPreview(t: FileType, n = 4): string {
  const shown = t.exts.slice(0, n).join(", ");
  return t.exts.length > n ? `${shown} +${t.exts.length - n}` : shown;
}
