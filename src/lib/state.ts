import { batch, createSignal } from "solid-js";
import { createStore, unwrap } from "solid-js/store";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";
import { invalidateFolderSizes } from "./foldersizes";
import { onColor } from "./format";
import { log } from "./log";
import { basename, isArchiveName, isRecycle, isSearch, isVirtual, isWsl, parent, pathKey, samePath, searchQuery } from "./path";
import type {
  Entry, Favorite, LaunchTarget, Places, ProfileName, Recent, ResolvedView, Session, Settings, ViewSettings, ViewSource,
} from "./types";

// ---------- global state ----------

export const DEFAULT_VIEW: ViewSettings = {
  mode: "list", sortBy: "name", sortDir: "asc", groupBy: "none",
  foldersFirst: true, showHidden: false, iconSize: 96, previewOpen: false,
};

export const [settings, setSettings] = createStore<Settings>({
  theme: "dark", accent: "#e2457f", backdrop: "mica", density: "comfortable",
  doubleClickOpen: true, rememberPerFolder: true, inheritToSubfolders: false, restoreSession: true,
  hiddenPlaces: [], hiddenSections: [],
  columnWidths: { modified: 150, type: 140, size: 90 }, hiddenColumns: [], folderSizes: true, animations: true,
  searchHistory: [], savedSearches: [],
  globalView: { ...DEFAULT_VIEW },
  profiles: {
    media: { enabled: true, view: {} },
    code: { enabled: true, view: {} },
    downloads: { enabled: true, view: {} },
  },
});

export const [places, setPlaces] = createSignal<Places>({ home: "C:\\", known: [], drives: [], wsl: [], recycle: { items: 0, bytes: 0 } });
/** Re-reads the Recycle Bin's item count for the sidebar. */
export const refreshRecycleInfo = () => api.recycleInfo().then((recycle) => setPlaces((p) => ({ ...p, recycle }))).catch(() => {});
export const [favorites, setFavorites] = createSignal<Favorite[]>([]);
export const [recents, setRecents] = createSignal<Recent[]>([]);
export const [ready, setReady] = createSignal(false);
/** This window runs as administrator: it saves no session or recents. */
export const [adminWindow, setAdminWindow] = createSignal(false);

/** Restarts this window as administrator, like Task Manager's "run as administrator":
 *  Windows shows the UAC prompt, then the same window comes back elevated with the same
 *  tabs in the same place. `open` (e.g. a folder that was refused) becomes the active tab. */
export async function elevateWindow(open?: string) {
  const list = tabs().map((t) => t.p);
  let active = Math.max(0, tabs().findIndex((t) => t.id === activeId()));
  if (open) {
    const i = list.findIndex((p) => samePath(p, open));
    if (i >= 0) active = i;
    else active = list.push(open) - 1;
  }
  if (!adminWindow()) await saveSessionNow();
  try {
    await api.restartElevated(list, active);
  } catch (e) {
    if (e !== "Cancelled") toast(`Couldn't restart as administrator: ${e}`, "error");
    return;
  }
  // The backend closes this window once the elevated one is on screen.
}

/** Opens another Ferret window (elevated if this one is, like any program it starts), by default
 *  at the folder this one shows, like Explorer's Ctrl+N. */
export function openNewWindow(path?: string) {
  const here = activeTab()?.p;
  path ??= here && !isVirtual(here) ? here : undefined;
  api.newWindow(path).catch((e) => toast(`Couldn't open a new window: ${e}`, "error"));
}
/** Bumped whenever a folder listing changes (previews of folders re-read). */
export const [fsTick, setFsTick] = createSignal(0);

/** Live window width (CSS px), for layout decisions. */
export const [winWidth, setWinWidth] = createSignal(window.innerWidth);
window.addEventListener("resize", () => setWinWidth(window.innerWidth));

/** Sidebar docked beside the content (hidden by the user or when the window is narrow). */
export const sidebarDocked = () => !ui.sidebarHidden && winWidth() >= 860;

/** Preview pane width that still leaves the file list usable; 0 = no room (pane hidden). */
export const previewSpace = () => {
  const room = winWidth() - (sidebarDocked() ? ui.sidebarWidth : 0) - 32 - 520;
  const w = Math.min(ui.previewWidth, room);
  return w >= 240 ? w : 0;
};

export interface MenuItem {
  label?: string;
  icon?: any;
  shortcut?: string;
  checked?: boolean;
  disabled?: boolean;
  danger?: boolean;
  heading?: boolean;
  separator?: boolean;
  /** Image URL used instead of `icon` (e.g. app icons in "Open with"). */
  iconUrl?: string | null;
  /** Renders a row of small buttons instead of a single item. */
  row?: { label: string; title?: string; active?: boolean; color?: string; action: () => void }[];
  /** A row of icon buttons (Cut, Copy, Rename, Delete), like the top of Windows 11's menu. */
  actions?: { icon: any; title: string; disabled?: boolean; danger?: boolean; action: () => void }[];
  submenu?: MenuItem[];
  /** Submenu built on first hover (e.g. "Open with" apps). */
  loadSubmenu?: () => Promise<MenuItem[]>;
  action?: () => void;
  /** Running it leaves the menu open (check items toggled several at a time). */
  keepOpen?: boolean;
  /** Dim text after the label (where a folder is, to tell same-named ones apart). */
  detail?: string;
  /** Tooltip. */
  title?: string;
}

export interface ToastAction {
  label: string;
  run: () => void;
}

export const [ui, setUi] = createStore({
  settingsOpen: false,
  editingPath: false,
  filterOpen: false,
  sidebarWidth: 248,
  collapsedSections: [] as string[],
  /** `hold`: other apps' entries are still loading; the menu appears once they're in (or after a moment). */
  menu: null as null | { x: number; y: number; items: MenuItem[]; hold?: boolean },
  toasts: [] as { id: number; text: string; kind: "info" | "error"; action?: ToastAction }[],
  previewOpen: false,
  previewWidth: 360,
  quickLook: false,
  /** Paths whose Properties dialog is open. */
  properties: null as null | string[],
  batchRename: null as null | string[],
  /** Videos or pictures open in the compress editor. */
  media: null as null | { kind: "video" | "image"; paths: string[] },
  conflict: null as null | { conflicts: import("./types").Conflict[]; op: "copy" | "move"; dest: string; resolve: (p: import("./types").Policy | null) => void },
  /** Mirror of what Ferret put on the clipboard, to dim cut items. */
  clipboard: null as null | { paths: string[]; cut: boolean },
  /** Folder path currently highlighted as a drop target. */
  dropTarget: null as null | string,
  dialog: null as null | DialogRequest,
  /** Dropzone shelf: file references parked while navigating. */
  shelf: [] as string[],
  /** User hid the sidebar (Ctrl+B). */
  sidebarHidden: false,
  /** Sidebar temporarily shown over the content (narrow window / hidden). */
  sidebarOverlay: false,
  /** Ctrl+K search / Ctrl+Shift+P commands. */
  palette: null as null | "files" | "commands",
});

export const SHELF = "::shelf::";

/** Keys typed between "New folder" and its rename box appearing (so fast typing isn't lost).
 *  One buffer per pending new item, consumed in creation order. */
type TypeAhead = { text: string; enter: boolean; at: number };
let typeAhead: TypeAhead[] = [];
const freshTypeAhead = () => (typeAhead = typeAhead.filter((t) => Date.now() - t.at < 5000));
export const beginTypeAhead = () => { freshTypeAhead().push({ text: "", enter: false, at: Date.now() }); };
export function feedTypeAhead(e: KeyboardEvent): boolean {
  const cur = freshTypeAhead()[typeAhead.length - 1];
  if (!cur || e.ctrlKey || e.altKey || e.metaKey) return false;
  if (cur.enter) return false; // already confirmed; later keys belong to the app
  if (e.key === "Enter") { cur.enter = true; holdKeys(); return true; }
  if (e.key === "Escape") { typeAhead.pop(); return true; }
  if (e.key === "Backspace") { cur.text = cur.text.slice(0, -1); return true; }
  if (e.key.length === 1) { cur.text += e.key; return true; }
  return false;
}
/** While a rename / new item is being saved, keystrokes are held and replayed afterwards
 *  (so "rename ⏎ Delete" or "new folder ⏎ ⏎" act on the right item). */
type HeldKey = { key: string; code: string; shiftKey: boolean; ctrlKey: boolean; altKey: boolean };
let held: HeldKey[] = [];
let holding = false;
let holdTimer: number | undefined;
export const keysHeld = () => holding;
export function holdKeys() {
  holding = true;
  clearTimeout(holdTimer);
  holdTimer = window.setTimeout(() => releaseKeys(), 5000);
}
export function queueKey(e: KeyboardEvent) {
  held.push({ key: e.key, code: e.code, shiftKey: e.shiftKey, ctrlKey: e.ctrlKey, altKey: e.altKey });
}
export function releaseKeys(discard = false) {
  if (!holding) return;
  clearTimeout(holdTimer);
  if (discard) {
    holding = false;
    held = [];
    return;
  }
  // Keep holding until the replay actually runs, so keys typed in the meantime
  // join the queue in order instead of jumping ahead of it.
  requestAnimationFrame(() => {
    holding = false;
    const keys = held;
    held = [];
    if (!keys.length) return;
    const files = fileList();
    if (!files) return;
    files.focus({ preventScroll: true });
    for (const k of keys) files.dispatchEvent(new KeyboardEvent("keydown", { ...k, bubbles: true, cancelable: true }));
  });
}

export function takeTypeAhead(): TypeAhead | null {
  return freshTypeAhead().shift() ?? null;
}

export function addToShelf(paths: string[]) {
  const have = new Set(ui.shelf.map((p) => p.toLowerCase()));
  const fresh = paths.filter((p) => !have.has(p.toLowerCase()));
  if (!fresh.length) return;
  setUi("shelf", [...ui.shelf, ...fresh]);
  persistSession();
  toast(`Added ${fresh.length === 1 ? basename(fresh[0]) : `${fresh.length} items`} to the shelf`);
}

export function removeFromShelf(paths: string[]) {
  const drop = new Set(paths.map((p) => p.toLowerCase()));
  setUi("shelf", ui.shelf.filter((p) => !drop.has(p.toLowerCase())));
  persistSession();
}

/** What `ask` resolves to when the dialog's second button (`alt`) is chosen. */
export const ASK_ALT = "\u0000alt";

export interface DialogRequest {
  title: string;
  /** A second confirm button; choosing it resolves to ASK_ALT. */
  alt?: string;
  message?: string;
  /** Present → shows a text input with this initial value. */
  input?: string;
  password?: boolean;
  confirmLabel?: string;
  danger?: boolean;
  /** Shows the Windows shield: confirming leads to a UAC prompt. */
  shield?: boolean;
  resolve: (value: string | null) => void;
}

/** Styled replacement for confirm()/prompt(). Resolves to the input (or "") on OK, null on cancel. */
export function ask(req: Omit<DialogRequest, "resolve">): Promise<string | null> {
  return new Promise((resolve) => setUi("dialog", { ...req, resolve }));
}

let toastSeq = 0;
export function toast(text: string, kind: "info" | "error" = "info", action?: ToastAction) {
  showToast(text, kind, action);
}
/** toast(), returning the toast's id for dismissToast. */
export function showToast(text: string, kind: "info" | "error" = "info", action?: ToastAction) {
  const id = ++toastSeq;
  // What the user was told, next to the raw error the backend logged.
  if (kind === "error") log.warn(`error shown: ${text}`);
  text = friendly(text);
  if (action && /Recycle Bin$/.test(text)) setUi("toasts", (t) => t.filter((x) => !(x.action && /Recycle Bin$/.test(x.text))));
  setUi("toasts", (t) => [...t.slice(-3), { id, text, kind, action }]);
  setTimeout(() => dismissToast(id), kind === "error" ? 6000 : action ? 6000 : 2600);
  return id;
}
/** Turns raw OS error text into something readable. */
export function friendly(text: string) {
  return text
    .replace(/elevate:/g, "")
    .replace(/denied:.*$/, "Access denied: Windows keeps this folder closed")
    .replace(/Error during `trash` operation: Os \{ code: -2147024894[^}]*\}/gi, "it no longer exists")
    .replace(/Error during (?:a )?`trash` operation: Unknown \{ description: "([^"]*)" \}/gi, "$1")
    .replace(/Os \{ code: -?\d+, description: "(?:windows error: )?([^"]*?)(?: \(0x[0-9A-F]+\))?" \}/gi, "$1")
    .replace(/Access is denied\.? ?\(os error 5\)/gi, "Access denied: you don't have permission")
    .replace(/The system cannot find the (file|path) specified\.? ?\(os error [23]\)/gi, "It no longer exists")
    .replace(/The process cannot access the file because it is being used by another process\.? ?\(os error 32\)/gi, "It's open in another program")
    .replace(/There is not enough space on the disk\.? ?\(os error 112\)/gi, "Not enough disk space")
    .replace(/ ?\(os error \d+\)/g, "");
}

export const dismissToast = (id: number) => setUi("toasts", (t) => t.filter((x) => x.id !== id));

export const openMenu = (x: number, y: number, items: MenuItem[]) => setUi("menu", { x, y, items });
export const closeMenu = () => setUi("menu", null);

// ---------- tabs ----------

let tabSeq = 0;

export class Tab {
  readonly id = ++tabSeq;
  token = 0;
  path = createSignal("");
  entries = createSignal<Entry[]>([]);
  loading = createSignal(false);
  error = createSignal<string | null>(null);
  view = createSignal<ViewSettings>({ ...DEFAULT_VIEW });
  source = createSignal<ViewSource>("global");
  sourcePath = createSignal<string | null>(null);
  profile = createSignal<ProfileName | null>(null);
  /** View changed but not saved (only when "remember per folder" is off). */
  dirty = createSignal(false);
  selection = createSignal<string[]>([]);
  cursor = createSignal<string | null>(null);
  anchor = createSignal<string | null>(null);
  collapsed = createSignal<Set<string>>(new Set());
  filter = createSignal("");
  history = createSignal<string[]>([]);
  hIndex = createSignal(-1);
  /** Bumped to ask the view to scroll the cursor into view. */
  reveal = createSignal(0);
  /** Entries in on-screen order (set by the file view; used by Quick Look). */
  ordered = createSignal<Entry[]>([]);
  /** Path being renamed inline. */
  renaming = createSignal<string | null>(null);
  /** Split view: the second pane, a Tab of its own (not in the tab strip). */
  split = createSignal<Tab | null>(null);
  /** The second pane has the keyboard (the toolbar, preview and commands follow it). */
  splitFocused = createSignal(false);

  get p() { return this.path[0](); }
}

export const [tabs, setTabs] = createSignal<Tab[]>([]);
export const [activeId, setActiveId] = createSignal(0);
/** The tab selected in the tab strip. */
export const stripTab = () => tabs().find((t) => t.id === activeId()) ?? tabs()[0];
/** The pane in use in a tab: the tab itself, or its second pane while that has the focus. */
export const paneOf = (t: Tab) => (t.splitFocused[0]() && t.split[0]()) || t;
/** The pane being worked in. Everything that acts on "the current folder" follows it: the
 *  toolbar, keys, preview, Ctrl+K, the window title. */
export const activeTab = () => { const t = stripTab(); return t && paneOf(t); };
/** Every folder on screen or in a tab, second panes included (refreshing, watching). */
export const allPanes = () => tabs().flatMap((t) => { const s = t.split[0](); return s ? [t, s] : [t]; });
/** The tab whose second pane `pane` is. */
const ownerOf = (pane: Tab) => tabs().find((t) => t.split[0]() === pane);
/** The other pane of a split tab (where "Copy to other pane" goes), if it's split. */
export function otherPane(): Tab | null {
  const t = stripTab();
  const s = t?.split[0]();
  return s ? (t.splitFocused[0]() ? t : s) : null;
}
/** The focused pane's file list. */
export const fileList = () =>
  document.querySelector<HTMLElement>(`.fileview[data-tab="${activeTab()?.id}"] .files`) ?? document.querySelector<HTMLElement>(".files");
const focusList = () => requestAnimationFrame(() => fileList()?.focus({ preventScroll: true }));
/** After a box closes, gives the focus back to the list, unless something else took it in the
 *  meantime (a quick Ctrl+L or F2 after Escape opens the next box before this runs). */
export const refocusList = () => requestAnimationFrame(() => {
  const a = document.activeElement;
  if (!a || a === document.body || !a.isConnected) fileList()?.focus({ preventScroll: true });
});

/** Opens a second pane next to the tab, at the same folder (Ctrl+\), or closes it. */
export async function toggleSplit(t = stripTab()) {
  if (!t) return;
  if (t.split[0]()) return closeSplit(t);
  const pane = new Tab();
  pane.path[1](t.p);
  pane.loading[1](true);
  t.split[1](pane);
  t.splitFocused[1](true);
  await navigate(pane, t.p);
  syncWatchers();
  persistSession();
  focusList();
}

/** Closes the second pane. The pane in use stays (the toolbar and preview follow it): when it's
 *  the second one, the tab takes over its folder, history, view and selection. */
export function closeSplit(t = stripTab()) {
  const pane = t?.split[0]();
  if (!t || !pane) return;
  batch(() => {
    if (t.splitFocused[0]()) {
      for (const [k, v] of Object.entries(pane)) {
        if (k !== "split" && k !== "splitFocused" && Array.isArray(v)) (t as any)[k][1](() => v[0]());
      }
    }
    t.splitFocused[1](false);
    t.split[1](null);
  });
  syncWatchers();
  persistSession();
  focusList();
}

/** Gives the keyboard to the other pane (Tab). */
export function switchPane(t = stripTab()) {
  if (!t?.split[0]()) return;
  t.splitFocused[1](!t.splitFocused[0]());
  focusList();
}

/** A pane got the focus (a click in it): the toolbar, preview and keys follow it. */
export function focusPane(pane: Tab) {
  const owner = ownerOf(pane);
  if (owner) owner.splitFocused[1](true);
  else if (pane.split[0]()) pane.splitFocused[1](false);
}

function applyResolved(tab: Tab, r: ResolvedView) {
  tab.view[1]({ ...DEFAULT_VIEW, ...r.view });
  tab.source[1](r.source);
  tab.sourcePath[1](r.sourcePath);
  tab.profile[1](r.profile);
  tab.dirty[1](false);
}

export async function navigate(
  tab: Tab,
  path: string,
  opts: { history?: "push" | "none"; select?: string | null } = {},
): Promise<boolean> {
  const tok = ++tab.token;
  tab.loading[1](true);
  // Drop the previous folder's selection right away so a shortcut pressed mid-navigation
  // (Ctrl+C/X, Alt+Enter…) can never act on items that are no longer on screen.
  if (!samePath(tab.p, path)) batch(() => { tab.selection[1]([]); tab.cursor[1](null); tab.anchor[1](null); });
  let res;
  try {
    res = await api.listDir(path);
  } catch (e) {
    // Archives with encrypted file lists need the password before they can be browsed.
    if (e === "password-required" || e === "wrong-password") {
      const archive = path.match(/^(.*?\.(?:zip|7z|rar|cbr|cbz))(\\|$)/i)?.[1] ?? path;
      const pw = await ask({
        title: e === "wrong-password" ? "Wrong password" : "Password required",
        message: `“${basename(archive)}” is encrypted. Enter its password to browse it:`,
        input: "", password: true, confirmLabel: "Unlock",
      });
      if (tok === tab.token) tab.loading[1](false);
      if (pw === null) return false;
      try {
        await api.archiveUnlock(archive, pw);
      } catch (err) {
        toast(err === "wrong-password" ? "Wrong password" : String(err), "error");
        return false;
      }
      return navigate(tab, path, opts);
    }
    if (tok === tab.token) tab.loading[1](false);
    // Folders Windows keeps closed stay closed: Ferret never changes permissions.
    if (typeof e === "string" && e.startsWith("denied:")) {
      const denied = e.slice("denied:".length);
      // Inside WSL, Linux decides, as the distro's user: Windows administrator rights don't help.
      if (isWsl(denied)) {
        toast(`“${basename(denied)}” is closed to your Linux user. Open it from Linux, with sudo`, "error");
        return false;
      }
      if (adminWindow()) {
        toast(`Even administrators can't open “${basename(denied)}”`, "error");
        return false;
      }
      const ok = await ask({
        title: "Windows keeps this folder closed to your account",
        message: `Ferret can restart this window as administrator and open “${basename(denied)}” in it. Windows will ask you to confirm. Your tabs stay, the folder's permissions don't change, and the window stays administrator until you close it.`,
        confirmLabel: "Run as administrator", shield: true,
      });
      if (ok !== null) elevateWindow(denied);
      return false;
    }
    toast(`Can't open ${basename(path) || path}: ${e}`, "error");
    return false;
  }
  if (tok !== tab.token) return false;
  batch(() => {
    const prev = tab.p;
    tab.path[1](res.path);
    tab.entries[1](res.entries);
    applyResolved(tab, res.resolved);
    tab.collapsed[1](new Set<string>());
    if (!samePath(prev, res.path)) tab.filter[1]("");
    const sel = opts.select ?? null;
    tab.selection[1](sel ? [sel] : []);
    tab.cursor[1](sel);
    tab.anchor[1](sel);
    tab.reveal[1]((n) => n + 1);
    tab.loading[1](false);
    tab.error[1](null);
    if (opts.history !== "none") {
      const h = tab.history[0]().slice(0, tab.hIndex[0]() + 1);
      if (!samePath(h[h.length - 1], res.path)) h.push(res.path);
      tab.history[1](h);
      tab.hIndex[1](h.length - 1);
    }
  });
  if (ui.sidebarOverlay) setUi("sidebarOverlay", false);
  noteVisit(tab, res.path);
  syncWatchers();
  persistSession();
  return true;
}

// ---------- recents: folders you actually used ----------

/** A folder counts once you've stayed a moment or opened a file there, not when you pass
 *  through it on the way somewhere else. */
const DWELL_MS = 3000;
const visits = new Map<number, { path: string; timer: number; counted: boolean }>();

function noteVisit(tab: Tab, path: string) {
  const prev = visits.get(tab.id);
  if (prev) clearTimeout(prev.timer);
  if (isVirtual(path) || adminWindow()) return visits.delete(tab.id);
  const visit = { path, counted: false, timer: 0 };
  visit.timer = window.setTimeout(() => {
    if (document.visibilityState === "visible") countVisit(tab);
  }, DWELL_MS);
  visits.set(tab.id, visit);
}

/** Records the tab's current folder in Recent (once per visit). */
export function countVisit(tab: Tab) {
  const v = visits.get(tab.id);
  if (!v || v.counted || !samePath(v.path, tab.p)) return;
  v.counted = true;
  clearTimeout(v.timer);
  api.pushRecent(v.path).then(setRecents).catch(() => {});
}

/** Re-lists the folder in place, keeping selection and (unless asked) the current view. */
const vanishing = new Set<number>();

/** F5 / the refresh button: list the folder again and count its folders' sizes again. */
export function refresh(tab: Tab) {
  invalidateFolderSizes([tab.p], true);
  return reload(tab);
}

export async function reload(tab: Tab, reresolve = false) {
  const tok = ++tab.token;
  let res;
  try {
    res = await api.listDir(tab.p);
  } catch {
    if (tok === tab.token && !vanishing.has(tab.id)) await leaveIfGone(tab, tok);
    return;
  }
  if (tok !== tab.token) return;
  // Keep unchanged entry objects so the list keeps its DOM (and any inline rename) intact.
  const old = new Map(tab.entries[0]().map((e) => [e.path, e]));
  const merged = res.entries.map((n) => {
    const o = old.get(n.path);
    return o && o.size === n.size && o.modified === n.modified && o.hidden === n.hidden && o.isDir === n.isDir ? o : n;
  });
  const same = merged.length === old.size && merged.every((e) => old.get(e.path) === e);
  batch(() => {
    if (!same) { tab.entries[1](merged); setFsTick((n) => n + 1); }
    if (reresolve) applyResolved(tab, res.resolved);
    const exists = new Set(res.entries.map((e) => pathKey(e.path)));
    tab.selection[1]((s) => s.filter((p) => exists.has(pathKey(p))));
  });
}

/** A folder that couldn't be listed again: if it was deleted, renamed or unmounted, the tab
 *  moves up to the nearest folder that still exists. A folder that's still there (a listing
 *  that failed for a moment, say while a build rewrites it) stays open as it was. */
async function leaveIfGone(tab: Tab, tok: number) {
  vanishing.add(tab.id);
  try {
    const still = await api.resolveInput(tab.p).catch(() => null);
    if (still?.kind === "dir" || tok !== tab.token) return;
    let up = parent(tab.p);
    while (up) {
      const r = await api.resolveInput(up).catch(() => null);
      if (r?.kind === "dir") break;
      up = parent(up);
    }
    if (tok !== tab.token) return;
    toast(`“${basename(tab.p)}” is no longer available${up ? `. Moved up to ${basename(up)}` : ""}`, "error");
    if (recents().some((r) => samePath(r.path, tab.p))) api.removeRecent(tab.p).then(setRecents).catch(() => {});
    if (up) await navigate(tab, up);
  } finally {
    vanishing.delete(tab.id);
  }
}

export async function goBack(tab: Tab) {
  const i = tab.hIndex[0]();
  if (i <= 0) return;
  const from = tab.p;
  if (await navigate(tab, tab.history[0]()[i - 1], { history: "none", select: childOf(tab.history[0]()[i - 1], from) }))
    tab.hIndex[1](i - 1);
}

export async function goForward(tab: Tab) {
  const i = tab.hIndex[0]();
  const h = tab.history[0]();
  if (i >= h.length - 1) return;
  if (await navigate(tab, h[i + 1], { history: "none" })) tab.hIndex[1](i + 1);
}

/** Jumps to entry `i` of the tab's history (the Back / Forward buttons' right-click lists). */
export async function goToHistory(tab: Tab, i: number) {
  const h = tab.history[0]();
  if (i < 0 || i >= h.length || i === tab.hIndex[0]()) return;
  const from = tab.p;
  if (await navigate(tab, h[i], { history: "none", select: childOf(h[i], from) })) tab.hIndex[1](i);
}

/** If `from` lives inside `dir`, returns the direct child of `dir` on the way to it. */
function childOf(dir: string, from: string): string | null {
  let cur: string | null = from;
  while (cur) {
    const p = parent(cur);
    if (p && samePath(p, dir)) return cur;
    cur = p;
  }
  return null;
}

export function goUp(tab: Tab) {
  const p = parent(tab.p);
  if (p) navigate(tab, p, { select: tab.p });
}

export async function newTab(path?: string, activate = true, select?: string | null) {
  const target = path || places().home;
  const tab = new Tab();
  // Show the destination right away so a quick second Ctrl+T (which copies the
  // active tab's path) never sees an empty, still-loading tab.
  tab.path[1](target);
  tab.loading[1](true);
  setTabs((t) => [...t, tab]);
  if (activate) setActiveId(tab.id);
  const ok = await navigate(tab, target, { select });
  if (!ok && target !== places().home) await navigate(tab, places().home);
  return tab;
}

/** Recently closed tabs (most recent last) for Ctrl+Shift+T. */
export const [closedTabs, setClosedTabs] = createSignal<{ path: string; index: number }[]>([]);

export function reopenClosedTab() {
  const list = closedTabs();
  const last = list[list.length - 1];
  if (!last) return toast("No recently closed tabs");
  setClosedTabs(list.slice(0, -1));
  newTab(last.path).then((t) => moveTab(t.id, Math.min(last.index, tabs().length - 1)));
}

export function closeTab(id: number) {
  // Ctrl+W in a second pane closes that pane.
  const owner = tabs().find((t) => t.split[0]()?.id === id);
  if (owner) return closeSplit(owner);
  const list = tabs();
  if (list.length <= 1) return;
  const idx = list.findIndex((t) => t.id === id);
  if (idx >= 0) setClosedTabs((c) => [...c.slice(-19), { path: list[idx].p, index: idx }]);
  // A tab closed straight away doesn't make its folder "recent".
  clearTimeout(visits.get(id)?.timer);
  visits.delete(id);
  const next = list.filter((t) => t.id !== id);
  setTabs(next);
  if (activeId() === id) setActiveId(next[Math.min(idx, next.length - 1)].id);
  syncWatchers();
  persistSession();
}

export function moveTab(id: number, toIndex: number) {
  const list = [...tabs()];
  const from = list.findIndex((t) => t.id === id);
  if (from < 0) return;
  const [t] = list.splice(from, 1);
  list.splice(toIndex, 0, t);
  setTabs(list);
  persistSession();
}

export function cycleTab(delta: number) {
  const list = tabs();
  const i = list.findIndex((t) => t.id === activeId());
  setActiveId(list[(i + delta + list.length) % list.length].id);
  persistSession();
}

/** Opens an entry: folders navigate (or open a tab), files go to the default app. */
export function openEntry(tab: Tab, e: Pick<Entry, "path" | "isDir">, inNewTab = false) {
  // Recycled items can't be opened in place: files get a preview, folders an offer to restore.
  if (isRecycle(tab.p)) {
    if (!e.isDir) {
      batch(() => { tab.selection[1]([e.path]); tab.cursor[1](e.path); });
      return setUi("quickLook", true);
    }
    return toast("Restore this folder to open it", "info", {
      label: "Restore", run: () => import("./ops").then((m) => m.restoreFromBin([e.path])),
    });
  }
  // Archives open as folders (like Explorer does for zips); "Open with" still reaches 7-Zip etc.
  if (e.isDir || isArchiveName(e.path)) {
    if (inNewTab) newTab(e.path, false);
    else navigate(tab, e.path);
  } else {
    // Opening a file is the clearest sign this folder is one you use.
    countVisit(tab);
    import("./ops").then((m) => m.openFile(e as Entry));
  }
}

/** Sidebar / favorite target: folder → navigate, file → open it. */
export async function goTo(path: string, inNewTab = false) {
  const res = await api.resolveInput(path).catch(() => null);
  if (!res || res.kind === "missing") {
    if (recents().some((r) => samePath(r.path, path))) api.removeRecent(path).then(setRecents).catch(() => {});
    return toast(`“${basename(path)}” no longer exists`, "error");
  }
  if (res.kind === "file") {
    const dir = parent(res.path);
    if (dir) await navigate(activeTab(), dir, { select: res.path });
    return api.openPath(res.path).catch((e) => toast(String(e), "error"));
  }
  if (inNewTab) newTab(res.path, false);
  else navigate(activeTab(), res.path);
}

// ---------- watchers ----------

let watchTimer: number | undefined;
function syncWatchers() {
  clearTimeout(watchTimer);
  watchTimer = window.setTimeout(() => {
    // Each tab folder plus its parent: a folder deleted/renamed from outside only shows up
    // as an event in its parent.
    const all = allPanes().filter((t) => !isSearch(t.p)).flatMap((t) => [t.p, parent(t.p)]).filter((p): p is string => !!p);
    const paths = [...new Map(all.map((p) => [pathKey(p), p])).values()];
    api.watchDirs(paths).catch(() => {});
  }, 200);
}

const pendingReload = new Map<string, number>();
/** File jobs running (ops.ts keeps it up to date). */
export const [jobsRunning, setJobsRunning] = createSignal(0);

listen<string[]>("fs-changed", (ev) => {
  // While a job deletes or copies thousands of items the events never stop: reload at most once
  // a second (so the list still visibly empties or fills) instead of waiting for a quiet moment
  // or reloading after every item.
  const busy = jobsRunning() > 0;
  for (const dir of ev.payload) {
    const k = pathKey(dir);
    if (busy && pendingReload.has(k)) continue;
    clearTimeout(pendingReload.get(k));
    pendingReload.set(k, window.setTimeout(() => {
      pendingReload.delete(k);
      for (const t of allPanes()) if (pathKey(t.p) === k) reload(t);
    }, busy ? 1000 : 180));
  }
});

// ---------- views ----------

const viewSaveTimers = new Map<number, number>();

/** The view a folder would get without its own override (profile or global default). */
function baseView(tab: Tab): ViewSettings {
  const prof = tab.profile[0]();
  const p = prof && settings.profiles[prof]?.enabled ? settings.profiles[prof].view : {};
  return { ...DEFAULT_VIEW, ...settings.globalView, ...p };
}
const sameView = (a: ViewSettings, b: ViewSettings) =>
  (Object.keys(DEFAULT_VIEW) as (keyof ViewSettings)[]).every((k) => a[k] === b[k]);

export function updateView(tab: Tab, patch: Partial<ViewSettings>) {
  const v = { ...tab.view[0](), ...patch };
  tab.view[1](v);
  // Back to what the folder would get anyway (and nothing inherited): forget the override.
  if (settings.rememberPerFolder && tab.source[0]() === "folder" && sameView(v, baseView(tab))) {
    const path = tab.p;
    clearTimeout(viewSaveTimers.get(tab.id));
    viewSaveTimers.set(tab.id, window.setTimeout(() => { api.clearFolderView(path).catch(() => {}); }, 250));
    const prof = tab.profile[0]();
    tab.source[1](prof && settings.profiles[prof]?.enabled ? "profile" : "global");
    tab.sourcePath[1](null);
    return;
  }
  if (settings.rememberPerFolder) {
    // Debounced so slider drags / Ctrl+wheel don't hammer the database.
    const path = tab.p;
    clearTimeout(viewSaveTimers.get(tab.id));
    viewSaveTimers.set(tab.id, window.setTimeout(() => {
      api.setFolderView(path, v, false).catch((e) => toast(String(e), "error"));
    }, 250));
    tab.source[1]("folder");
    tab.sourcePath[1](tab.p);
  } else {
    tab.dirty[1](true);
  }
}

export async function saveViewForFolder(tab: Tab, recursive = false) {
  await api.setFolderView(tab.p, tab.view[0](), recursive);
  batch(() => {
    tab.source[1]("folder");
    tab.sourcePath[1](tab.p);
    tab.dirty[1](false);
  });
  toast(recursive ? "View applied to this folder and all subfolders" : "View saved for this folder");
}

export async function resetView(tab: Tab) {
  await api.clearFolderView(tab.p);
  await reload(tab, true);
  toast("View reset to default");
}

/** Re-resolves the view in every tab (after settings / overrides change). */
export function reresolveAll() {
  for (const t of allPanes()) reload(t, true);
}

export function sortBy(tab: Tab, key: ViewSettings["sortBy"]) {
  const v = tab.view[0]();
  if (v.sortBy === key) updateView(tab, { sortDir: v.sortDir === "asc" ? "desc" : "asc" });
  else updateView(tab, { sortBy: key, sortDir: key === "name" || key === "type" ? "asc" : "desc" });
}

// ---------- favorites ----------

export const isFavorite = (path: string) => favorites().some((f) => samePath(f.path, path));

export async function toggleFavorite(path: string, isDir = true) {
  if (isFavorite(path)) {
    setFavorites(await api.removeFavorite(path));
    toast(`Removed ${basename(path)} from favorites`);
  } else {
    setFavorites(await api.addFavorite(path, basename(path), isDir));
    toast(`Added ${basename(path)} to favorites`);
  }
}


// ---------- persistence ----------

let sessionTimer: number | undefined;
export function persistSession() {
  if (!ready() || adminWindow()) return;
  clearTimeout(sessionTimer);
  sessionTimer = window.setTimeout(() => void saveSessionNow(), 400);
}

/** Saves this window's tabs and layout now (the last window to save is restored next time). */
export function saveSessionNow(): Promise<void> {
  clearTimeout(sessionTimer);
  if (!ready() || adminWindow()) return Promise.resolve();
  const list = tabs();
  const session: Session = {
    tabs: list.map((t) => ({ path: t.p, ...(t.split[0]() ? { split: t.split[0]()!.p, splitFocused: t.splitFocused[0]() } : {}) })),
    active: Math.max(0, list.findIndex((t) => t.id === activeId())),
    sidebarWidth: ui.sidebarWidth,
    collapsedSections: [...ui.collapsedSections],
    previewOpen: ui.previewOpen,
    previewWidth: ui.previewWidth,
    shelf: [...ui.shelf],
    sidebarHidden: ui.sidebarHidden,
  };
  return api.saveSession(session).catch(() => {});
}

let settingsTimer: number | undefined;
export function persistSettings() {
  clearTimeout(settingsTimer);
  settingsTimer = window.setTimeout(() => {
    api.saveSettings(structuredClone(unwrap(settings))).catch((e) => toast(String(e), "error"));
  }, 250);
}

// ---------- search history and saved searches ----------

const HISTORY_MAX = 15;
const sameQuery = (a: string, b: string) => a.trim().toLowerCase() === b.trim().toLowerCase();

/** Remembers a Ctrl+K query that led somewhere (newest first, no repeats). */
export function rememberSearch(query: string) {
  const q = query.trim();
  if (!q || adminWindow()) return;
  setSettings("searchHistory", (h) => [q, ...h.filter((x) => !sameQuery(x, q))].slice(0, HISTORY_MAX));
  persistSettings();
}

export function forgetSearch(query: string) {
  setSettings("searchHistory", (h) => h.filter((x) => !sameQuery(x, query)));
  persistSettings();
}

export const savedSearch = (query: string) => settings.savedSearches.find((s) => sameQuery(s.query, query));

/** Pins a search to the sidebar (renaming it if it's already there). */
export function saveSearch(name: string, query: string) {
  const q = query.trim();
  const n = name.trim() || q;
  setSettings("savedSearches", (list) => savedSearch(q)
    ? list.map((s) => (sameQuery(s.query, q) ? { ...s, name: n } : s))
    : [...list, { name: n, query: q }]);
  persistSettings();
}

export function removeSavedSearch(query: string) {
  setSettings("savedSearches", (list) => list.filter((s) => !sameQuery(s.query, query)));
  persistSettings();
}

/** Asks for a name, then pins the search. */
export async function promptSaveSearch(query: string) {
  const q = query.trim();
  if (!q) return;
  const existing = savedSearch(q);
  const name = await ask({
    title: existing ? "Rename saved search" : "Save search",
    message: `“${q}” goes in the sidebar under Searches. It opens as a tab that always lists the current matches.`,
    input: existing?.name ?? q, confirmLabel: existing ? "Rename" : "Save",
  });
  if (name === null) return;
  saveSearch(name, q);
  if (!existing) toast(`Saved “${name.trim() || q}” to the sidebar`);
}

/** What a search tab is called: its saved name, or the query. */
export const searchTitle = (p: string) => savedSearch(searchQuery(p))?.name ?? `“${searchQuery(p)}”`;

// ---------- appearance ----------

const darkQuery = window.matchMedia("(prefers-color-scheme: dark)");
export const isDark = () => settings.theme === "dark" || (settings.theme === "auto" && darkQuery.matches);

export async function applyAppearance() {
  const root = document.documentElement;
  const dark = isDark();
  root.dataset.theme = dark ? "dark" : "light";
  root.dataset.density = settings.density;
  root.style.setProperty("--accent", settings.accent);
  root.style.setProperty("--on-accent", onColor(settings.accent));
  const ok = settings.backdrop === "solid" ? false : await api.setBackdrop(settings.backdrop, dark).catch(() => false);
  if (settings.backdrop === "solid") api.setBackdrop("solid", dark).catch(() => {});
  root.dataset.backdrop = ok ? "glass" : "solid";
}
darkQuery.addEventListener("change", () => settings.theme === "auto" && applyAppearance());

// ---------- boot ----------

/** How long starting took, step by step (ms since the page began loading): logged once
 *  ("startup: …"), and kept for the end-to-end tests. */
const startup: Record<string, number> = {};
export const stamp = (step: string) => { startup[step] = Math.round(performance.now()); };

export async function boot() {
  // Dev-only handle for debugging / UI tests.
  if (import.meta.env.DEV) (window as any).__ferret = { tabs, activeTab, ui, settings };
  stamp("code");
  const [b, info, launch] = await Promise.all([
    api.bootstrap(),
    api.windowInfo().catch(() => ({ elevated: false, firstWindow: true })),
    // What this window was opened for: command line, Win+E / a folder double-click, or the
    // tabs of a window that restarted itself as administrator.
    api.launchTargets().catch(() => [] as LaunchTarget[]),
  ]);
  stamp("settings");
  setAdminWindow(info.elevated);
  batch(() => {
    setSettings(b.settings);
    setPlaces({ ...b.places, recycle: { items: 0, bytes: 0 } });
    setFavorites(b.favorites);
    setRecents(b.recents);
    if (b.session?.sidebarWidth) setUi("sidebarWidth", b.session.sidebarWidth);
    if (b.session?.collapsedSections) setUi("collapsedSections", b.session.collapsedSections);
    if (b.session?.previewOpen) setUi("previewOpen", true);
    if (b.session?.previewWidth) setUi("previewWidth", b.session.previewWidth);
    if (b.session?.shelf) setUi("shelf", b.session.shelf);
    if (b.session?.sidebarHidden) setUi("sidebarHidden", true);
  });
  await applyAppearance();
  stamp("appearance");

  // Only the first window restores the last session; later ones open what they were
  // launched for, or the home folder.
  const fromSession = !launch.length && info.firstWindow && !info.elevated && settings.restoreSession && !!b.session?.tabs?.length;
  const wanted: { path: string; select?: string | null; split?: string; splitFocused?: boolean }[] = launch.length
    ? launch.map((t) => ({ path: t.path || b.places.home, select: t.select }))
    : fromSession ? b.session!.tabs : [{ path: b.places.home }];
  const first = launch.length ? Math.max(0, launch.findIndex((t) => t.active)) : fromSession ? b.session!.active ?? 0 : 0;
  // Every tab is in the strip from the start, with its folder named, but only the one shown
  // (and its second pane) is opened before the window appears; the others open right after.
  const restored = wanted.map((t) => {
    const tab = new Tab();
    tab.path[1](t.path);
    tab.loading[1](true);
    if (t.split) {
      const pane = new Tab();
      pane.path[1](t.split);
      pane.loading[1](true);
      tab.split[1](pane);
      tab.splitFocused[1](!!t.splitFocused);
    }
    return tab;
  });
  /** Opens a restored tab's folders: false when its own can't be opened. A second pane that
   *  can't be opened is closed. */
  const open = async (tab: Tab, i: number) => {
    const pane = tab.split[0]();
    const [ok, paneOk] = await Promise.all([
      navigate(tab, wanted[i].path, { select: wanted[i].select ?? null }),
      !pane || navigate(pane, pane.p),
    ]);
    if (!paneOk && tab.split[0]() === pane && !pane!.history[0]().length) batch(() => { tab.splitFocused[1](false); tab.split[1](null); });
    return ok;
  };
  // The tab to show, or the first that opens if its folder is gone, as if it had never been there.
  const order = [...new Set([first, ...wanted.keys()])].filter((i) => i < wanted.length);
  const gone = new Set<Tab>();
  let shown: Tab | undefined;
  for (const i of order) {
    if (await open(restored[i], i)) { shown = restored[i]; break; }
    gone.add(restored[i]);
  }
  if (!shown) {
    shown = new Tab();
    await navigate(shown, b.places.home);
  }
  const opened = restored.includes(shown) ? restored.filter((t) => !gone.has(t)) : [shown];
  setTabs(opened);
  setActiveId(shown.id);
  stamp("ready");
  setReady(true);
  syncWatchers();
  const s = startup;
  log.info(`startup: page and code ${s.code} ms, settings and window ${s.settings - s.code} ms, appearance ${s.appearance - s.settings} ms, ` +
    `shown tab ${s.ready - s.appearance} ms; ready ${s.ready} ms after the page began loading`);
  if (import.meta.env.DEV || import.meta.env.MODE === "e2e") (window as any).__ferretStartup = startup;

  // The other tabs, one at a time. One whose folder is gone is closed, unless it's been
  // used in the meantime (shown, or another folder opened in it).
  void (async () => {
    for (const tab of opened) {
      if (tab === shown || !tabs().includes(tab)) continue;
      if (await open(tab, restored.indexOf(tab)) || tab.history[0]().length || !tabs().includes(tab)) continue;
      if (stripTab() === tab || tabs().length <= 1) await navigate(tab, b.places.home);
      else {
        setTabs(tabs().filter((t) => t !== tab));
        persistSession();
      }
    }
    stamp("tabs");
  })();

  // Other Ferret windows share settings, favorites and recents: pick up their changes.
  window.addEventListener("focus", async () => {
    const s = await api.sharedState().catch(() => null);
    if (!s) return;
    if (JSON.stringify(s.settings) !== JSON.stringify(unwrap(settings))) {
      setSettings(s.settings);
      applyAppearance();
    }
    setFavorites(s.favorites);
    setRecents(s.recents);
  });

  // Folders deleted from outside on shares/WSL may not raise watcher events: check the open one.
  setInterval(() => {
    const t = activeTab();
    if (!t || t.loading[0]() || document.hidden || isVirtual(t.p)) return;
    api.resolveInput(t.p).then((r) => { if (r.kind === "missing") reload(t); }).catch(() => {});
  }, 4000);

  // Recent drops folders that no longer exist (checked now and every couple of minutes).
  const pruneRecents = async () => {
    for (const r of recents()) {
      const res = await api.resolveInput(r.path).catch(() => null);
      if (res?.kind === "missing") setRecents(await api.removeRecent(r.path));
    }
  };
  pruneRecents();
  setInterval(pruneRecents, 120_000);

  // Drive free space changes; refresh places occasionally.
  setInterval(() => api.getPlaces().then((p) => setPlaces((prev) => ({ ...p, recycle: prev.recycle }))).catch(() => {}), 60_000);
  // The Recycle Bin count comes separately so a slow drive never holds up startup.
  refreshRecycleInfo();
  setInterval(refreshRecycleInfo, 60_000);
}
