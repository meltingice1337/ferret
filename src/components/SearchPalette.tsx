// Ctrl+K: instant whole-disk search. Ctrl+Shift+P (or ">" prefix): command palette.

import { invoke } from "@tauri-apps/api/core";
import { AppWindow, CalendarClock, CircleHelp, Command, Copy, ExternalLink, FolderOpen, FolderSearch, HardDrive, Hash, History, Info, LoaderCircle, Ruler, Search, Server, Shapes, ShieldCheck, SquareTerminal, Star, TextSearch, X } from "../lib/icons";
import { batch, createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Dynamic, Portal } from "solid-js/web";
import FileIcon from "./FileIcon";
import { typeIcon } from "./typeIcons";
import { didYouMean, extPreview, FILE_TYPES, findType, FOLDERS, KIND_EXTS, knownType, typeTokens, withoutTypes } from "../lib/filetypes";
import "../palette.css";
import { formatDate, formatSize, KIND_LABEL, kindOf } from "../lib/format";
import { basename, isArchiveName, isRemote, isVirtual, parent, remoteParts, searchPath } from "../lib/path";
import { awaitingValue, badFilter, inFolderName, inFolderPath, isLooseMatch, mergeNear, nameColumn, splitName, withoutFilters } from "../lib/searchquery";
import {
  activeTab, connections, forgetSearch, navigate, newTab, openMenu, promptSaveSearch, recents, rememberSearch, removeSavedSearch, savedSearch,
  setUi, settings, toast, ui,
} from "../lib/state";
import { api } from "../lib/api";
import { beginDrag, drag } from "../lib/drag";
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
  /** "content-unsupported": inside files on a server that only allows SFTP. */
  source: "service" | "walk" | "content" | "content-unsupported" | "none";
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

/** A result's name, shortening in the middle. A name that matches whole has nothing to pick out. */
function ResultName(props: { text: string; matches: number[] }): JSX.Element {
  const parts = createMemo(() => splitName(props.text));
  const matches = () => (props.matches.length >= props.text.length ? [] : props.matches);
  return (
    <>
      <span class="palette-name-start"><Highlight text={parts()[0]} matches={matches()} /></span>
      <Show when={parts()[1]}>
        <span class="palette-name-end"><Highlight text={parts()[1]} matches={matches().map((m) => m - parts()[0].length)} /></span>
      </Show>
    </>
  );
}

/** What a query should look like in a sentence: it may be pasted, and very long. */
const quoted = (q: string) => `“${q.length > 60 ? `${q.slice(0, 60)}…` : q}”`;

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
  { token: "dm:2026-03-04", value: "2026-03-04", text: "Changed on this day" },
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
  /** Tab takes this row: it finishes what's being typed (and shows the Tab badge). */
  completes?: boolean;
  /** The filter value this row fills in: typed out in full, it has nothing left to offer. */
  fills?: string;
}

/** What the next Ctrl+K opens with (a search tab's "Edit search"). */
let seed = "";
/** "This folder" was the last scope picked. */
let lastScoped = false;
/** The search closed with Esc: the next Ctrl+K, soon after, brings it back (typing replaces it). */
let dismissed: { query: string; at: number } | null = null;
const DISMISSED_FOR_MS = 8000;
/** Results listed first from the open folder. */
const NEAR_LIMIT = 5;
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
function filterValues(key: string, value: string, complete: (token: string, space?: boolean) => void): Suggestion[] {
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
        completes: true,
        fills: t.id,
      }));
  }
  if (k === "content") {
    return [{
      icon: TextSearch,
      main: <><code>content:"…"</code> Text inside files, in {basename(activeTab().p) || "this folder"}</>,
      apply: () => complete('content:"', false),
      completes: true,
    }];
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
      completes: true,
      fills: e,
    }));
  }
  const options = (list: [string, string][], icon: typeof Search) =>
    list.filter(([v]) => v.startsWith(cur)).map(([v, text]) => ({ icon, main: <><code>{key}:{v}</code> {text}</>, apply: finish(v), completes: true, fills: v }));
  if (k === "size") return options(SIZES, Ruler);
  if (k === "modified" || k === "mod" || k === "date" || k === "dm") return options(DATES, CalendarClock);
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
      completes: true,
      fills: p,
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
          Alt+0…9 picks a type · Tab switches this folder or everywhere<br />
          Ctrl+S saves · Ctrl+Shift+C copies the path · Shift+F10 opens the menu<br />
          Shift+Tab reaches the buttons · <code>&gt;</code> for commands
        </div>
      </div>
    </div>
  );
}

export default function SearchPalette(props: { mode: "files" | "commands"; commands: PaletteCommand[] }) {
  const restored = props.mode === "files" && !seed && dismissed && Date.now() - dismissed.at < DISMISSED_FOR_MS ? dismissed.query : "";
  const [text, setText] = createSignal(props.mode === "commands" ? ">" : seed || restored);
  seed = "";
  dismissed = null;
  // The scope picked last time (in this window), where there's a folder to search.
  const [scoped, pickScope] = createSignal(lastScoped && !isVirtual(activeTab().p));
  const setScoped = (on: boolean) => { lastScoped = on; pickScope(on); };
  const [res, setRes] = createSignal<SearchResults | null>(null);
  /** Hits from the open folder, listed first (searching everywhere, it's easy to lose them). */
  const [near, setNear] = createSignal<Hit[]>([]);
  /** The query `res` answers: while it isn't the one in the box, `res` says nothing about it. */
  const [answered, setAnswered] = createSignal<string | null>(null);
  const [failed, setFailed] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal<SearchStatus | null>(null);
  const [pick, setPick] = createSignal(0);
  const [busy, setBusy] = createSignal(false);
  /** A search that's taking long enough to be worth saying so (most take milliseconds). */
  const [slow, setSlow] = createSignal(false);
  let input!: HTMLInputElement;
  let list!: HTMLDivElement;
  let panel!: HTMLDivElement;

  /** Near hits, then real matches, then scattered-letter ones, each group under its heading. */
  const shown = createMemo(() => {
    const q = answered() ?? "";
    const rows = mergeNear(near(), res()?.results ?? []).map((r) => ({ ...r, loose: !r.near && isLooseMatch(r.hit.path, q) }));
    const ordered = [...rows.filter((r) => !r.loose), ...rows.filter((r) => r.loose)];
    const nears = near().length;
    return ordered.map((r, i) => ({
      ...r,
      heading: r.near && i === 0 ? `In ${basename(activeTab().p)}`
        : r.loose && !ordered[i - 1]?.loose ? "Loose matches"
        : !r.near && !r.loose && nears > 0 && i === nears ? "Everywhere" : undefined,
    }));
  });
  /** How many of the results shown are only loose matches. */
  const looseCount = () => shown().filter((r) => r.loose).length;

  // One width for every result's name, so the paths line up.
  createEffect(on(shown, () => requestAnimationFrame(() => {
    const names = [...list.querySelectorAll<HTMLElement>(".palette-item.file .palette-name")]
      .map((n) => [...n.children].reduce((w, part) => w + part.scrollWidth, 0));
    list.style.setProperty("--name-w", `${nameColumn(names)}px`);
  })));

  const commandMode = () => text().startsWith(">");
  const query = () => (commandMode() ? text().slice(1).trim() : text().trim());
  const close = () => setUi("palette", null);
  /** Closed without using a result: the search is kept for a moment. */
  const dismiss = () => {
    const q = fullQuery();
    if (!commandMode() && q && q !== "?") dismissed = { query: q, at: Date.now() };
    close();
  };
  // Back from a drag that went nowhere: the keyboard is the search box's again.
  createEffect(on(drag, (now, was) => { if (was && !now) input?.focus(); }, { defer: true }));

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
  /** Searching everywhere from a real folder: that folder's hits are listed first. A whole drive
   *  isn't "near" anything. */
  const nearFolder = () => {
    const p = activeTab().p;
    return scoped() || ownFolder() || contentSearch() || isVirtual(p) || isRemote(p) || /^[a-z]:\\?$/i.test(p) ? null : p;
  };
  /** What the search is limited to, for a sentence. */
  const whereSearched = () => {
    if (ownFolder()) return ` in ${inFolderName(fullQuery())}`;
    return scoped() ? ` in ${basename(activeTab().p)}` : "";
  };
  /** Save a search: a cancelled dialog comes back to the search it was opened from. */
  const askToSave = (q: string, back: string) => {
    close();
    promptSaveSearch(q).then((saved) => { if (!saved) openSearchPalette(back); });
  };

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
          { icon: Star, title: "Save to the sidebar (Ctrl+S)", run: () => askToSave(q, "") },
          { icon: X, title: "Forget this search (Delete)", run: () => forgetSearch(q) },
        ],
      })),
    ];
  };

  const suggestions = createMemo((): { rows: Suggestion[]; first: boolean } => {
    if (commandMode() || showHelp()) return { rows: [], first: false };
    // An empty box offers saved searches, then recent ones.
    if (!text().trim() && !typeChip()) return { rows: historyRows(), first: true };
    const word = typing();
    const filter = word && !word.includes('"') ? /^(type|kind|ext|size|modified|mod|dm|date|in|content):(.*)$/i.exec(word) : null;
    if (filter) {
      // Values for the filter being typed; they come first (Enter or Tab takes one). A value
      // already typed out in full has nothing to finish: Enter is for the results then.
      const rows = filterValues(filter[1], filter[2], complete);
      const last = filter[2].split(",").pop()!.replace(/"/g, "").toLowerCase();
      if (rows.length && !rows.some((r) => r.fills?.toLowerCase() === last)) return { rows, first: true };
    }
    const h = typeHint();
    if (h) return { rows: [{ icon: Info, main: <>Unknown type “{h.bad}”. {h.fix ? <>Did you mean <b>{h.fix}</b>?</> : "See the types"}</>, apply: fixType, completes: !!h.fix }], first: true };
    const rows: Suggestion[] = [];
    // Search ignores a value it can't read, so the results aren't limited the way it looks.
    const bad = badFilter(query());
    if (bad) {
      rows.push({
        icon: Info,
        main: <>Ignoring <code>{bad.raw}</code>: not a {bad.kind === "size" ? "size" : "date"}. Try <code>{bad.example}</code></>,
        apply: () => { setText(text().replace(bad.raw, bad.example)); input.focus(); },
      });
    }
    const w = word?.toLowerCase() ?? "";
    if (w.length >= 2 && !filter && !/[:!"\\]/.test(w)) {
      for (const k of FILTER_KEYS) if (k.key.startsWith(w)) rows.push({ icon: k.icon, main: <><code>{k.key}:</code> {k.text}</>, apply: () => complete(`${k.key}:`, false), completes: true });
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
      // Search ranks loose matches last, so once any show, every real match above them has too.
      const firm = shown().length - looseCount();
      const loose = looseCount() && !r.more ? `, ${(r.total - firm).toLocaleString()} of them loose` : "";
      rows.push({
        icon: FolderSearch,
        // A search tab lists up to 2,000 (FileView's SEARCH_TAB_LIMIT).
        main: r.more ? <>Open every match in a tab</>
          : r.total > 2000 ? <>Open the first 2,000 of {r.total.toLocaleString()} matches in a tab</>
          : r.total === 1 ? <>Open the match in a tab</> : <>Open all {r.total.toLocaleString()} matches in a tab{loose}</>,
        detail: "Ctrl+Shift+Enter",
        apply: openAsTab,
      });
    }
    return { rows, first: false };
  });

  /** Results are numbered after the suggestions. */
  const offset = () => suggestions().rows.length;
  /** The suggestion Tab takes: a completion, the picked one or else the first. It always shows
   *  the Tab badge. Everything else (recent searches, "Open … in a tab", a suggested rewrite of
   *  what's typed) isn't one, so Tab there switches the scope as the footer says. */
  const tabRow = () => {
    const rows = suggestions().rows;
    const picked = rows[pick()];
    return picked?.completes ? picked : rows.find((s) => s.completes) ?? null;
  };
  /** Tab can switch the scope: an `in:` or a search inside files has already picked the folder. */
  const canSwitchScope = () => !commandMode() && !ownFolder() && !contentFolder();
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
    setFailed(null);
    const slowTimer = setTimeout(() => setSlow(true), 120);
    try {
      const dir = nearFolder();
      const [r, hits] = await Promise.all([
        invoke<SearchResults>("search_query", { query: q, limit: 60, scope: searchFolder(), frecency: frecency() }),
        // Only a nicety on top of the search itself: if it fails, the list just doesn't have it.
        dir ? invoke<SearchResults>("search_query", { query: q, limit: NEAR_LIMIT, scope: dir, frecency: frecency() }).catch(() => null) : null,
      ]);
      // No matches at all and hits in the folder: a query with nothing search can read lists the
      // whole folder when it's scoped, and those aren't matches.
      // Loose matches stay with the other loose ones, instead of leading the list as "near".
      if (q === fullQuery() && !commandMode()) batch(() => { setRes(r); setNear(r.total ? (hits?.results ?? []).filter((h) => !isLooseMatch(h.path, q)) : []); setAnswered(q); });
    } catch (e) {
      if (q === fullQuery()) batch(() => { setRes(null); setNear([]); setAnswered(q); setFailed(String(e)); });
    } finally {
      clearTimeout(slowTimer);
      inflight = false;
      inflightContent = false;
      batch(() => { setBusy(false); setSlow(false); });
      if (queued !== null) { const next = queued; queued = null; if (next === fullQuery()) run(next); }
    }
  };
  createEffect(on([fullQuery, scoped, commandMode], ([q, , cmd]) => {
    if (cmd) return;
    if (!q || text().trim() === "?") { batch(() => { setRes(null); setNear([]); setAnswered(null); setFailed(null); }); return; }
    run(q);
  }));
  /** The results on screen are the answer to what's in the box. */
  const settled = () => !busy() && answered() === fullQuery();
  /** What's in the box can't be judged yet: a filter with no value, or only a value search
   *  ignores. No count or "nothing matches" for those. */
  const noVerdict = () => !shown().length && (awaitingValue(fullQuery()) || (!!badFilter(query()) && !withoutFilters(query())));
  /** An `in:` folder that can't be opened, once a search in it found nothing. */
  const [missing, setMissing] = createSignal<string | null>(null);
  createEffect(on([settled, shown], ([done, rows]) => {
    const dir = done && !rows.length && !contentSearch() ? inFolderPath(fullQuery()) : null;
    if (!dir || awaitingValue(fullQuery())) { setMissing(null); return; }
    const q = fullQuery();
    api.listDir(dir).then(() => setMissing(null), () => { if (q === fullQuery()) setMissing(dir); });
  }));

  // ---- index status (progress while building, service offer) ----
  onMount(() => {
    const poll = () => invoke<SearchStatus>("search_status").then(setStatus).catch(() => {});
    poll();
    const timer = setInterval(() => { if (status()?.building || !status()) poll(); }, 700);
    onCleanup(() => clearInterval(timer));
    queueMicrotask(() => { input.focus(); input.setSelectionRange(restored ? 0 : text().length, text().length); });
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

  const count = () => (commandMode() ? commandHits().length : offset() + shown().length);
  // The first result is picked, unless a filter is being finished (or there's nothing else).
  createEffect(on([suggestions, shown], ([s, r]) => setPick(s.first || !r.length ? 0 : s.rows.length)));

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
    const h = shown()[i - offset()]?.hit;
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
  // A result's menu closed without closing the search: the keyboard is the search box's again.
  createEffect(on(() => ui.menu, (now, was) => { if (was && !now) input?.focus(); }, { defer: true }));

  /** The result the arrow keys are on. */
  const pickedHit = () => (commandMode() ? undefined : shown()[pick() - offset()]?.hit);
  const pathOf = (h: Hit) => remoteParts(h.path)?.linux ?? h.path;
  const copied = (text: string, what: string) => copyText(text).then(() => toast(`${what} copied`));

  /** Tab belongs to the search box (it completes, or switches scope), so the buttons around it
   *  are reached with Shift+Tab. Focus stays inside the palette either way. */
  const focusable = () => [input, ...panel.querySelectorAll<HTMLElement>(".palette-input button:not(:disabled), .palette-types button, .palette-help-row")];
  const onPanelKey = (e: KeyboardEvent) => {
    if (e.target === input) return;
    e.stopPropagation();
    const at = e.target as HTMLElement;
    if (e.key === "Escape") { e.preventDefault(); input.focus(); }
    else if (e.key === "Tab") {
      e.preventDefault();
      const all = focusable();
      all[(all.indexOf(at) + (e.shiftKey ? -1 : 1) + all.length) % all.length].focus();
    } else if (at.matches(".palette-help-row") && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault();
      const rows = [...panel.querySelectorAll<HTMLElement>(".palette-help-row")];
      (rows[rows.indexOf(at) + (e.key === "ArrowDown" ? 1 : -1)] ?? input).focus();
    }
  };

  const onKey = (e: KeyboardEvent) => {
    e.stopPropagation();
    const n = count();
    const row = suggestions().rows[pick()];
    if (e.altKey && /^[0-9]$/.test(e.key) && !commandMode()) { e.preventDefault(); pickType(CHIPS[+e.key]); }
    else if (e.key === "Escape") { e.preventDefault(); if (helpOpen()) setHelpOpen(false); else if (!ui.menu) dismiss(); }
    else if (e.key === "Backspace" && typeChip() && input.selectionStart === 0 && input.selectionEnd === 0) { e.preventDefault(); setTypeChip(null); }
    else if (e.key === "Tab" && e.shiftKey) { e.preventDefault(); focusable().at(-1)?.focus(); }
    else if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "c") {
      const h = pickedHit();
      if (h) { e.preventDefault(); copied(pathOf(h), "Path"); }
    }
    else if ((e.shiftKey && e.key === "F10") || e.key === "ContextMenu") {
      e.preventDefault();
      const h = pickedHit();
      const at = list.querySelector<HTMLElement>(`[data-i="${pick()}"]`)?.getBoundingClientRect();
      if (h && at) showResultMenu(at.left + 24, at.bottom, pick(), h);
    }
    else if (e.ctrlKey && e.key.toLowerCase() === "s" && !commandMode()) {
      e.preventDefault();
      // On a recent search, that one; otherwise what's in the box.
      if (row?.icon === History) row.actions?.[0]?.run();
      else if (tabQuery()) askToSave(tabQuery(), fullQuery());
    }
    else if (e.key === "/" && e.ctrlKey && !commandMode()) { e.preventDefault(); if (text().trim() === "?") setText(""); setHelpOpen(!showHelp()); }
    else if (e.key === "Delete" && !text() && row?.icon === History) { e.preventDefault(); row.actions?.[1]?.run(); }
    else if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "p") { e.preventDefault(); setText(commandMode() ? "" : ">"); }
    else if (e.key === "ArrowDown" && showHelp()) { e.preventDefault(); panel.querySelector<HTMLElement>(".palette-help-row")?.focus(); }
    else if (e.key === "Tab" && tabRow()) { e.preventDefault(); tabRow()!.apply(); }
    else if (e.key === "ArrowDown") { e.preventDefault(); if (n) setPick((p) => (p + 1) % n); }
    else if (e.key === "ArrowUp") { e.preventDefault(); if (n) setPick((p) => (p - 1 + n) % n); }
    else if (e.key === "PageDown") { e.preventDefault(); setPick((p) => Math.min(n - 1, p + 8)); }
    else if (e.key === "PageUp") { e.preventDefault(); setPick((p) => Math.max(0, p - 8)); }
    else if (e.key === "Tab") { e.preventDefault(); if (canSwitchScope()) setScoped(!scoped()); }
    else if (e.key === "Enter" && e.ctrlKey && e.shiftKey && !commandMode()) { e.preventDefault(); openAsTab(); }
    else if (e.key === "Enter") {
      e.preventDefault();
      activate(pick(), e.ctrlKey ? "tab" : e.altKey || e.shiftKey ? "reveal" : "open");
    }
  };

  const resultMenu = (e: MouseEvent, i: number, h: Hit) => {
    e.preventDefault();
    showResultMenu(e.clientX, e.clientY, i, h);
  };
  const showResultMenu = (x: number, y: number, i: number, h: Hit) => {
    setPick(i);
    openMenu(x, y, [
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
      { label: "Copy path", icon: Hash, shortcut: "Ctrl+Shift+C", action: () => copied(pathOf(h), "Path") },
      { label: "Copy name", icon: Copy, action: () => copied(h.name, "Name") },
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

  const took = (r: SearchResults) => `${(r.tookUs / 1000).toFixed(r.tookUs < 10_000 ? 1 : 0)} ms`;
  const footer = () => {
    if (slow()) return <>Searching…</>;
    const r = res();
    if (!r || noVerdict()) return <>↑↓ choose · Enter open · <b>Alt+Enter show in folder</b> · Ctrl+Enter new tab · ? filters{canSwitchScope() ? " · Tab scope" : ""}</>;
    if (r.source === "content") return <>{r.total.toLocaleString()}{r.more ? "+" : ""} {r.total === 1 && !r.more ? "file has" : "files have"} it · {(r.scanned ?? 0).toLocaleString()} files read · {took(r)}</>;
    return <>{r.total === 1 ? "1 match" : `${r.total.toLocaleString()} matches`} · {took(r)}{r.partial ? " · still indexing" : ""}</>;
  };
  /** Read out by a screen reader when a search finishes. */
  const announcement = () => {
    const r = res();
    if (commandMode() || !settled() || noVerdict()) return "";
    if (failed()) return `Search failed: ${failed()}`;
    if (missing()) return `Folder not found: ${missing()}`;
    return r?.results.length ? `${r.total.toLocaleString()} ${r.total === 1 ? "match" : "matches"}` : fullQuery() ? "No matches" : "";
  };
  const emptyMessage = () => {
    if (contentSearch()) {
      return busy() ? `Reading the files in ${basename(searchFolder() ?? "") || "this folder"}…`
        : searchFolder() && isVirtual(searchFolder()!) ? <>Searching inside files needs a folder: open one, or add <code>in:C:\folder</code></>
        : res()?.source === "content-unsupported" ? "This server only allows file access (SFTP), so Ferret can't search inside its files"
        : `No file in ${basename(searchFolder() ?? "") || "the folder"} contains ${quoted(contentText(fullQuery()))}`;
    }
    if (failed()) return `Search failed: ${failed()}`;
    if (missing()) return `Folder not found, or it can't be opened: ${missing()}`;
    // No verdict until there's an answer: most searches take milliseconds, so nothing shows at all.
    if (!settled()) return slow() ? "Searching…" : "";
    if (status()?.building) return "Still indexing, so more results will appear…";
    const type = typeChip() ? findType(typeChip()!)?.label : null;
    if (!query() && type) return `No ${files(type)} found${whereSearched()}`;
    // The filters are in the box and on the chips: the sentence only needs the words.
    const words = withoutFilters(query());
    return `Nothing matches ${words ? quoted(words) : "these filters"}${type ? ` in ${files(type)}` : ""}${whereSearched()}`;
  };
  /** Ways out of a search that found nothing. */
  const emptyActions = () => {
    if (!settled() || failed() || contentSearch() || status()?.building) return [];
    const out: { label: string; run: () => void }[] = [];
    if (scoped()) out.push({ label: "Search everywhere", run: () => setScoped(false) });
    if (typeChip()) out.push({ label: "All types", run: () => setTypeChip(null) });
    const bare = withoutFilters(text());
    if (bare !== text().trim() && bare) out.push({ label: "Remove filters", run: () => setText(bare) });
    return out;
  };

  return (
    <Portal>
      {/* Out of the way while a result is dragged, to show the folders it can go to. */}
      <div class="palette-backdrop" classList={{ dragging: !!drag() }} onMouseDown={(e) => e.target === e.currentTarget && dismiss()}>
        <div class="palette" ref={panel} role="dialog" aria-modal="true" aria-label={commandMode() ? "Command palette" : "Search"} onKeyDown={onPanelKey}>
          <div class="palette-live" role="status">{announcement()}</div>
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
              role="combobox"
              aria-label={commandMode() ? "Command" : "Search"}
              aria-autocomplete="list"
              aria-controls="palette-list"
              aria-expanded={count() > 0}
              aria-activedescendant={count() > 0 ? `palette-opt-${pick()}` : undefined}
              onInput={(e) => { setText(e.currentTarget.value); setHelpOpen(false); }}
              onKeyDown={onKey}
            />
            <Show when={busy()}><LoaderCircle size={15} class="spin muted" /></Show>
            <Show when={!commandMode() && tabQuery()}>
              <button class="icon-btn tiny" classList={{ on: !!savedSearch(tabQuery()) }}
                title={savedSearch(tabQuery()) ? "Saved in the sidebar: rename (Ctrl+S)" : "Save this search to the sidebar (Ctrl+S)"}
                onClick={() => askToSave(tabQuery(), fullQuery())}>
                <Star size={15} fill={savedSearch(tabQuery()) ? "currentColor" : "none"} />
              </button>
            </Show>
            <Show when={!commandMode()}>
              <button class="icon-btn tiny palette-help-btn" classList={{ on: showHelp() }} title="File types and filters (Ctrl+/)"
                onClick={() => { const open = showHelp(); if (text().trim() === "?") setText(""); setHelpOpen(!open); input.focus(); }}>
                <CircleHelp size={15} />
              </button>
              <button class="palette-scope" classList={{ on: scoped() || !!contentFolder() || ownFolder() }} disabled={!!contentFolder() || ownFolder()}
                title={ownFolder() ? "The in: filter picks the folder" : contentFolder() ? "Searches inside files read this folder (or the one named with in:)" : "Toggle scope (Tab)"}
                onClick={() => { setScoped(!scoped()); input.focus(); }}>
                {/* A search inside files reads the open folder even when "Everywhere" is picked. */}
                {ownFolder() ? <><FolderSearch size={13} /> <span>in {inFolderName(fullQuery())}</span></>
                  : scoped() || contentFolder() ? <><FolderSearch size={13} /> This folder</> : <><HardDrive size={13} /> Everywhere</>}
              </button>
            </Show>
            <button class="icon-btn tiny" title="Close (Esc)" onClick={dismiss}><X size={14} /></button>
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

          <div class="palette-list" id="palette-list" ref={list} role={showHelp() ? undefined : "listbox"} aria-label={showHelp() ? undefined : "Results"}>
            <Show when={commandMode()} fallback={
              <Show when={!showHelp()} fallback={<SearchHelp onType={pickType} onInsert={insertFilter} />}>
              <For each={suggestions().rows}>
                {(s, i) => (
                  <>
                    <Show when={s.heading}><div class="palette-heading" role="presentation">{s.heading}</div></Show>
                    <button class="palette-suggest" classList={{ active: i() === pick() }} data-i={i()} id={`palette-opt-${i()}`} tabIndex={-1}
                      role="option" aria-selected={i() === pick()}
                      onMouseMove={() => setPick(i())} onClick={() => s.apply()}>
                      <Dynamic component={s.icon} size={15} />
                      <span class="palette-suggest-main">{s.main}</span>
                      <span class="palette-suggest-exts">{s.detail}</span>
                      <For each={s.actions}>
                        {(a) => (
                          <span class="palette-suggest-act" role="button" title={a.title} aria-label={a.title}
                            onClick={(e) => { e.stopPropagation(); a.run(); input.focus(); }}>
                            <Dynamic component={a.icon} size={13} />
                          </span>
                        )}
                      </For>
                      <Show when={tabRow() === s}><kbd>Tab</kbd></Show>
                    </button>
                  </>
                )}
              </For>
              <Show when={shown().length} fallback={
                // A filter still waiting for its value ("ext:") isn't a search that found nothing.
                <div class="palette-empty" hidden={noVerdict()}>
                  <Show when={fullQuery()} fallback={<span>Type to search {indexed() ? `${indexed().toLocaleString()} indexed items` : "your drives"}, or pick a type above. <code>?</code> shows every filter · <code>&gt;</code> for commands</span>}>
                    <span>{emptyMessage()}</span>
                    <Show when={emptyActions().length}>
                      <div class="palette-empty-actions">
                        <For each={emptyActions()}>{(a) => <button class="btn" onClick={() => { a.run(); input.focus(); }}>{a.label}</button>}</For>
                      </div>
                    </Show>
                  </Show>
                </div>
              }>
                <For each={shown()}>
                  {(row, i) => { const h = row.hit; const n = () => i() + offset(); return (
                    <>
                    <Show when={row.heading}><div class="palette-heading" role="presentation">{row.heading}</div></Show>
                    <div class="palette-item file" id={`palette-opt-${n()}`} role="option" aria-selected={n() === pick()} classList={{ active: n() === pick(), "has-line": !!h.line }} data-i={n()}
                      onMouseMove={() => setPick(n())}
                      onMouseDown={(e) => {
                        if ((e.target as HTMLElement).closest("button")) return;
                        // The search box keeps the keyboard, and no text gets selected on the way.
                        e.preventDefault();
                        beginDrag(e, () => [h.path], false, { copies: true, known: () => ({ isDir: h.isDir, size: h.size ?? 0 }), onDropped: close });
                      }}
                      onClick={(e) => activate(n(), e.ctrlKey ? "tab" : e.altKey ? "reveal" : "open")}
                      onAuxClick={(e) => e.button === 1 && activate(n(), "tab")}
                      onContextMenu={(e) => resultMenu(e, n(), h)}
                      title={h.path}>
                      <FileIcon entry={{ isDir: h.isDir, ext: h.isDir ? "" : (h.name.split(".").pop() ?? "").toLowerCase(), isSymlink: false }} size={16} />
                      <span class="palette-name"><ResultName text={h.name} matches={row.loose ? [] : h.matches} /></span>
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
                      <span class="palette-meta palette-size">{h.isDir || h.size == null ? "" : formatSize(h.size)}</span>
                      <span class="palette-meta palette-date">{h.modified ? formatDate(h.modified).slice(0, 10) : ""}</span>
                      <button class="palette-reveal" tabIndex={-1} title={h.isDir ? "Open folder" : "Show in folder (Alt+Enter)"} aria-label={h.isDir ? "Open folder" : "Show in folder"}
                        onClick={(e) => { e.stopPropagation(); activate(n(), "reveal"); }}>
                        <FolderOpen size={14} />
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
                    </>
                  ); }}
                </For>
                <Show when={!res()?.more && (res()?.total ?? 0) > (res()?.results.length ?? 0)}>
                  <div class="palette-more" role="presentation">
                    <span>Showing the top {res()!.results.length} of {res()!.total.toLocaleString()} matches</span>
                    <button class="btn" tabIndex={-1} title="Ctrl+Shift+Enter" onClick={openAsTab}>Open in a tab</button>
                  </div>
                </Show>
              </Show>
              </Show>
            }>
              <For each={commandHits()} fallback={<div class="palette-empty">No matching command</div>}>
                {(x, i) => (
                  <button class="palette-item cmd" classList={{ active: i() === pick() }} data-i={i()} id={`palette-opt-${i()}`} tabIndex={-1}
                    role="option" aria-selected={i() === pick()}
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
              <span class="palette-foot-main">{footer()}</span>
              <Show when={pickedHit()}>
                {(h) => <span class="palette-picked" title={pathOf(h())}>{`‎${pathOf(h())}‎`}</span>}
              </Show>
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
