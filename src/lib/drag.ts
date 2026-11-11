// Pointer-driven drag & drop. HTML5 DnD is unavailable inside WebView2 while the
// window accepts external file drops, so we track the mouse ourselves:
//  - inside the window: drop onto any element with `data-drop="<folder path>"`
//    (folders in the list, sidebar places, breadcrumbs, tabs);
//  - leaving the window: hand off to a native OLE drag (Explorer, desktop, other apps);
//  - files dragged in from other apps arrive as `ferret-drop` events from Ferret's own drop
//    target (src-tauri/src/filedrop.rs), with what the app they come from allows.

import { createSignal } from "solid-js";
import { startDrag } from "@crabnebula/tauri-plugin-drag";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";
import { dropOp, mayTakeAway } from "./dropop";
import { basename, inRecycleBin, isRecycle, isRemote, isSearch, isWithin, parent, samePath } from "./path";
import { activeTab, addToShelf, SHELF, setActiveId, tabs, toast } from "./state";
import { deleteItems, realPath, transferTo } from "./ops";

/** Eats the click that a mouseup right after a drag would produce (only for this tick). */
function swallowNextClick() {
  const stop = (ev: MouseEvent) => ev.stopPropagation();
  window.addEventListener("click", stop, { capture: true, once: true });
  setTimeout(() => window.removeEventListener("click", stop, true), 0);
}

export interface DragState {
  paths: string[];
  x: number;
  y: number;
  target: string | null;
  op: "copy" | "move";
  inArchive: boolean;
}

export const [drag, setDrag] = createSignal<DragState | null>(null);

/** Server files dragged to another app are downloaded first, up to this much. */
const DRAG_DOWNLOAD_MAX = 128 << 20;

function chooseOp(sources: string[], target: string, e: { ctrlKey: boolean; shiftKey: boolean }, inArchive: boolean, copies = false): "copy" | "move" {
  if (inArchive) return "copy";
  if (copies && !e.shiftKey) return "copy";
  return dropOp(sources, target, e);
}

/** The last drag that left the window was one that copies (`DragFrom.copies`), for when it comes back in. */
let nativeCopies = false;

let hoverEl: HTMLElement | null = null;
function setHover(el: HTMLElement | null) {
  if (hoverEl === el) return;
  hoverEl?.classList.remove("drop-hover");
  hoverEl = el;
  el?.classList.add("drop-hover");
}

/** Finds a valid drop folder under the point, or null. */
function dropTargetAt(x: number, y: number, sources: string[]): { el: HTMLElement; path: string } | null {
  const el = (document.elementFromPoint(x, y) as HTMLElement | null)?.closest<HTMLElement>("[data-drop]");
  const path = el?.dataset.drop;
  if (!el || !path) return null;
  if (path === SHELF) return { el, path };
  // The Recycle Bin takes anything except its own items.
  if (isRecycle(path)) return sources.some(inRecycleBin) ? null : { el, path };
  // Can't drop a folder into itself or one of its children.
  if (sources.some((s) => isWithin(path, s))) return null;
  return { el, path };
}

let tabHoverTimer: number | undefined;
let tabHoverId: string | undefined;

function hoverTabs(x: number, y: number) {
  const tabEl = (document.elementFromPoint(x, y) as HTMLElement | null)?.closest<HTMLElement>("[data-tab-id]");
  const id = tabEl?.dataset.tabId;
  if (id === tabHoverId) return;
  tabHoverId = id;
  clearTimeout(tabHoverTimer);
  // Hovering a tab while dragging switches to it, so you can drop into its folder.
  if (id) tabHoverTimer = window.setTimeout(() => setActiveId(+id), 650);
}

/** For a drag from outside the list (Ctrl+K's results). */
export interface DragFrom {
  /** It copies unless Shift says otherwise, and dropping back in the item's own folder does nothing. */
  copies?: boolean;
  /** What's known about an item that isn't on screen, for handing it to another app. */
  known?: (path: string) => { isDir: boolean; size: number } | undefined;
  /** It landed somewhere in Ferret. */
  onDropped?: () => void;
}

/**
 * Call from an item's mousedown. `getPaths` runs once the drag actually starts
 * (after selection has settled).
 */
export function beginDrag(down: MouseEvent, getPaths: () => string[], inArchive = false, from: DragFrom = {}) {
  if (down.button !== 0) return;
  const sx = down.clientX;
  const sy = down.clientY;
  let started = false;
  let paths: string[] = [];
  let iconPath: Promise<string | null> = Promise.resolve(null);
  let native = false;

  const onMove = (e: MouseEvent) => {
    if (!started) {
      if (Math.abs(e.clientX - sx) + Math.abs(e.clientY - sy) < 6) return;
      paths = getPaths();
      if (!paths.length) return cleanup();
      started = true;
      document.body.classList.add("dragging-files");
      iconPath = api.dragIcon(paths[0]).catch(() => null);
    }
    const outside = e.clientX <= 0 || e.clientY <= 0 || e.clientX >= window.innerWidth - 1 || e.clientY >= window.innerHeight - 1;
    if (outside && !native) {
      native = true;
      goNative();
      return;
    }
    const t = dropTargetAt(e.clientX, e.clientY, paths);
    setHover(t?.el ?? null);
    hoverTabs(e.clientX, e.clientY);
    const target = t?.path ?? null;
    setDrag({ paths, x: e.clientX, y: e.clientY, target, op: target ? chooseOp(paths, target, e, inArchive, from.copies) : from.copies ? "copy" : "move", inArchive });
  };

  const onUp = (e: MouseEvent) => {
    const d = drag();
    cleanup();
    if (!started || !d) return;
    // Swallow the click that follows a drag.
    swallowNextClick();
    if (!d.target) return;
    const home = paths.every((p) => samePath(parent(p), d.target));
    if (from.copies && home && !e.ctrlKey) return;
    from.onDropped?.();
    if (d.target === SHELF) return addToShelf(paths);
    if (isRecycle(d.target)) return deleteItems(paths, e.shiftKey);
    const op = chooseOp(paths, d.target, e, inArchive, from.copies);
    if (op === "move" && home) return;
    transferTo(op, paths, d.target);
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      // Only the drag: not the selection, or the search it started from.
      e.preventDefault();
      e.stopPropagation();
      cleanup();
      // The mouse is still down: it coming up mustn't count as a click on the item.
      if (started) window.addEventListener("mouseup", swallowNextClick, { capture: true, once: true });
    } else if (drag()) setDrag((d) => d && { ...d, op: d.target ? chooseOp(paths, d.target, e, inArchive, from.copies) : d.op });
  };

  // Alt+Tab (or anything else taking focus) mid-drag: the pointer never left the window,
  // so hand off to the native drag now and the drop can land in the app switched to.
  const onBlur = () => {
    if (!started) return cleanup();
    if (!native) {
      native = true;
      goNative();
    }
  };

  async function goNative() {
    const d = drag();
    cleanup();
    nativeCopies = !!from.copies;
    const dragged = d?.paths ?? paths;
    const shown = new Map(activeTab().entries[0]().map((e) => [e.path, e]));
    const about = (p: string) => shown.get(p) ?? from.known?.(p);
    // A server's files aren't on this PC for other apps to take: small ones are downloaded first
    // (the drag starts once they're here), folders and big ones are copied to a folder in Ferret.
    if (dragged.some(isRemote)) {
      const items = dragged.map(about);
      if (items.some((e) => !e || e.isDir)) return toast("To use a server's folders in another app, copy them to a folder in Ferret first");
      if (items.reduce((n, e) => n + (e?.size ?? 0), 0) > DRAG_DOWNLOAD_MAX) return toast("Those files are big to drag out of a server: copy them to a folder in Ferret first");
    }
    try {
      // Archive entries need real files before other apps can take them (server files too).
      const real = (await Promise.all(dragged.map((p) => realPath({ ...about(p), path: p, inArchive }, true)))).filter(Boolean) as string[];
      const icon = (await iconPath) ?? "";
      if (real.length) await startDrag({ item: real, icon });
    } catch (err) {
      toast(`Drag failed: ${err}`, "error");
    }
  }

  function cleanup() {
    window.removeEventListener("mousemove", onMove, true);
    window.removeEventListener("mouseup", onUp, true);
    window.removeEventListener("keydown", onKey, true);
    window.removeEventListener("keyup", onKey, true);
    window.removeEventListener("blur", onBlur);
    document.body.classList.remove("dragging-files");
    setHover(null);
    clearTimeout(tabHoverTimer);
    tabHoverId = undefined;
    setDrag(null);
  }

  window.addEventListener("mousemove", onMove, true);
  window.addEventListener("mouseup", onUp, true);
  window.addEventListener("keydown", onKey, true);
  window.addEventListener("keyup", onKey, true);
  window.addEventListener("blur", onBlur);
}

/** Simple pointer reordering for tabs / favorites: calls onDrop(from, to). */
export function beginReorder(down: MouseEvent, selector: string, from: number, onDrop: (from: number, to: number) => void) {
  if (down.button !== 0) return;
  const sx = down.clientX;
  const sy = down.clientY;
  let started = false;
  let over: HTMLElement | null = null;
  const onMove = (e: MouseEvent) => {
    if (!started && Math.abs(e.clientX - sx) + Math.abs(e.clientY - sy) < 6) return;
    started = true;
    document.body.classList.add("reordering");
    const el = (document.elementFromPoint(e.clientX, e.clientY) as HTMLElement | null)?.closest<HTMLElement>(selector);
    if (el !== over) {
      over?.classList.remove("reorder-over");
      over = el ?? null;
      over?.classList.add("reorder-over");
    }
  };
  const onUp = () => {
    window.removeEventListener("mousemove", onMove, true);
    window.removeEventListener("mouseup", onUp, true);
    document.body.classList.remove("reordering");
    over?.classList.remove("reorder-over");
    const to = over ? Number(over.dataset.index) : NaN;
    if (started) {
      swallowNextClick();
      if (!Number.isNaN(to) && to !== from) onDrop(from, to);
    }
  };
  window.addEventListener("mousemove", onMove, true);
  window.addEventListener("mouseup", onUp, true);
}

// ---------- files dropped in from other apps ----------

/** A `ferret-drop` event (src-tauri/src/filedrop.rs). */
interface DropEvent {
  kind: "enter" | "over" | "drop" | "leave";
  /** On "enter" and "drop". */
  paths: string[];
  /** Physical pixels in the window. */
  x: number;
  y: number;
  /** DROPEFFECT bits: what the app the files come from allows (1 copy, 2 move). */
  allowed: number;
  ctrl: boolean;
  shift: boolean;
  /** Ferret's own drag out of this window, coming back in. */
  own: boolean;
}

/**
 * What dropping where the pointer is does: copy or move into `target`, recycle, add to the
 * Dropzone, or nothing: "keep" (Recycle Bin, but the app only lets its files be copied),
 * "here" (they're already in that folder), "search" (a search tab isn't a folder).
 */
export type DropAction = "copy" | "move" | "recycle" | "shelf" | "keep" | "here" | "search" | "none";

export interface ExternalDrag {
  count: number;
  target: string | null;
  action: DropAction;
  /** A copy only because the app the files come from allows nothing else. */
  copyOnly: boolean;
}

export const [externalDrag, setExternalDrag] = createSignal<ExternalDrag | null>(null);

let dropPaths: string[] = [];
let toldCursor = "";

function planDrop(e: DropEvent, x: number, y: number): { el: HTMLElement | null; drag: ExternalDrag } {
  const t = dropTargetAt(x, y, dropPaths);
  const dest = t?.path ?? activeTab()?.p ?? null;
  // From another app, what it allows decides first; Ferret's own drag back in works as inside the window.
  const allowed = e.own ? undefined : e.allowed;
  const keys = { ctrlKey: e.ctrl, shiftKey: e.shift };
  let action: DropAction;
  let copyOnly = false;
  if (!dest || !dropPaths.length) action = "none";
  else if (dest === SHELF) action = "shelf";
  else if (isRecycle(dest)) action = mayTakeAway(allowed) ? "recycle" : "keep";
  else if (isSearch(dest)) action = "search";
  else if (dropPaths.every((s) => samePath(parent(s), dest))) action = "here";
  else if (e.own && nativeCopies && !e.shift) action = "copy";
  else {
    action = dropOp(dropPaths, dest, keys, allowed);
    copyOnly = action === "copy" && dropOp(dropPaths, dest, keys) === "move";
  }
  return { el: t?.el ?? null, drag: { count: dropPaths.length, target: dest, action, copyOnly } };
}

/** The cursor Windows shows for the drag (filedrop.rs keeps what it was last told). */
function tellCursor(action: DropAction) {
  const op = action === "copy" || action === "shelf" ? "copy" : action === "move" || action === "recycle" ? "move" : "none";
  if (op === toldCursor) return;
  toldCursor = op;
  api.dropFeedback(op).catch(() => {});
}

listen<DropEvent>("ferret-drop", ({ payload: e }) => {
  if (e.kind === "leave") {
    setHover(null);
    setExternalDrag(null);
    dropPaths = [];
    return;
  }
  if (e.kind === "enter") toldCursor = "";
  if (e.paths.length) dropPaths = e.paths;
  const scale = window.devicePixelRatio || 1;
  const x = e.x / scale;
  const y = e.y / scale;
  const { el, drag: plan } = planDrop(e, x, y);
  if (e.kind !== "drop") {
    setHover(el);
    hoverTabs(x, y);
    tellCursor(plan.action);
    setExternalDrag(plan);
    return;
  }
  setHover(null);
  setExternalDrag(null);
  const paths = dropPaths;
  dropPaths = [];
  const dest = plan.target;
  const what = paths.length === 1 ? basename(paths[0]) : `${paths.length} items`;
  switch (plan.action) {
    case "shelf":
      return addToShelf(paths);
    case "recycle":
      return deleteItems(paths, false);
    case "keep":
      return toast(KEEP_NOTE);
    case "search":
      return void transferTo("copy", paths, dest!);
    case "copy":
    case "move":
      toast(`${plan.action === "move" ? "Moving" : "Copying"} ${what} to ${basename(dest!)}`);
      return void transferTo(plan.action, paths, dest!);
  }
});

const KEEP_NOTE = "The app these come from only lets them be copied, so they can't go to the Recycle Bin";

/**
 * The banner shown while files from another app are over the window, saying what a drop
 * there does: `lead`, then `name` in bold, then `tail`. Empty when it does nothing to say.
 */
export function externalDragLabel(d: ExternalDrag): { lead: string; name?: string; tail?: string } {
  const what = d.count > 1 ? `${d.count} items` : "here";
  const into = d.target ? basename(d.target) : "this folder";
  switch (d.action) {
    case "copy":
      return { lead: `Drop ${what} to copy into `, name: into, tail: d.copyOnly ? ": the app they come from only allows a copy" : "" };
    case "move":
      return { lead: `Drop ${what} to move into `, name: into };
    case "recycle":
      return { lead: `Drop ${what} to move to the `, name: "Recycle Bin" };
    case "shelf":
      return { lead: `Drop ${what} to add to the `, name: "Dropzone" };
    case "keep":
      return { lead: KEEP_NOTE };
    case "here":
      return { lead: "Already in ", name: into };
    case "search":
      return { lead: "Search results aren't a folder: open a folder to drop into it" };
    case "none":
      return { lead: "" };
  }
}

export const dragLabel = (d: DragState) => {
  const what = d.paths.length === 1 ? basename(d.paths[0]) : `${d.paths.length} items`;
  if (!d.target) return what;
  if (d.target === SHELF) return `Add ${what} to the shelf`;
  if (isRecycle(d.target)) return `Move ${what} to the Recycle Bin`;
  return `${d.op === "copy" ? (d.inArchive ? "Extract" : "Copy") : "Move"} ${what} to ${basename(d.target)}`;
};

/** Ensures tab ids still exist (tabs may close mid-drag). */
export const tabExists = (id: number) => tabs().some((t) => t.id === id);
