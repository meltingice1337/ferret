// Pointer-driven drag & drop. HTML5 DnD is unavailable inside WebView2 while the
// window accepts external file drops, so we track the mouse ourselves:
//  - inside the window: drop onto any element with `data-drop="<folder path>"`
//    (folders in the list, sidebar places, breadcrumbs, tabs);
//  - leaving the window: hand off to a native OLE drag (Explorer, desktop, other apps);
//  - files dragged in from Explorer arrive through Tauri's drag-drop event.

import { createSignal } from "solid-js";
import { startDrag } from "@crabnebula/tauri-plugin-drag";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api } from "./api";
import { basename, inRecycleBin, isRecycle, isRemote, isWithin, parent, samePath } from "./path";
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

const sameVolume = (a: string, b: string) => {
  const root = (p: string) => (p.startsWith("\\\\") ? p.split("\\").slice(2, 4).join("\\") : p.slice(0, 2)).toLowerCase();
  return root(a) === root(b);
};

function chooseOp(sources: string[], target: string, e: { ctrlKey: boolean; shiftKey: boolean }, inArchive: boolean): "copy" | "move" {
  if (inArchive) return "copy";
  if (e.ctrlKey) return "copy";
  if (e.shiftKey) return "move";
  return sameVolume(sources[0], target) ? "move" : "copy";
}

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

/**
 * Call from an item's mousedown. `getPaths` runs once the drag actually starts
 * (after selection has settled).
 */
export function beginDrag(down: MouseEvent, getPaths: () => string[], inArchive = false) {
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
    setDrag({ paths, x: e.clientX, y: e.clientY, target, op: target ? chooseOp(paths, target, e, inArchive) : "move", inArchive });
  };

  const onUp = (e: MouseEvent) => {
    const d = drag();
    cleanup();
    if (!started || !d) return;
    // Swallow the click that follows a drag.
    swallowNextClick();
    if (!d.target) return;
    if (d.target === SHELF) return addToShelf(paths);
    if (isRecycle(d.target)) return deleteItems(paths, e.shiftKey);
    const op = chooseOp(paths, d.target, e, inArchive);
    if (op === "move" && paths.every((p) => samePath(parent(p), d.target))) return;
    transferTo(op, paths, d.target);
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") cleanup();
    else if (drag()) setDrag((d) => d && { ...d, op: d.target ? chooseOp(paths, d.target, e, inArchive) : d.op });
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
    const dragged = d?.paths ?? paths;
    // A server's files aren't on this PC for other apps to take: small ones are downloaded first
    // (the drag starts once they're here), folders and big ones are copied to a folder in Ferret.
    if (dragged.some(isRemote)) {
      const shown = new Map(activeTab().entries[0]().map((e) => [e.path, e]));
      const items = dragged.map((p) => shown.get(p));
      if (items.some((e) => !e || e.isDir)) return toast("To use a server's folders in another app, copy them to a folder in Ferret first");
      if (items.reduce((n, e) => n + (e?.size ?? 0), 0) > DRAG_DOWNLOAD_MAX) return toast("Those files are big to drag out of a server: copy them to a folder in Ferret first");
    }
    try {
      // Archive entries need real files before other apps can take them (server files too).
      const shown = new Map(activeTab().entries[0]().map((e) => [e.path, e]));
      const real = (await Promise.all(dragged.map((p) => realPath({ ...shown.get(p), path: p, inArchive }, true)))).filter(Boolean) as string[];
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

export const [externalDrag, setExternalDrag] = createSignal<{ count: number; target: string | null } | null>(null);

getCurrentWebview().onDragDropEvent((ev) => {
  const p = ev.payload;
  if (p.type === "leave") {
    setHover(null);
    setExternalDrag(null);
    return;
  }
  const scale = window.devicePixelRatio || 1;
  const x = p.position.x / scale;
  const y = p.position.y / scale;
  const paths = "paths" in p ? p.paths : [];
  if (p.type === "enter" || p.type === "over") {
    const t = dropTargetAt(x, y, paths);
    setHover(t?.el ?? null);
    hoverTabs(x, y);
    setExternalDrag((prev) => ({ count: paths.length || prev?.count || 0, target: t?.path ?? activeTab()?.p ?? null }));
    return;
  }
  if (p.type === "drop") {
    const t = dropTargetAt(x, y, paths);
    setHover(null);
    setExternalDrag(null);
    if (t?.path === SHELF) return addToShelf(paths);
    const dest = t?.path ?? activeTab()?.p;
    if (dest && isRecycle(dest)) return deleteItems(paths, false);
    if (!dest || !paths.length) return;
    if (paths.every((s) => samePath(parent(s), dest))) return;
    const op = sameVolume(paths[0], dest) ? "move" : "copy";
    toast(`${op === "move" ? "Moving" : "Copying"} ${paths.length === 1 ? basename(paths[0]) : `${paths.length} items`} to ${basename(dest)}`);
    transferTo(op, paths, dest);
  }
});

export const dragLabel = (d: DragState) => {
  const what = d.paths.length === 1 ? basename(d.paths[0]) : `${d.paths.length} items`;
  if (!d.target) return what;
  if (d.target === SHELF) return `Add ${what} to the shelf`;
  if (isRecycle(d.target)) return `Move ${what} to the Recycle Bin`;
  return `${d.op === "copy" ? (d.inArchive ? "Extract" : "Copy") : "Move"} ${what} to ${basename(d.target)}`;
};

/** Ensures tab ids still exist (tabs may close mid-drag). */
export const tabExists = (id: number) => tabs().some((t) => t.id === id);
