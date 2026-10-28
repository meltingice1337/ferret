// The folder filter (Ctrl+F): words in the name, plus what search understands for types:
// `type:image`, `type:folder`, `ext:pdf,docx`, `!word` to leave out, "quoted words" together.
import { FILE_WORDS, findType } from "./filetypes";
import type { Entry } from "./types";

/** Lower case without accents, so `cafe` finds "Café" and `munchen` "München" (as search does). */
export function fold(s: string): string {
  const l = s.toLowerCase();
  // Plain ASCII (most names) has nothing to fold.
  if (!/[^\x20-\x7e]/.test(l)) return l;
  return l.normalize("NFD").replace(/\p{M}/gu, "")
    .replace(/[ßæœøłđıĳþ]/g, (c) => ({ ß: "ss", æ: "ae", œ: "oe", ø: "o", ł: "l", đ: "d", ı: "i", ĳ: "ij", þ: "th" })[c] ?? c);
}

/** A test for the filter text, or null when it filters nothing. Types and extensions add up
 *  (`type:image ext:pdf` = images or PDFs); words must all be in the name. A type that isn't
 *  known (yet, while typing) is ignored. */
export function entryFilter(text: string): ((e: Entry) => boolean) | null {
  const words: string[] = [];
  const not: string[] = [];
  const exts = new Set<string>();
  let folders = false;
  let files = false;
  let typed = false;
  for (const m of text.matchAll(/"([^"]*)"?|(\S+)/g)) {
    if (m[1] !== undefined) {
      if (m[1].trim()) words.push(fold(m[1]));
      continue;
    }
    const tok = m[2];
    const filter = /^(type|kind|ext):(.*)$/i.exec(tok);
    if (filter) {
      const isExt = filter[1].toLowerCase() === "ext";
      for (const v of filter[2].toLowerCase().split(/[,;|]/).map((s) => s.trim().replace(/^\./, "")).filter(Boolean)) {
        if (isExt) { exts.add(v); typed = true; continue; }
        const t = findType(v);
        if (t?.id === "folder") folders = typed = true;
        else if (FILE_WORDS.includes(v)) files = typed = true;
        else if (t) { t.exts.forEach((x) => exts.add(x)); typed = true; }
      }
      continue;
    }
    if (tok.startsWith("!") && tok.length > 1) not.push(fold(tok.slice(1)));
    else words.push(fold(tok));
  }
  if (!typed && !words.length && !not.length) return null;
  return (e) => {
    if (typed && !((folders && e.isDir) || (files && !e.isDir) || (!e.isDir && exts.has(e.ext.toLowerCase())))) return false;
    const name = fold(e.name);
    return words.every((w) => name.includes(w)) && !not.some((w) => name.includes(w));
  };
}
