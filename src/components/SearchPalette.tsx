// Ctrl+K: instant whole-disk search. Ctrl+Shift+P (or ">" prefix): command palette.

import { invoke } from "@tauri-apps/api/core";
import { AppWindow, CalendarClock, CircleHelp, Command, Copy, ExternalLink, FolderOpen, FolderSearch, HardDrive, Hash, History, Info, LoaderCircle, Ruler, Search, Server, Shapes, ShieldCheck, SquareTerminal, Star, TextSearch, X } from "../lib/icons";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Dynamic, Portal } from "solid-js/web";
import FileIcon from "./FileIcon";
import { typeIcon } from "./typeIcons";
import { didYouMean, extPreview, FILE_TYPES, findType, FOLDERS, KIND_EXTS, knownType, typeTokens, withoutTypes } from "../lib/filetypes";
import "../palette.css";
import { formatDate, formatSize, KIND_LABEL, kindOf } from "../lib/format";
import { basename, isArchiveName, isRemote, isVirtual, parent, remoteParts, searchPath } from "../lib/path";
import {
  activeTab, connections, forgetSearch, navigate, newTab, openMenu, promptSaveSearch, recents, rememberSearch, removeSavedSearch, savedSearch,
  setUi, settings, toast, ui,
} from "../lib/state";
import { api } from "../lib/api";
import { copyText, openFile } from "../lib/ops";

export interface Hit {
  path: string;
  name: string;
  isDir: boolean;
  size?: number | null;
  modified?: number | null;
  score: number;
  matches: number[];
  /** Searches inside files: the first line with the text (UTF-16 offsets of the match). */
  line?: { line: number; text: string; start: number; end: number; count: number } | null;
}

interface SearchResults {
  results: Hit[];
  total: number;
  tookUs: number;
  source: "service" | "walk" | "content" | "none";
  partial: boolean;
  /** Searches inside files stop at the limit: there may be more than `total`. */
  more?: boolean;
  /** Searches inside files: how many files were read. */
  scanned?: number;
}

/** `content:"…"` with something to find: a search inside files (search/content.rs). */
const CONTENT = /(?:^|\s)content:(?:"[^"]+|[^\s"]+)/i;
const IN_FILTER = /(?:^|\s)(?:in|scope):\S/i;
/** The text a search inside files looks for, as typed. */
const contentText = (q: string) => /(?:^|\s)content:(?:"([^"]*)"?|(\S+))/i.exec(q)?.slice(1).find((x) => x != null) ?? "";

interface SearchStatus {
  source: string;
  serviceInstalled: boolean;
  serviceRunning: boolean;
  serviceOutdated: boolean;
  volumes: { root: string; entries: number; ready: boolean; progress: number }[];
  building: boolean;
}

export interface PaletteCommand {
  label: string;
  hint?: string;
  keys?: string;
  run: () => void;
}

/** Highlights matched character positions (UTF-16 offsets) in a name. */
/** Where a result lives: a WSL distro (with its Linux path), a drive (with the rest of the
 *  path), or elsewhere (network). */
function location(dir: string): { kind: "wsl" | "server" | "drive" | "other"; label: string; rest: string } {
  const wsl = /^\\\\wsl(?:\.localhost|\$)\\([^\\]+)(.*)$/i.exec(dir);
  if (wsl) return { kind: "wsl", label: wsl[1], rest: wsl[2].replace(/\\/g, "/").replace(/\/+$/, "") || "/" };
  const server = remoteParts(dir);
  if (server) return { kind: "server", label: connections().find((c) => c.id === server.id)?.name ?? server.id, rest: server.linux };
  const drive = /^([a-z]):\\?(.*)$/i.exec(dir);
  if (drive) return { kind: "drive", label: drive[1].toUpperCase(), rest: drive[2].replace(/\\+$/, "") };
  return { kind: "other", label: "", rest: dir };
}

function Highlight(props: { text: string; matches: number[] }): JSX.Element {
  const parts = createMemo(() => {
    const set = new Set(props.matches);
    const out: { s: string; hit: boolean }[] = [];
    for (let i = 0; i < props.text.length; i++) {
      const hit = set.has(i);
      const last = out[out.length - 1];
      if (last && last.hit === hit) last.s += props.text[i];
      else out.push({ s: props.text[i], hit });
    }
    return out;
  });
  return <For each={parts()}>{(p) => (p.hit ? <mark>{p.s}</mark> : <>{p.s}</>)}</For>;
}

/** Simple subsequence scorer for the command list. */
function fuzzy(label: string, q: string): { score: number; matches: number[] } | null {
  if (!q) return { score: 0, matches: [] };
  const l = label.toLowerCase();
  const sub = l.indexOf(q);
  if (sub >= 0) return { score: 1000 - sub, matches: [...Array(q.length).keys()].map((k) => k + sub) };
  const matches: number[] = [];
  let i = 0;
  for (const ch of q) {
    i = l.indexOf(ch, i);
    if (i < 0) return null;
    matches.push(i++);
  }
  return { score: 500 - (matches[matches.length - 1] - matches[0]), matches };
}

/** Type chips under the search box, in Alt+0…9 order (null = all). */
const CHIPS: (string | null)[] = [null, "folder", "image", "video", "audio", "doc", "text", "code", "archive", "app"];

/** "Images" → "images", keeping "PDFs". */
const lower = (label: string) => label.replace(/\b[A-Z][a-z]/g, (m) => m.toLowerCase());
/** A type as files in a sentence: "images", "text files", "PDFs". */
const files = (label: string) => (label.endsWith("s") ? lower(label) : `${lower(label)} files`);

const FILTERS: { token: string; value: string; text: string }[] = [
  { token: "ext:pdf,docx", value: "pdf,docx", text: "These extensions" },
  { token: "size:>100mb", value: ">100mb", text: "Bigger than 100 MB. Also <1mb, 1mb..1gb, empty" },
  { token: "modified:<7d", value: "<7d", text: "Changed in the last 7 days. Also today, >1y, 2026-01-01..2026-03-31" },
  { token: "in:C:\\dev", value: "C:\\dev", text: "Only inside this folder" },
  { token: 'content:"some text"', value: "some text", text: "Files with this text inside (in this folder, or in:)" },
  { token: "!node_modules", value: "node_modules", text: "Leave out names with this" },
  { token: "!C:\\Windows", value: "C:\\Windows", text: "Leave out a folder" },
  { token: '"two words"', value: "two words", text: "These words together" },
  { token: "*.log", value: "*.log", text: "Wildcards: * is any text, ? one character" },
  { token: "type:file", value: "file", text: "Files only (type:folder for folders)" },
];

/** A row above the results: a filter to finish, or a type to switch to. */
interface Suggestion {
  icon: typeof Search;
  main: JSX.Element;
  detail?: string;
  apply: () => void;
  /** Small buttons at the row's end (forget, save…). */
  actions?: { icon: typeof Search; title: string; run: () => void; on?: boolean }[];
  /** Section heading shown above this row. */
  heading?: string;
}

/** What the next Ctrl+K opens with (a search tab's "Edit search"). */
let seed = "";
/** "This folder" was the last scope picked. */
let lastScoped = false;
export function openSearchPalette(query = "") {
  seed = query;
  setUi("palette", null);
  queueMicrotask(() => setUi("palette", "files"));
}

/** Filters offered while their name is typed ("ty" → type:). */
const FILTER_KEYS = [
  { key: "type", icon: Shapes, text: "Images, videos, documents…" },
  { key: "ext", icon: Hash, text: "An extension, like pdf" },
  { key: "size", icon: Ruler, text: "Bigger or smaller than" },
  { key: "modified", icon: CalendarClock, text: "When it last changed" },
  { key: "in", icon: FolderSearch, text: "Only inside a folder" },
  { key: "content", icon: TextSearch, text: "Text inside files, in this folder" },
];
const SIZES: [string, string][] = [
  [">100mb", "Bigger than 100 MB"], [">1gb", "Bigger than 1 GB"], ["<1mb", "Smaller than 1 MB"],
  ["1mb..100mb", "Between 1 and 100 MB"], ["empty", "Empty files"],
];
const DATES: [string, string][] = [
  ["today", "Today"], ["yesterday", "Yesterday"], ["<7d", "In the last 7 days"], ["<30d", "In the last 30 days"],
  ["<1y", "In the last year"], [">1y", "More than a year ago"],
];
/** Extensions people look for most, first when several match. */
const COMMON_EXTS = [
  "pdf", "docx", "xlsx", "pptx", "jpg", "png", "gif", "mp4", "mp3", "zip", "txt", "md", "exe", "msi", "csv", "json",
  "py", "js", "ts", "html", "mov", "mkv", "wav", "doc", "xls", "rar", "7z", "iso", "svg", "webp", "heic", "log",
];

/** Values for a filter being typed (`type:im` → Images; `ext:p` → pdf, png…), each finishing it. */
function filterValues(key: string, value: string, complete: (token: string) => void): Suggestion[] {
  const k = key.toLowerCase();
  // Lists (`type:image,vi`) complete their last item.
  const parts = value.split(",");
  const cur = parts.pop()!.toLowerCase();
  const before = parts.length ? `${parts.join(",")},` : "";
  const finish = (v: string) => () => complete(`${key}:${before}${v}`);
  if (k === "type" || k === "kind") {
    // Folders and files first: they're what type:f is most often after.
    const types = [FOLDERS, { id: "file", label: "Files", words: [], suggest: [], exts: [] }, ...FILE_TYPES, findType("pdf")!, findType("sheet")!];
    return types
      .filter((t) => !cur || t.id.startsWith(cur) || t.words.some((w) => w.startsWith(cur)))
      .map((t) => ({
        icon: typeIcon(t.id),
        main: <><span class="palette-suggest-label">{t.id === "file" ? "Files only" : t.label}</span> <code>type:{t.id}</code></>,
        detail: extPreview(t),
        apply: finish(t.id),
      }));
  }
  if (k === "ext") {
    const all = [...new Set([...COMMON_EXTS, ...Object.values(KIND_EXTS).flat()])];
    const rank = (e: string) => (COMMON_EXTS.includes(e) ? COMMON_EXTS.indexOf(e) : 100 + e.length);
    const exts = all.filter((e) => e.startsWith(cur)).sort((a, b) => rank(a) - rank(b));
    return exts.slice(0, 8).map((e) => ({
      icon: typeIcon(kindOf({ isDir: false, ext: e })),
      main: <code>.{e}</code>,
      detail: KIND_LABEL[kindOf({ isDir: false, ext: e })],
      apply: finish(e),
    }));
  }
  const options = (list: [string, string][], icon: typeof Search) =>
    list.filter(([v]) => v.startsWith(cur)).map(([v, text]) => ({ icon, main: <><code>{key}:{v}</code> {text}</>, apply: finish(v) }));
  if (k === "size") return options(SIZES, Ruler);
  if (k === "modified" || k === "mod" || k === "date") return options(DATES, CalendarClock);
  // in: this folder, then recent ones.
  const folders = [...new Set([activeTab().p, ...recents().map((r) => r.path)])];
  return folders
    .filter((p) => !value || p.toLowerCase().includes(value.toLowerCase()))
    .slice(0, 6)
    .map((p) => ({
      icon: FolderSearch,
      main: <>{basename(p) || p}</>,
      detail: p,
      apply: () => complete(`in:${/\s/.test(p) ? `"${p}"` : p}`),
    }));
}

/** Typing "?" (or the ? button): every file type and filter, each one click away. */
function SearchHelp(props: { onType: (id: string) => void; onInsert: (token: string, value: string) => void }): JSX.Element {
  return (
    <div class="palette-help">
      <div class="palette-help-col">
        <div class="palette-help-title">File types</div>
        <For each={[FOLDERS, ...FILE_TYPES]}>
          {(t) => (
            <button class="palette-help-row" onClick={() => props.onType(t.id)}
              title={[t.words.length > 1 && `Also: ${t.words.filter((w) => w !== t.id).join(", ")}`, t.exts.join(" ")].filter(Boolean).join("\n")}>
              <Dynamic component={typeIcon(t.id)} size={14} />
              <span class="palette-help-label">{t.label}</span>
              <code>type:{t.id}</code>
              <span class="palette-help-note">{extPreview(t)}</span>
            </button>
          )}
        </For>
      </div>
      <div class="palette-help-col">
        <div class="palette-help-title">Filters</div>
        <For each={FILTERS}>
          {(f) => (
            <button class="palette-help-row palette-help-filter" onClick={() => props.onInsert(f.token, f.value)}>
              <code>{f.token}</code>
              <span class="palette-help-note">{f.text}</span>
            </button>
          )}
        </For>
        <div class="palette-help-keys">
          Filters combine: <code>report type:doc modified:&lt;30d</code><br />
          Alt+0…9 picks a type · Tab switches this folder or everywhere · <code>&gt;</code> for commands
        </div>
      </div>
    </div>
  );
}

export default function SearchPalette(props: { mode: "files" | "commands"; commands: PaletteCommand[] }) {
  const [text, setText] = createSignal(props.mode === "commands" ? ">" : seed);
  seed = "";
  // The scope picked last time (in this window), where there's a folder to search.
  const [scoped, pickScope] = createSignal(lastScoped && !isVirtual(activeTab().p));
  const setScoped = (on: boolean) => { lastScoped = on; pickScope(on); };
  const [res, setRes] = createSignal<SearchResults | null>(null);
  const [status, setStatus] = createSignal<SearchStatus | null>(null);
  const [pick, setPick] = createSignal(0);
  const [busy, setBusy] = createSignal(false);
  let input!: HTMLInputElement;
  let list!: HTMLDivElement;

  // One width for every result's name (the longest, within limits), so the paths line up.
  createEffect(on(res, () => requestAnimationFrame(() => {
    const names = [...list.querySelectorAll<HTMLElement>(".palette-item.file .palette-name")].map((n) => n.scrollWidth);
    list.style.setProperty("--name-w", `${Math.min(340, Math.max(120, ...names))}px`);
  })));

  const commandMode = () => text().startsWith(">");
  const query = () => (commandMode() ? text().slice(1).trim() : text().trim());
  const close = () => setUi("palette", null);

  // ---- file types: a chip (or a typed `type:`) keeps only images, videos… ----
  const [typeChip, setTypeChip] = createSignal<string | null>(null);
  const [helpOpen, setHelpOpen] = createSignal(false);
  const showHelp = () => !commandMode() && (helpOpen() || text().trim() === "?");
  /** What's searched: the box plus the chosen type. */
  const fullQuery = () => {
    const t = typeChip();
    return commandMode() || !t ? query() : `${query()} type:${t}`.trim();
  };
  const typed = createMemo(() => (commandMode() ? [] : typeTokens(query())));
  const typeActive = (id: string | null) => {
    const ids = typed().flatMap((t) => t.values.map((v) => findType(v)?.id));
    return id === null ? !typeChip() && !ids.some(Boolean) : typeChip() === id || ids.includes(id);
  };
  const pickType = (id: string | null) => {
    // A chip replaces any type typed in the box (and closes the help).
    const rest = withoutTypes(text().trim() === "?" ? "" : text());
    if (rest !== text()) setText(rest);
    setTypeChip(id === null || typeChip() === id ? null : id);
    setHelpOpen(false);
    input.focus();
  };
  /** "vacation videos", "image" or "text files": the type word, to offer it as a filter. */
  const wordType = createMemo(() => {
    if (commandMode() || typeChip() || typed().length || text().includes('"')) return null;
    const words = query().split(/\s+/).filter(Boolean);
    for (let i = 0; i < words.length; i++) {
      const w = words[i].toLowerCase();
      const pair = i + 1 < words.length && /^files?$/i.test(words[i + 1]) ? findType(w) : undefined;
      const type = pair ?? (FOLDERS.words.includes(w) ? FOLDERS : FILE_TYPES.find((t) => t.suggest.includes(w)));
      if (type) return { type, rest: words.filter((_, j) => j !== i && !(pair && j === i + 1)).join(" ") };
    }
    return null;
  });
  /** A `type:` word search doesn't know, and the closest one it does. */
  const typeHint = createMemo(() => {
    for (const t of typed()) for (const v of t.values) if (!knownType(v)) return { raw: t.raw, bad: v, fix: didYouMean(v) };
    return null;
  });
  const fixType = () => {
    const h = typeHint();
    if (!h) return;
    if (h.fix) setText(text().replace(h.raw, h.raw.replace(h.bad, h.fix)));
    else setHelpOpen(true);
    input.focus();
  };

  // ---- suggestions above the results: filter keys and values as they're typed ----
  /** The word being typed (the last one, unless the box ends with a space). */
  const typing = () => (/\s$/.test(text()) ? null : /(\S+)$/.exec(text())?.[1] ?? null);
  /** Puts `token` in place of the word being typed. */
  const complete = (token: string, space = true) => {
    const t = text();
    const m = /(\S+)$/.exec(t);
    const next = (m ? t.slice(0, m.index) : t) + token + (space ? " " : "");
    setText(next);
    setHelpOpen(false);
    queueMicrotask(() => { input.focus(); input.setSelectionRange(next.length, next.length); });
  };
  // ---- searches inside files: they need a folder, the one open unless an `in:` names one ----
  const contentSearch = () => !commandMode() && CONTENT.test(fullQuery());
  /** An `in:` in the box names the folder itself; "This folder" has nothing to add to it. */
  const ownFolder = () => !commandMode() && IN_FILTER.test(fullQuery());
  /** The folder a search inside files reads when no `in:` names one: the open one. */
  const contentFolder = () => (contentSearch() && !ownFolder() ? activeTab().p : null);
  const searchFolder = () => (ownFolder() ? null : scoped() ? activeTab().p : contentFolder());

  // ---- search history and saved searches ----
  /** The search as a tab sees it: "This folder" (or the folder a search inside files reads)
   *  becomes an `in:` filter. */
  const tabQuery = () => {
    const dir = searchFolder();
    return (dir && !isVirtual(dir) ? `${fullQuery()} in:"${dir}"` : fullQuery()).trim();
  };
  const openAsTab = () => {
    const q = tabQuery();
    if (!q) return;
    close();
    rememberSearch(q);
    newTab(searchPath(q));
  };
  const useQuery = (q: string) => {
    setTypeChip(null);
    // A remembered search carries its own folder; the scope picked for the next Ctrl+K stays.
    pickScope(false);
    setText(q);
    queueMicrotask(() => { input.focus(); input.setSelectionRange(q.length, q.length); });
  };
  const historyRows = (): Suggestion[] => {
    const saved = settings.savedSearches;
    const recent = settings.searchHistory.filter((q) => !savedSearch(q)).slice(0, 8);
    return [
      ...saved.map((s, i): Suggestion => ({
        heading: i === 0 ? "Saved searches" : undefined,
        icon: Star,
        main: <><span class="palette-suggest-label">{s.name}</span>{s.name !== s.query && <code>{s.query}</code>}</>,
        apply: () => useQuery(s.query),
        actions: [
          { icon: FolderSearch, title: "Open as a tab", run: () => { close(); newTab(searchPath(s.query)); } },
          { icon: X, title: "Remove from the sidebar", run: () => removeSavedSearch(s.query) },
        ],
      })),
      ...recent.map((q, i): Suggestion => ({
        heading: i === 0 ? "Recent searches" : undefined,
        icon: History,
        main: <code class="palette-recent-query">{q}</code>,
        apply: () => useQuery(q),
        actions: [
          { icon: Star, title: "Save to the sidebar", run: () => { close(); promptSaveSearch(q); } },
          { icon: X, title: "Forget this search", run: () => forgetSearch(q) },
        ],
      })),
    ];
  };

  const suggestions = createMemo((): { rows: Suggestion[]; first: boolean } => {
    if (commandMode() || showHelp()) return { rows: [], first: false };
    // An empty box offers saved searches, then recent ones.
    if (!text().trim() && !typeChip()) return { rows: historyRows(), first: true };
    const word = typing();
    const filter = word && !word.includes('"') ? /^(type|kind|ext|size|modified|mod|date|in):(.*)$/i.exec(word) : null;
    if (filter) {
      // Values for the filter being typed; they come first (Enter or Tab takes one).
      const rows = filterValues(filter[1], filter[2], complete);
      if (rows.length) return { rows, first: true };
    }
    const h = typeHint();
    if (h) return { rows: [{ icon: Info, main: <>Unknown type “{h.bad}”. {h.fix ? <>Did you mean <b>{h.fix}</b>?</> : "See the types"}</>, apply: fixType }], first: true };
    const rows: Suggestion[] = [];
    const w = word?.toLowerCase() ?? "";
    if (w.length >= 2 && !filter && !/[:!"\\]/.test(w)) {
      for (const k of FILTER_KEYS) if (k.key.startsWith(w)) rows.push({ icon: k.icon, main: <><code>{k.key}:</code> {k.text}</>, apply: () => complete(`${k.key}:`, false) });
    }
    const s = wordType();
    if (s) {
      rows.push({
        icon: typeIcon(s.type.id),
        main: s.rest ? <>Only <b>{files(s.type.label)}</b> named “{s.rest}”</> : <>All <b>{files(s.type.label)}</b></>,
        detail: extPreview(s.type),
        apply: () => { setText(s.rest); setTypeChip(s.type.id); input.focus(); },
      });
    }
    // Last, so the first result stays picked: every match as a tab.
    const r = res();
    if (r?.results.length) {
      rows.push({
        icon: FolderSearch,
        // A search tab lists up to 2,000 (FileView's SEARCH_TAB_LIMIT).
        main: r.more ? <>Open every match in a tab</>
          : r.total > 2000 ? <>Open the first 2,000 of {r.total.toLocaleString()} matches in a tab</>
          : r.total === 1 ? <>Open the match in a tab</> : <>Open all {r.total.toLocaleString()} matches in a tab</>,
        detail: "Ctrl+Shift+Enter",
        apply: openAsTab,
      });
    }
    return { rows, first: false };
  });

  /** Results are numbered after the suggestions. */
  const offset = () => suggestions().rows.length;
  /** The suggestion Tab takes: a completion (picked, or the first one). Saved and recent
   *  searches and "Open … in a tab" aren't, so Tab there switches the scope as the footer says. */
  const tabRow = () => {
    const rows = suggestions().rows;
    const s = rows[pick() < offset() ? pick() : 0];
    return s && !s.actions && s.apply !== openAsTab ? s : null;
  };
  /** Adds a filter from the help to the box, its value selected to type over. */
  const insertFilter = (token: string, value: string) => {
    const base = text().trim() === "?" ? "" : text().trimEnd();
    const next = base ? `${base} ${token}` : token;
    setText(next);
    setHelpOpen(false);
    queueMicrotask(() => {
      input.focus();
      const at = next.lastIndexOf(value);
      input.setSelectionRange(at, at + value.length);
    });
  };

  // ---- file search: always run the latest query, one request at a time ----
  let inflight = false;
  /** The request running reads files (it can take a while: a newer query stops it). */
  let inflightContent = false;
  let queued: string | null = null;
  onCleanup(() => { if (inflightContent) invoke("search_cancel").catch(() => {}); });
  const frecency = () => {
    const now = Date.now();
    const out: Record<string, number> = {};
    for (const r of recents()) out[r.path] = Math.min(1, r.count / 10) * 0.5 + Math.max(0, 1 - (now - r.lastOpened) / (14 * 86_400_000)) * 0.5;
    return out;
  };
  const run = async (q: string) => {
    if (inflight) {
      if (inflightContent && queued === null) invoke("search_cancel").catch(() => {});
      queued = q;
      return;
    }
    inflight = true;
    inflightContent = CONTENT.test(q);
    setBusy(true);
    try {
      const r = await invoke<SearchResults>("search_query", { query: q, limit: 60, scope: searchFolder(), frecency: frecency() });
      if (q === fullQuery() && !commandMode()) setRes(r);
    } catch (e) {
      toast(`Search failed: ${e}`, "error");
    } finally {
      inflight = false;
      inflightContent = false;
      setBusy(false);
      if (queued !== null) { const next = queued; queued = null; if (next === fullQuery()) run(next); }
    }
  };
  createEffect(on([fullQuery, scoped, commandMode], ([q, , cmd]) => {
    if (cmd) return;
    if (!q || text().trim() === "?") { setRes(null); return; }
    run(q);
  }));

  // ---- index status (progress while building, service offer) ----
  onMount(() => {
    const poll = () => invoke<SearchStatus>("search_status").then(setStatus).catch(() => {});
    poll();
    const timer = setInterval(() => { if (status()?.building || !status()) poll(); }, 700);
    onCleanup(() => clearInterval(timer));
    queueMicrotask(() => { input.focus(); input.setSelectionRange(text().length, text().length); });
  });
  const indexed = () => status()?.volumes.reduce((a, v) => a + v.entries, 0) ?? 0;
  const progress = () => {
    const v = status()?.volumes ?? [];
    return v.length ? v.reduce((a, x) => a + x.progress, 0) / v.length : 0;
  };

  // ---- commands ----
  const commandHits = createMemo(() => {
    if (!commandMode()) return [];
    const q = query().toLowerCase();
    return props.commands
      .map((c) => ({ c, m: fuzzy(c.label, q) }))
      .filter((x) => x.m)
      .sort((a, b) => b.m!.score - a.m!.score)
      .slice(0, 40);
  });

  const count = () => (commandMode() ? commandHits().length : offset() + (res()?.results.length ?? 0));
  // The first result is picked, unless a filter is being finished (or there's nothing else).
  createEffect(on([suggestions, res], ([s, r]) => setPick(s.first || !r?.results.length ? 0 : s.rows.length)));

  const reveal = (h: Hit) => {
    const dir = h.isDir ? h.path : parent(h.path);
    if (dir) navigate(activeTab(), dir, h.isDir ? {} : { select: h.path });
  };
  const activate = (i: number, how: "open" | "tab" | "reveal") => {
    if (commandMode()) {
      const c = commandHits()[i]?.c;
      if (c) { close(); c.run(); }
      return;
    }
    if (i < offset()) return suggestions().rows[i]?.apply();
    const h = res()?.results[i - offset()];
    if (!h) return;
    close();
    rememberSearch(tabQuery());
    // Archives open as folders, like in the list: Windows would hand a zip to Explorer (and,
    // with Ferret as the default file manager, on to a new Ferret window, slowly).
    const archive = !h.isDir && isArchiveName(h.path) && !isRemote(h.path);
    if (how === "tab") newTab(h.isDir || archive ? h.path : parent(h.path) ?? h.path);
    else if (how === "reveal" || h.isDir) reveal(h);
    else if (archive) navigate(activeTab(), h.path);
    else openFile({ path: h.path, inArchive: false });
  };

  createEffect(on(pick, (i) => {
    list?.querySelector<HTMLElement>(`[data-i="${i}"]`)?.scrollIntoView({ block: "nearest" });
  }));

  const onKey = (e: KeyboardEvent) => {
    e.stopPropagation();
    const n = count();
    if (e.altKey && /^[0-9]$/.test(e.key) && !commandMode()) { e.preventDefault(); pickType(CHIPS[+e.key]); }
    else if (e.key === "Escape") { e.preventDefault(); if (helpOpen()) setHelpOpen(false); else if (!ui.menu) close(); }
    else if (e.key === "Backspace" && typeChip() && input.selectionStart === 0 && input.selectionEnd === 0) { e.preventDefault(); setTypeChip(null); }
    else if (e.key === "Tab" && tabRow()) { e.preventDefault(); tabRow()!.apply(); }
    else if (e.key === "ArrowDown") { e.preventDefault(); if (n) setPick((p) => (p + 1) % n); }
    else if (e.key === "ArrowUp") { e.preventDefault(); if (n) setPick((p) => (p - 1 + n) % n); }
    else if (e.key === "PageDown") { e.preventDefault(); setPick((p) => Math.min(n - 1, p + 8)); }
    else if (e.key === "PageUp") { e.preventDefault(); setPick((p) => Math.max(0, p - 8)); }
    else if (e.key === "Tab" && !commandMode()) { e.preventDefault(); setScoped(!scoped()); }
    else if (e.key === "Enter" && e.ctrlKey && e.shiftKey && !commandMode()) { e.preventDefault(); openAsTab(); }
    else if (e.key === "Enter") {
      e.preventDefault();
      activate(pick(), e.ctrlKey ? "tab" : e.altKey || e.shiftKey ? "reveal" : "open");
    }
  };

  const resultMenu = (e: MouseEvent, i: number, h: Hit) => {
    e.preventDefault();
    setPick(i);
    const copy = (text: string, what: string) => copyText(text).then(() => toast(`${what} copied`));
    openMenu(e.clientX, e.clientY, [
      { label: h.isDir ? "Open folder" : "Open", icon: ExternalLink, shortcut: "Enter", action: () => activate(i, "open") },
      { label: h.isDir ? "Show in parent folder" : "Show in folder", icon: FolderOpen, shortcut: "Alt+Enter", action: () => {
        close();
        const dir = parent(h.path);
        if (dir) navigate(activeTab(), dir, { select: h.path });
      } },
      { label: "Open in new tab", icon: FolderOpen, shortcut: "Ctrl+Enter", action: () => activate(i, "tab") },
      ...(h.isDir || isRemote(h.path) ? [] : [{ label: "Open with…", icon: AppWindow, action: () => { close(); api.openWithDialog(h.path).catch((err) => toast(String(err), "error")); } }]),
      { separator: true },
      // A server's file: its path there.
      { label: "Copy path", icon: Hash, action: () => copy(remoteParts(h.path)?.linux ?? h.path, "Path") },
      { label: "Copy name", icon: Copy, action: () => copy(h.name, "Name") },
      { separator: true },
      { label: "Properties", icon: Info, action: () => { close(); setUi("properties", [h.path]); } },
    ]);
  };

  const installService = async () => {
    // An installed service updates itself (no prompt; one from an older Ferret asks once).
    const update = !!status()?.serviceInstalled;
    try {
      await invoke("search_install_service");
      toast(update ? "Updating the drive index service…" : "Installing the Ferret indexer: approve the Windows prompt");
      setTimeout(() => invoke<SearchStatus>("search_status").then(setStatus).catch(() => {}), 4000);
    } catch (e) {
      toast(`Couldn't install the indexer: ${e}`, "error");
    }
  };

  return (
    <Portal>
      <div class="palette-backdrop" onMouseDown={(e) => e.target === e.currentTarget && close()}>
        <div class="palette" role="dialog" aria-label={commandMode() ? "Command palette" : "Search"}>
          <div class="palette-input">
            <Show when={commandMode()} fallback={<Search size={17} />}><Command size={17} /></Show>
            <Show when={!commandMode() && typeChip()}>
              {(id) => (
                <button class="palette-type-pill" title="Show all types (Backspace)" onClick={() => { setTypeChip(null); input.focus(); }}>
                  <Dynamic component={typeIcon(id())} size={13} />{findType(id())?.label ?? id()}<X size={12} />
                </button>
              )}
            </Show>
            <input
              ref={input}
              value={text()}
              placeholder={commandMode() ? "Type a command…" : scoped() ? `Search in ${basename(activeTab().p)}…` : "Search files and folders everywhere…"}
              spellcheck={false}
              onInput={(e) => { setText(e.currentTarget.value); setHelpOpen(false); }}
              onKeyDown={onKey}
            />
            <Show when={busy()}><LoaderCircle size={15} class="spin muted" /></Show>
            <Show when={!commandMode() && tabQuery()}>
              <button class="icon-btn tiny" classList={{ on: !!savedSearch(tabQuery()) }}
                title={savedSearch(tabQuery()) ? "Saved in the sidebar (rename)" : "Save this search to the sidebar"}
                onClick={() => { const q = tabQuery(); close(); promptSaveSearch(q); }}>
                <Star size={15} fill={savedSearch(tabQuery()) ? "currentColor" : "none"} />
              </button>
            </Show>
            <Show when={!commandMode()}>
              <button class="icon-btn tiny palette-help-btn" classList={{ on: showHelp() }} title="File types and filters (?)"
                onClick={() => { const open = showHelp(); if (text().trim() === "?") setText(""); setHelpOpen(!open); input.focus(); }}>
                <CircleHelp size={15} />
              </button>
              <button class="palette-scope" classList={{ on: scoped() || !!contentFolder() || ownFolder() }} disabled={!!contentFolder() || ownFolder()}
                title={ownFolder() ? "The in: filter picks the folder" : contentFolder() ? "Searches inside files read this folder (or the one named with in:)" : "Toggle scope (Tab)"}
                onClick={() => { setScoped(!scoped()); input.focus(); }}>
                {/* A search inside files reads the open folder even when "Everywhere" is picked. */}
                {ownFolder() ? <><FolderSearch size={13} /> in: folder</>
                  : scoped() || contentFolder() ? <><FolderSearch size={13} /> This folder</> : <><HardDrive size={13} /> Everywhere</>}
              </button>
            </Show>
            <button class="icon-btn tiny" title="Close (Esc)" onClick={close}><X size={14} /></button>
          </div>
          <Show when={!commandMode()}>
            <div class="palette-types" role="toolbar" aria-label="File type">
              <For each={CHIPS}>
                {(id, i) => {
                  const t = id ? findType(id) : undefined;
                  return (
                    <button classList={{ on: typeActive(id) }} aria-pressed={typeActive(id)} onClick={() => pickType(id)}
                      title={`${t ? t.label + (t.exts.length ? `: ${extPreview(t, 8)}` : "") : "All types"} (Alt+${i()})`}>
                      <Show when={id}><Dynamic component={typeIcon(id!)} size={13} /></Show>
                      {t?.label ?? "All"}
                    </button>
                  );
                }}
              </For>
            </div>
          </Show>

          <div class="palette-list" ref={list}>
            <Show when={commandMode()} fallback={
              <Show when={!showHelp()} fallback={<SearchHelp onType={pickType} onInsert={insertFilter} />}>
              <For each={suggestions().rows}>
                {(s, i) => (
                  <>
                    <Show when={s.heading}><div class="palette-heading">{s.heading}</div></Show>
                    <button class="palette-suggest" classList={{ active: i() === pick() }} data-i={i()}
                      onMouseMove={() => setPick(i())} onClick={() => s.apply()}>
                      <Dynamic component={s.icon} size={15} />
                      <span class="palette-suggest-main">{s.main}</span>
                      <span class="palette-suggest-exts">{s.detail}</span>
                      <For each={s.actions}>
                        {(a) => (
                          <span class="palette-suggest-act" role="button" title={a.title}
                            onClick={(e) => { e.stopPropagation(); a.run(); input.focus(); }}>
                            <Dynamic component={a.icon} size={13} />
                          </span>
                        )}
                      </For>
                      <Show when={i() === pick() && tabRow() === s}><kbd>Tab</kbd></Show>
                    </button>
                  </>
                )}
              </For>
              <Show when={res()?.results.length} fallback={
                <div class="palette-empty">
                  <Show when={fullQuery()} fallback={<span>Type to search {indexed() ? `${indexed().toLocaleString()} indexed items` : "your drives"}, or pick a type above. <code>?</code> shows every filter · <code>&gt;</code> for commands</span>}>
                    <span>{contentSearch() ? (
                      busy() ? `Reading the files in ${basename(searchFolder() ?? "") || "this folder"}…`
                      : searchFolder() && isVirtual(searchFolder()!) ? <>Searching inside files needs a folder: open one, or add <code>in:C:\folder</code></>
                      : searchFolder() && isRemote(searchFolder()!) ? "Searching inside files doesn't work on servers yet (names do, with search on in the connection's settings)"
                      : `No file in ${basename(searchFolder() ?? "") || "the folder"} contains “${contentText(fullQuery())}”`)
                      : status()?.building ? "Still indexing, so more results will appear…"
                      : !query() && typeChip() ? `No ${files(findType(typeChip()!)?.label ?? "files")} found`
                      : `Nothing matches “${query()}”${typeChip() ? ` in ${files(findType(typeChip()!)?.label ?? "")}` : ""}`}</span>
                  </Show>
                </div>
              }>
                <For each={res()!.results}>
                  {(h, i) => { const n = () => i() + offset(); return (
                    <div class="palette-item file" role="option" aria-selected={n() === pick()} classList={{ active: n() === pick(), "has-line": !!h.line }} data-i={n()}
                      onMouseMove={() => setPick(n())}
                      onClick={(e) => activate(n(), e.ctrlKey ? "tab" : e.altKey ? "reveal" : "open")}
                      onAuxClick={(e) => e.button === 1 && activate(n(), "tab")}
                      onContextMenu={(e) => resultMenu(e, n(), h)}
                      title={h.path}>
                      <FileIcon entry={{ isDir: h.isDir, ext: h.isDir ? "" : (h.name.split(".").pop() ?? "").toLowerCase(), isSymlink: false }} size={16} />
                      <span class="palette-name"><Highlight text={h.name} matches={h.matches} /></span>
                      <span class="palette-path">
                        {(() => {
                          const loc = location(parent(h.path) ?? h.path);
                          return (
                            <>
                              <Show when={loc.kind === "wsl"}>
                                <span class="loc-wsl" title={`In WSL (${loc.label})`}><SquareTerminal size={11} />{loc.label}</span>
                              </Show>
                              <Show when={loc.kind === "server"}>
                                <span class="loc-wsl" title={`On ${loc.label}`}><Server size={11} />{loc.label}</span>
                              </Show>
                              <Show when={loc.kind === "drive"}>
                                <span class="drive-letter" title={`On drive ${loc.label}:`}>{loc.label}</span>
                              </Show>
                              {/* Left-to-right marks keep "/home/me" from reading "home/me/" in the right-to-left (start-truncating) box. */}
                              <span class="palette-path-text">{`‎${loc.rest}‎`}</span>
                            </>
                          );
                        })()}
                      </span>
                      <span class="palette-meta">{h.isDir ? "" : h.size != null ? formatSize(h.size) : ""}{h.modified ? ` · ${formatDate(h.modified).slice(0, 10)}` : ""}</span>
                      <button class="palette-reveal" title={h.isDir ? "Open folder" : "Show in folder (Alt+Enter)"}
                        onClick={(e) => { e.stopPropagation(); activate(n(), "reveal"); }}>
                        <FolderOpen size={14} /><span>{h.isDir ? "Open" : "Show in folder"}</span>
                      </button>
                      {/* Searches inside files: the line with the text, under the name. */}
                      <Show when={h.line}>
                        {(m) => (
                          <span class="palette-line" title={m().count > 1 ? `${m().count.toLocaleString()} matches in this file` : undefined}>
                            <span class="palette-line-no">{m().line}</span>
                            <span class="palette-line-text">{m().text.slice(0, m().start)}<mark>{m().text.slice(m().start, m().end)}</mark>{m().text.slice(m().end)}</span>
                            <Show when={m().count > 1}><span class="palette-line-more">+{(m().count - 1).toLocaleString()}</span></Show>
                          </span>
                        )}
                      </Show>
                    </div>
                  ); }}
                </For>
              </Show>
              </Show>
            }>
              <For each={commandHits()} fallback={<div class="palette-empty">No matching command</div>}>
                {(x, i) => (
                  <button class="palette-item cmd" classList={{ active: i() === pick() }} data-i={i()}
                    onMouseMove={() => setPick(i())} onClick={() => activate(i(), "open")}>
                    <Command size={14} class="muted" />
                    <span class="palette-name"><Highlight text={x.c.label} matches={x.m!.matches} /></span>
                    <span class="palette-path">{x.c.hint ?? ""}</span>
                    <Show when={x.c.keys}><kbd>{x.c.keys}</kbd></Show>
                  </button>
                )}
              </For>
            </Show>
          </div>

          <div class="palette-foot">
            <Show when={!commandMode()} fallback={<span>↑↓ choose · Enter run · Esc close</span>}>
              <span>
                <Show when={res()} fallback={<>↑↓ choose · Enter open · <b>Alt+Enter show in folder</b> · Ctrl+Enter new tab · ? filters · Tab scope</>}>
                  {(r) => r().source === "content"
                    ? <>{r().total.toLocaleString()}{r().more ? "+" : ""} {r().total === 1 && !r().more ? "file has" : "files have"} it · {(r().scanned ?? 0).toLocaleString()} files read ·{(r().tookUs / 1000).toFixed(r().tookUs < 10_000 ? 1 : 0)} ms</>
                    : <>{r().total === 1 ? "1 match" : `${r().total.toLocaleString()} matches`} · {(r().tookUs / 1000).toFixed(r().tookUs < 10_000 ? 1 : 0)} ms{r().partial ? " · indexing" : ""}</>}
                </Show>
              </span>
              <span class="palette-status">
                <Show when={status()?.building}>
                  <LoaderCircle size={12} class="spin" /> Indexing {Math.round(progress() * 100)}%
                </Show>
                <Show when={status() && !status()!.serviceInstalled && !status()!.building}>
                  <button class="palette-install" onClick={installService} title="Reads the NTFS file table for instant, always-current results across all drives. Needs one admin approval.">
                    <ShieldCheck size={12} /> Enable instant drive index
                  </button>
                </Show>
                <Show when={status()?.serviceRunning}>
                  <Show when={status()!.serviceOutdated} fallback={<><ShieldCheck size={12} /> Drive index</>}>
                    <button class="palette-install" onClick={installService} title="This version of Ferret ships a newer drive index service. It updates itself; one installed by an older Ferret needs one admin approval, once.">
                      <ShieldCheck size={12} /> Update drive index
                    </button>
                  </Show>
                </Show>
              </span>
            </Show>
          </div>
        </div>
      </div>
    </Portal>
  );
}

export const paletteOpen = () => !!ui.palette;
