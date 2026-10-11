import { KIND_EXTS } from "./filetypes";
import type { Entry } from "./types";

export type Kind =
  | "folder" | "image" | "video" | "audio" | "code" | "text" | "doc" | "sheet"
  | "pdf" | "archive" | "exe" | "font" | "data" | "other";

const EXT = new Map<string, Kind>();
for (const [kind, exts] of Object.entries(KIND_EXTS)) for (const e of exts) EXT.set(e, kind as Kind);

export function kindOf(e: Pick<Entry, "isDir" | "ext">): Kind {
  if (e.isDir) return "folder";
  return EXT.get(e.ext) ?? "other";
}

export const KIND_LABEL: Record<Kind, string> = {
  folder: "Folder", image: "Image", video: "Video", audio: "Audio", code: "Source code",
  text: "Text", doc: "Document", sheet: "Spreadsheet", pdf: "PDF", archive: "Archive",
  exe: "Application", font: "Font", data: "Data", other: "File",
};

const EXT_LABEL: Record<string, string> = {
  md: "Markdown", markdown: "Markdown", txt: "Text document", log: "Log file", pdf: "PDF document",
  csv: "CSV file", tsv: "TSV file", zip: "ZIP archive", "7z": "7-Zip archive", rar: "RAR archive",
  exe: "Application", msi: "Installer", lnk: "Shortcut", url: "Internet shortcut", dll: "Library",
  svg: "SVG image", ico: "Icon", ttf: "TrueType font", otf: "OpenType font", woff2: "Web font",
  docx: "Word document", doc: "Word document", xlsx: "Excel workbook", xls: "Excel workbook",
  pptx: "PowerPoint deck", ppt: "PowerPoint deck", ini: "Settings file", iso: "Disc image",
};

/** Source files read better by language name ("Rust source", not "RS source"). */
const LANGUAGE: Record<string, string> = {
  rs: "Rust", py: "Python", js: "JavaScript", mjs: "JavaScript", cjs: "JavaScript", jsx: "JavaScript (JSX)",
  ts: "TypeScript", tsx: "TypeScript (TSX)", go: "Go", java: "Java", kt: "Kotlin", cs: "C#", c: "C", h: "C header",
  cpp: "C++", cc: "C++", hpp: "C++ header", swift: "Swift", rb: "Ruby", php: "PHP", lua: "Lua", sh: "Shell script",
  ps1: "PowerShell", bat: "Batch", cmd: "Batch", sql: "SQL", html: "HTML", htm: "HTML", css: "CSS", scss: "SCSS",
  vue: "Vue", svelte: "Svelte", dart: "Dart", zig: "Zig", scala: "Scala", r: "R", pl: "Perl", ex: "Elixir", hs: "Haskell",
};

/** "Rust", "Python"… for a file extension, or "" when there's no friendly name. */
export const languageName = (ext: string) => LANGUAGE[ext.toLowerCase()] ?? "";

export const plural = (n: number, word: string) => `${n.toLocaleString()} ${word}${n === 1 ? "" : "s"}`;

export function typeLabel(e: Entry): string {
  if (!e.isDir && EXT_LABEL[e.ext]) return EXT_LABEL[e.ext];
  const k = kindOf(e);
  if (k === "other") return e.ext ? `${e.ext.toUpperCase()} file` : "File";
  if (k === "code" && languageName(e.ext)) return `${languageName(e.ext)} source`;
  if (k === "code" || k === "data") return `${e.ext.toUpperCase()} ${k === "code" ? "source" : "file"}`;
  return KIND_LABEL[k];
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

const pad = (n: number) => String(n).padStart(2, "0");

export function formatDate(ms: number | null): string {
  if (ms == null) return "";
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** "just now", "5 min ago", "3 hours ago", "yesterday", "4 days ago", then the date. */
export function timeAgo(ms: number, now = Date.now()): string {
  const min = Math.floor((now - ms) / 60_000);
  if (min < 1) return "just now";
  if (min < 60) return `${min} min ago`;
  const hours = Math.floor(min / 60);
  if (hours < 24) return `${plural(hours, "hour")} ago`;
  const days = Math.floor(hours / 24);
  if (days === 1) return "yesterday";
  if (days < 30) return `${days} days ago`;
  return formatDate(ms).slice(0, 10);
}

/** Readable text color (black/white) for a hex background. */
export function onColor(hex: string): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex);
  if (!m) return "#fff";
  const n = parseInt(m[1], 16);
  const lin = (c: number) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  const L = 0.2126 * lin(n >> 16) + 0.7152 * lin((n >> 8) & 255) + 0.0722 * lin(n & 255);
  // Whichever text color contrasts more with the accent (WCAG ratio): white on cyan is only 2.4:1.
  const onWhite = 1.05 / (L + 0.05);
  const onDark = (L + 0.05) / (0.0056 + 0.05);
  return onDark > onWhite ? "#111" : "#fff";
}
