import { ArchiveRestore, ArrowDown, ArrowUp, ChevronDown, ChevronRight, FileArchive, FolderOpen, LoaderCircle, PackageOpen, Pencil, Play, Plug, Search, Star, Trash2 } from "../lib/icons";
import { batch, createComputed, createEffect, createMemo, createResource, createSignal, For, Match, on, onCleanup, onMount, Show, Switch, untrack } from "solid-js";
import { Portal } from "solid-js/web";
import FileIcon from "./FileIcon";
import { PdfThumb } from "./preview/PdfPages";
import { addShellEntries, backgroundMenu, fromKeyboard, itemMenu, openKeyboardMenu } from "./menus";
import { api, fileUrl, noteVersion, thumbUrl } from "../lib/api";
import { beginDrag } from "../lib/drag";
import { formatDate, formatSize, kindOf, plural, timeAgo, typeLabel } from "../lib/format";
import { emptyBin, extractFromInside, isCut, newFolder, renamePath, restoreFromBin, startRename, targetsOf, visibleEntries } from "../lib/ops";
import { groupEntries, sortEntries, type Group } from "../lib/sortgroup";
import { entryFilter } from "../lib/filter";
import { archiveRoot, basename, folderLook, isRecycle, isSearch, isWithin, pathKey, samePath, searchQuery } from "../lib/path";
import { enter } from "../lib/motion";
import { openSearchPalette } from "./SearchPalette";
import { folderSizes, requestFolderSizes, sizeOrder, sizesApply, sizesEpoch, uncounted } from "../lib/foldersizes";
import { connectionOf, navigate, OFFLINE } from "../lib/state";
import { activeTab, ask, focusPane, goUp, holdKeys, jobsRunning, promptSaveSearch, savedSearch, openEntry, openMenu, persistSettings, refocusList, releaseKeys, reload, setSettings, setUi, settings, sortBy, takeTypeAhead, toast, ui, updateView, type Tab } from "../lib/state";
import type { Entry, ListColumn, SortBy } from "../lib/types";

export { GROUP_LABELS, SORT_LABELS } from "./menus";

type Row =
  | { kind: "header"; group: Group; collapsed: boolean; h: number }
  | { kind: "items"; items: Entry[]; h: number };

const HEADER_H = 34;
const scrollMemory = new Map<number, number>();
const THUMB_KINDS = new Set(["image", "video", "pdf", "doc", "sheet", "font", "audio"]);
const ICON_KINDS = new Set(["exe"]);

/** Thumbnail sizes are bucketed so the disk cache gets hits across zoom levels. */
const bucket = (px: number) => [64, 96, 128, 192, 256, 384, 512].find((b) => b >= px) ?? 512;

const NATIVE_IMG = /^(svg|png|jpe?g|gif|webp|avif|bmp|ico)$/;

/** Real file to thumbnail: archive entries (images/videos only) are extracted to the cache first. */
export function useThumbPath(entry: () => Entry) {
  const [real] = createResource(
    () => {
      const e = entry();
      if (!e.inArchive) return null;
      const k = kindOf(e);
      return k === "image" || k === "video" ? e.path : null;
    },
    (p) => api.archiveMaterialize(p).then((file) => ({ of: p, file })).catch(() => null),
  );
  // The resource keeps its last value when the entry changes (to one that isn't extracted, or
  // while the next one is): only use it for the entry it was made from.
  return () => {
    const e = entry();
    if (!e.inArchive) return e.path;
    const r = real();
    return r && r.of === e.path ? r.file : null;
  };
}

export function Thumb(props: { entry: Entry; size: number; fallbackSize?: number }) {
  // An edited file gets a new thumbnail (its URL carries the version).
  createComputed(() => !props.entry.isDir && !props.entry.inArchive && noteVersion(props.entry.path, props.entry.modified, props.entry.size));
  const [failed, setFailed] = createSignal(false);
  const kind = () => kindOf(props.entry);
  // Windows has no SVG thumbnailer; browser-native formats can fall back to the file itself.
  const [direct, setDirect] = createSignal(false);
  const native = () => NATIVE_IMG.test(props.entry.ext);
  const path = useThumbPath(() => props.entry);
  const usable = () => !!path() && !failed() && (THUMB_KINDS.has(kind()) || ICON_KINDS.has(kind()));
  // Very wide or tall pictures (a screenshot of a toolbar, a panorama) show as a cropped strip
  // in grid tiles: fetch enough pixels for that strip to be sharp, and show where it starts.
  const [shape, setShape] = createSignal<"" | "wide" | "tall">("");
  const [sharpSize, setSharpSize] = createSignal(0);
  const baseSize = () => bucket(props.size * (window.devicePixelRatio || 1));
  const src = () => (direct() || props.entry.ext === "svg"
    ? fileUrl(path()!)
    : thumbUrl(path()!, Math.max(baseSize(), sharpSize()), ICON_KINDS.has(kind())));
  createEffect(on(() => props.entry.path, () => { setFailed(false); setDirect(false); setShape(""); setSharpSize(0); }, { defer: true }));
  const checkShape = (img: HTMLImageElement) => {
    const ratio = img.naturalWidth / img.naturalHeight;
    if (ICON_KINDS.has(kind()) || (ratio <= 5 && ratio >= 1 / 5)) return;
    setShape(ratio > 1 ? "wide" : "tall");
    // The strip's short side is at least 12px on screen: ~30 thumbnail pixels keep it crisp.
    const long = 30 * Math.max(ratio, 1 / ratio);
    const want = [512, 768, 1024].find((b) => b >= long) ?? 1024;
    if (props.size >= 48 && !direct() && want > Math.max(baseSize(), sharpSize())) setSharpSize(want);
  };
  return (
    <Show when={usable()} fallback={
      <Show when={failed() && kind() === "pdf" && props.size >= 48 && !props.entry.inArchive} fallback={<FileIcon entry={props.entry} size={props.fallbackSize ?? Math.round(props.size * 0.62)} />}>
        <PdfThumb path={props.entry.path} size={props.size} fallback={<FileIcon entry={props.entry} size={Math.round(props.size * 0.62)} />} />
      </Show>
    }>
      <img
        class="thumb"
        classList={{ icon: ICON_KINDS.has(kind()), wide: shape() === "wide", tall: shape() === "tall" }}
        src={src()}
        loading="lazy"
        decoding="async"
        draggable={false}
        style={{ "max-width": `${props.size}px`, "max-height": `${props.size}px` }}
        onError={() => (native() && !direct() ? setDirect(true) : setFailed(true))}
        onLoad={(e) => {
          if (e.currentTarget.naturalWidth <= 1) return native() && !direct() ? setDirect(true) : setFailed(true);
          checkShape(e.currentTarget);
        }}
        alt=""
      />
    </Show>
  );
}

export default function FileView(props: { tab: Tab }) {
  const t = props.tab;
  const view = () => t.view[0]();
  /** Showing the Recycle Bin: items can only be previewed, restored or deleted. */
  const bin = () => isRecycle(t.p);
  /** A search opened as a tab: results from all over, each with the folder it's in. */
  const found = () => isSearch(t.p);
  const [width, setWidth] = createSignal(800);
  const [height, setHeight] = createSignal(600);
  /** Scrollbar width (0 when the list doesn't scroll): column headings leave the same room. */
  const [gutter, setGutter] = createSignal(0);
  const [scrollTop, setScrollTop] = createSignal(0);
  const [band, setBand] = createSignal<null | { x0: number; y0: number; x1: number; y1: number }>(null);
  let scroller!: HTMLDivElement;

  // "Always current": a search isn't a folder that can be watched, so while one is shown it asks
  // again every few seconds (the drive index answers in milliseconds and follows every change).
  createEffect(() => {
    if (!found()) return;
    const timer = window.setInterval(() => {
      if (!document.hidden && scroller?.offsetParent && !t.renaming[0]() && !t.loading[0]()) reload(t);
    }, 2000);
    onCleanup(() => clearInterval(timer));
  });

  // The listing eases in: from the right into a subfolder, from the left going up, a plain
  // fade for anywhere else and when switching tabs.
  onMount(() => enter(scroller, "fade", 150, "folder"));
  createEffect(on(() => t.path[0](), (p, prev) => {
    if (!prev || samePath(p, prev)) return;
    enter(scroller, isWithin(p, prev) ? "in" : isWithin(prev, p) ? "out" : "fade", 200, "folder");
  }, { defer: true }));

  // ---------- data pipeline: filter → sort → group → rows ----------

  const showFolderSizes = () => settings.folderSizes && sizesApply(t.p);
  const groups = createMemo(() => {
    const v = view();
    let list = t.entries[0]();
    if (!v.showHidden) list = list.filter((e) => !e.hidden);
    const match = entryFilter(t.filter[0]());
    if (match) list = list.filter(match);
    // Sorting by size puts folders in order once their sizes are counted (finished counts
    // only, so rows don't keep moving while totals grow).
    // The sizes are read untracked: the order follows `sizeOrder` (at most once a second while
    // counting), not every single size that arrives.
    const sorting = v.sortBy === "size" && showFolderSizes();
    if (sorting) sizeOrder();
    const sizeOf = sorting
      ? (e: Entry) => (e.isDir ? untrack(() => { const s = folderSizes[pathKey(e.path)]; return s?.done ? s.bytes : 0; }) : e.size)
      : undefined;
    // In the Recycle Bin the newest deletions come first, folders or not.
    return groupEntries(sortEntries(list, bin() ? { ...v, foldersFirst: false } : v, sizeOf), v);
  });

  // Folder sizes: this tab's folders are counted in the background. Asked again when the
  // folders change or something was marked out of date (not on every re-sort).
  const sizeTargets = createMemo(() => {
    if (!showFolderSizes()) return "";
    const hidden = view().showHidden;
    return t.entries[0]().filter((e) => e.isDir && !e.inArchive && (hidden || !e.hidden) && !uncounted(e.path)).map((e) => e.path).join("\n");
  });
  // While a file job runs, counting waits (the backend pauses it) and the list doesn't ask: it
  // would reload and re-ask after every deleted item. It asks once when the job is done.
  const sizesPaused = () => jobsRunning() > 0;
  // The folder itself too: it and the folders above it are counted in the background.
  const sizesDir = () => (showFolderSizes() ? t.p : "");
  createEffect(on([sizeTargets, sizesDir, sizesEpoch, sizesPaused], ([s, dir, , paused]) => {
    if (!paused) requestFolderSizes(t.id, s ? s.split("\n") : [], dir || undefined);
  }));
  onCleanup(() => requestFolderSizes(t.id, []));

  const itemH = () => (settings.density === "compact" ? 24 : 30);
  const cellW = () => view().iconSize + 34;
  const cols = () => (view().mode === "grid" ? Math.max(1, Math.floor((width() - 24) / cellW())) : 1);
  /** Grid cells share the row's leftover width, so there's no ragged gap on the right. */
  const slotW = () => (view().mode === "grid" ? (width() - 18) / cols() : cellW());
  const gridRowH = () => view().iconSize + 58;

  // Row objects are reused across recomputes when their content is unchanged, so <For>
  // keeps their DOM while scrolling or refreshing (and an inline rename survives).
  let rowCache = new Map<string, Row>();
  const rows = createMemo<Row[]>(() => {
    const out: Row[] = [];
    const next = new Map<string, Row>();
    const grouped = view().groupBy !== "none";
    const collapsed = t.collapsed[0]();
    const grid = view().mode === "grid";
    const c = cols();
    const ih = itemH(), gh = gridRowH();
    const keep = (key: string, fresh: Row, same: (prev: Row) => boolean) => {
      const prev = rowCache.get(key);
      const r = prev && same(prev) ? prev : fresh;
      next.set(key, r);
      out.push(r);
    };
    for (const g of groups()) {
      if (grouped) {
        const isCollapsed = collapsed.has(g.key);
        keep(`h:${g.key}`, { kind: "header", group: g, collapsed: isCollapsed, h: HEADER_H }, (p) => {
          if (p.kind !== "header" || p.collapsed !== isCollapsed || p.group.label !== g.label || p.group.items.length !== g.items.length) return false;
          p.group = g; // keep the latest items for "select group"
          return true;
        });
        if (isCollapsed) continue;
      }
      if (grid) {
        for (let i = 0; i < g.items.length; i += c) {
          const chunk = g.items.slice(i, i + c);
          keep(`g:${g.key}:${i}`, { kind: "items", items: chunk, h: gh },
            (p) => p.kind === "items" && p.h === gh && p.items.length === chunk.length && p.items.every((x, k) => x === chunk[k]));
        }
      } else {
        for (const e of g.items) keep(`i:${e.path}`, { kind: "items", items: [e], h: ih }, (p) => p.kind === "items" && p.h === ih && p.items[0] === e);
      }
    }
    rowCache = next;
    return out;
  });
  const rowIndex = createMemo(() => new Map(rows().map((r, i) => [r, i] as const)));
  const topOf = (r: Row) => layout().offs[rowIndex().get(r) ?? 0] ?? 0;
  /** The list row above (-1) or below (1) is selected too: a run of selected rows reads as
   *  one block (a group heading in between breaks it). */
  const selectedNeighbor = (r: Row, d: -1 | 1) => {
    const i = rowIndex().get(r);
    const n = i === undefined ? undefined : rows()[i + d];
    return n?.kind === "items" && isSel(n.items[0].path);
  };

  const layout = createMemo(() => {
    const offs: number[] = [];
    let acc = 0;
    for (const r of rows()) { offs.push(acc); acc += r.h; }
    return { offs, total: acc + 12 };
  });

  const ordered = createMemo(() => rows().flatMap((r) => (r.kind === "items" ? r.items : [])));
  createEffect(() => t.ordered[1](ordered()));

  // Filtering (or hiding hidden files) drops now-invisible items from the selection,
  // so actions like Delete only ever touch what's on screen.
  createEffect(on(ordered, (list) => {
    const shown = new Set(list.map((e) => pathKey(e.path)));
    const sel = untrack(() => t.selection[0]());
    if (sel.some((p) => !shown.has(pathKey(p)))) t.selection[1](sel.filter((p) => shown.has(pathKey(p))));
  }, { defer: true }));
  const rowOf = createMemo(() => {
    const m = new Map<string, number>();
    rows().forEach((r, i) => r.kind === "items" && r.items.forEach((e) => m.set(e.path, i)));
    return m;
  });

  const visible = createMemo(() => {
    const { offs } = layout();
    const rs = rows();
    const top = scrollTop() - 300;
    const bottom = scrollTop() + height() + 300;
    let lo = 0, hi = offs.length - 1, start = 0;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (offs[mid] + rs[mid].h < top) lo = mid + 1;
      else { start = mid; hi = mid - 1; }
    }
    const out: Row[] = [];
    for (let i = start; i < rs.length && offs[i] < bottom; i++) out.push(rs[i]);
    return out;
  });

  const stickyHeader = createMemo(() => {
    if (view().groupBy === "none") return null;
    const { offs } = layout();
    const st = scrollTop();
    let found: Extract<Row, { kind: "header" }> | null = null;
    rows().forEach((r, i) => { if (r.kind === "header" && offs[i] < st) found = r; });
    return found as Extract<Row, { kind: "header" }> | null;
  });

  // ---------- selection ----------

  const selSet = createMemo(() => new Set(t.selection[0]().map(pathKey)));
  const isSel = (p: string) => selSet().has(pathKey(p));
  const entryByPath = (p: string | null) => (p ? ordered().find((e) => e.path === p) : undefined);

  const selectOnly = (e: Entry) => batch(() => {
    t.selection[1]([e.path]); t.cursor[1](e.path); t.anchor[1](e.path);
  });

  const selectRange = (to: Entry) => {
    const list = ordered();
    const a = list.findIndex((x) => x.path === (t.anchor[0]() ?? to.path));
    const b = list.findIndex((x) => x.path === to.path);
    const [s, e] = a < b ? [a, b] : [b, a];
    batch(() => { t.selection[1](list.slice(Math.max(0, s), e + 1).map((x) => x.path)); t.cursor[1](to.path); });
  };

  // Slow click, as in Explorer: a click on the name of the item that already was the only one
  // selected, not followed by a second click (a double click), renames it.
  let slowTimer: number | undefined;
  let slowAt: { path: string; x: number; y: number } | null = null;
  const cancelSlowClick = () => { clearTimeout(slowTimer); slowAt = null; };
  onCleanup(cancelSlowClick);

  const onItemDown = (ev: MouseEvent, e: Entry) => {
    cancelSlowClick();
    if (t.renaming[0]() === e.path) return;
    const sel = t.selection[0]();
    if (ev.button === 0 && ev.detail === 1 && !ev.shiftKey && !ev.ctrlKey && !ev.altKey && !bin() && !e.inArchive && document.activeElement === scroller &&
        sel.length === 1 && samePath(sel[0], e.path) && (ev.target as HTMLElement).closest(".name, .cell-name")) {
      slowAt = { path: e.path, x: ev.clientX, y: ev.clientY };
    }
    // Middle-click: a folder opens in a new tab; on a file it starts autoscroll (lib/autoscroll.ts).
    if (ev.button === 1) { if (e.isDir) { ev.preventDefault(); openEntry(t, e, true); } return; }
    if (ev.button === 2) { if (!isSel(e.path)) selectOnly(e); return; }
    if (ev.button !== 0) return;
    scroller.focus({ preventScroll: true });
    if (ev.shiftKey) selectRange(e);
    else if (ev.ctrlKey) {
      batch(() => {
        t.selection[1]((s) => (isSel(e.path) ? s.filter((p) => pathKey(p) !== pathKey(e.path)) : [...s, e.path]));
        t.cursor[1](e.path); t.anchor[1](e.path);
      });
      return;
    } else if (!isSel(e.path)) selectOnly(e);
    else t.cursor[1](e.path);
    // A plain press on a selected item may become a drag of the whole selection.
    if (!bin()) beginDrag(ev, () => targetsOf(t).map((x) => x.path), e.inArchive);
  };

  const onItemClick = (ev: MouseEvent, e: Entry) => {
    if (ev.button !== 0 || ev.shiftKey || ev.ctrlKey) return;
    const slow = slowAt;
    if (slow && slow.path === e.path && ev.detail === 1 && Math.hypot(ev.clientX - slow.x, ev.clientY - slow.y) < 4) {
      slowTimer = window.setTimeout(() => {
        const sel = t.selection[0]();
        if (slowAt === slow && !t.renaming[0]() && sel.length === 1 && samePath(sel[0], e.path) && document.activeElement === scroller) startRename(t);
        slowAt = null;
      }, 500);
    } else slowAt = null;
    // Clicking one of several selected items (without dragging) narrows the selection.
    if (t.selection[0]().length > 1) selectOnly(e);
    if (!settings.doubleClickOpen) openEntry(t, e);
  };

  // ---------- rubber-band selection ----------

  const contentPoint = (e: MouseEvent) => {
    const r = scroller.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top + scroller.scrollTop - 4 };
  };

  const hitTest = (b: { x0: number; y0: number; x1: number; y1: number }): string[] => {
    const [left, right] = [Math.min(b.x0, b.x1), Math.max(b.x0, b.x1)];
    const [top, bottom] = [Math.min(b.y0, b.y1), Math.max(b.y0, b.y1)];
    const { offs } = layout();
    const out: string[] = [];
    rows().forEach((r, i) => {
      if (r.kind !== "items") return;
      const rt = offs[i], rb = offs[i] + r.h;
      if (rb < top || rt > bottom) return;
      if (view().mode === "list") {
        // Only the name column counts, like Explorer.
        if (left < Math.min(width() * 0.6, 420)) out.push(r.items[0].path);
        return;
      }
      r.items.forEach((e, c) => {
        const cl = 12 + c * slotW(), cr = cl + slotW() - 6;
        if (cr >= left && cl <= right && rt + 4 <= bottom && rb >= top) out.push(e.path);
      });
    });
    return out;
  };

  const startBand = (down: MouseEvent) => {
    const p0 = contentPoint(down);
    const base = down.ctrlKey ? t.selection[0]() : [];
    if (!down.ctrlKey && !down.shiftKey) t.selection[1]([]);
    let last: MouseEvent = down;
    let raf = 0;
    const update = () => {
      const p1 = contentPoint(last);
      const b = { x0: p0.x, y0: p0.y, x1: p1.x, y1: p1.y };
      if (Math.abs(b.x1 - b.x0) + Math.abs(b.y1 - b.y0) < 4) return;
      setBand(b);
      const hits = hitTest(b);
      const merged = down.ctrlKey ? [...new Set([...base, ...hits])] : hits;
      batch(() => { t.selection[1](merged); if (hits.length) t.cursor[1](hits[hits.length - 1]); });
    };
    // Auto-scroll while the pointer is near the top/bottom edge.
    const tick = () => {
      const r = scroller.getBoundingClientRect();
      if (last.clientY < r.top + 24) scroller.scrollTop -= 18;
      else if (last.clientY > r.bottom - 24) scroller.scrollTop += 18;
      update();
      raf = requestAnimationFrame(tick);
    };
    const move = (e: MouseEvent) => { last = e; };
    const up = () => {
      cancelAnimationFrame(raf);
      window.removeEventListener("mousemove", move, true);
      window.removeEventListener("mouseup", up, true);
      setBand(null);
    };
    window.addEventListener("mousemove", move, true);
    window.addEventListener("mouseup", up, true);
    raf = requestAnimationFrame(tick);
  };

  // ---------- scrolling ----------

  const revealCursor = () => {
    const p = t.cursor[0]();
    const i = p ? rowOf().get(p) : undefined;
    if (i === undefined) return;
    const top = layout().offs[i];
    const h = rows()[i].h;
    const pad = view().groupBy !== "none" ? HEADER_H : 0;
    if (top - pad < scroller.scrollTop) scroller.scrollTop = Math.max(0, top - pad - 4);
    else if (top + h > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = top + h - scroller.clientHeight + 8;
  };

  // Switching list ⇄ grid (or icon size) changes every row's position: keep the current item in view.
  createEffect(on(() => [view().mode, view().iconSize], () => {
    requestAnimationFrame(() => {
      const p = t.cursor[0]();
      const i = p ? rowOf().get(p) : undefined;
      // Center it, rather than leaving it pinned to an edge.
      if (i !== undefined) scroller.scrollTop = Math.max(0, layout().offs[i] - scroller.clientHeight / 2 + rows()[i].h / 2);
    });
  }, { defer: true }));

  /** Takes the keyboard after a folder loads, unless Ctrl+K, a dialog or a menu opened over
   *  it meanwhile (typing there must not jump around the list). */
  const takeFocus = () => {
    // In split view only the pane in use takes it (both lists load at once).
    if (t !== activeTab()) return;
    if (!document.activeElement?.closest(".palette, .modal, .menu, .ql")) scroller.focus({ preventScroll: true });
  };
  createEffect(on(() => t.reveal[0](), (_, prev) => {
    if (prev !== undefined && !t.cursor[0]()) scroller.scrollTop = 0;
    requestAnimationFrame(() => { revealCursor(); if (!t.renaming[0]()) takeFocus(); });
  }, { defer: true }));

  onMount(() => {
    const ro = new ResizeObserver(() => requestAnimationFrame(() => {
      setWidth(scroller.clientWidth);
      setHeight(scroller.clientHeight);
      setGutter(scroller.offsetWidth - scroller.clientWidth);
    }));
    ro.observe(scroller);
    onCleanup(() => ro.disconnect());
    scroller.scrollTop = scrollMemory.get(t.id) ?? 0;
    setScrollTop(scroller.scrollTop);
    takeFocus();
  });
  onCleanup(() => scrollMemory.set(t.id, scroller.scrollTop));

  // ---------- keyboard (navigation; file actions are global in App) ----------

  let typeBuf = "";
  let typeTimer: number | undefined;

  const moveCursor = (target: Entry | undefined, ev: KeyboardEvent) => {
    if (!target) return;
    if (ev.shiftKey) selectRange(target);
    else if (ev.ctrlKey) t.cursor[1](target.path);
    else selectOnly(target);
    requestAnimationFrame(revealCursor);
  };

  const gridVertical = (cur: Entry, dir: 1 | -1): Entry | undefined => {
    const rs = rows();
    const ri = rowOf().get(cur.path);
    if (ri === undefined) return;
    const row = rs[ri] as Extract<Row, { kind: "items" }>;
    const col = row.items.indexOf(cur);
    for (let i = ri + dir; i >= 0 && i < rs.length; i += dir) {
      const r = rs[i];
      if (r.kind === "items") return r.items[Math.min(col, r.items.length - 1)];
    }
  };

  const onKeyDown = (ev: KeyboardEvent) => {
    if (t.renaming[0]()) return;
    const list = ordered();
    if (!list.length && ev.key !== "Backspace") return;
    const cur = entryByPath(t.cursor[0]());
    const idx = cur ? list.indexOf(cur) : -1;
    const grid = view().mode === "grid";
    const step = grid ? cols() : 1;
    const page = Math.max(1, Math.floor(scroller.clientHeight / (grid ? gridRowH() : itemH())) - 1) * step;

    switch (ev.key) {
      case "ArrowDown":
        ev.preventDefault();
        if (grid && cur) return moveCursor(gridVertical(cur, 1), ev);
        return moveCursor(list[Math.min(list.length - 1, idx + 1)], ev);
      case "ArrowUp":
        ev.preventDefault();
        if (grid && cur) return moveCursor(gridVertical(cur, -1), ev);
        return moveCursor(list[Math.max(0, idx < 0 ? 0 : idx - 1)], ev);
      case "ArrowRight":
        if (!grid || ev.altKey) return;
        ev.preventDefault();
        return moveCursor(list[Math.min(list.length - 1, idx + 1)], ev);
      case "ArrowLeft":
        if (!grid || ev.altKey) return;
        ev.preventDefault();
        return moveCursor(list[Math.max(0, idx - 1)], ev);
      case "Home": ev.preventDefault(); return moveCursor(list[0], ev);
      case "End": ev.preventDefault(); return moveCursor(list[list.length - 1], ev);
      case "PageDown": ev.preventDefault(); return moveCursor(list[Math.min(list.length - 1, idx + page)], ev);
      case "PageUp": ev.preventDefault(); return moveCursor(list[Math.max(0, idx - page)], ev);
      case "Enter": {
        if (ev.altKey) return; // Alt+Enter = properties (global)
        ev.preventDefault();
        const targets = targetsOf(t);
        const dir = targets.find((e) => e.isDir);
        if (dir && targets.length === 1) return openEntry(t, dir, ev.ctrlKey);
        for (const e of targets.filter((e) => !e.isDir).slice(0, 10)) openEntry(t, e);
        targets.filter((e) => e.isDir).forEach((d) => openEntry(t, d, true));
        return;
      }
      case "Backspace": ev.preventDefault(); return goUp(t);
      case "Escape":
        if (t.filter[0]() || ui.filterOpen) { t.filter[1](""); setUi("filterOpen", false); }
        else batch(() => { t.selection[1]([]); t.anchor[1](null); });
        return;
      case " ":
        if (typeBuf) break; // part of a type-to-jump name
        ev.preventDefault();
        if (ev.ctrlKey) {
          if (cur) t.selection[1]((s) => (isSel(cur.path) ? s.filter((p) => pathKey(p) !== pathKey(cur.path)) : [...s, cur.path]));
          return;
        }
        if (cur) { if (!isSel(cur.path)) selectOnly(cur); setUi("quickLook", true); }
        return;
    }
    if (ev.ctrlKey && ev.key.toLowerCase() === "a") {
      ev.preventDefault();
      t.selection[1](list.map((e) => e.path));
      return;
    }
    if (ev.key.length === 1 && !ev.ctrlKey && !ev.altKey && !ev.metaKey) {
      typeBuf += ev.key.toLowerCase();
      clearTimeout(typeTimer);
      typeTimer = window.setTimeout(() => (typeBuf = ""), 800);
      const start = typeBuf.length === 1 ? idx + 1 : Math.max(0, idx);
      const hit = [...list.slice(start), ...list.slice(0, start)].find((e) => e.name.toLowerCase().startsWith(typeBuf));
      if (hit) { selectOnly(hit); requestAnimationFrame(revealCursor); }
    }
  };

  // ---------- menus ----------

  const onItemMenu = (ev: MouseEvent, e: Entry) => {
    ev.preventDefault();
    ev.stopPropagation();
    if (fromKeyboard(ev)) return keyMenu();
    if (!isSel(e.path)) selectOnly(e);
    const targets = targetsOf(t);
    openMenu(ev.clientX, ev.clientY, itemMenu(t, e, targets, ev.clientX, ev.clientY));
    if (!targets.some((x) => x.inArchive)) addShellEntries(t, targets.map((x) => x.path), false);
  };

  // The Menu key sends a contextmenu event to the focused list (not to the selected item):
  // it gets the same menu as Shift+F10, unless the key press already opened one.
  let keyMenuAt = 0;
  const keyMenu = () => {
    if (ui.menu || performance.now() - keyMenuAt < 300) return;
    keyMenuAt = performance.now();
    openKeyboardMenu(t);
  };

  const onBackgroundMenu = (ev: MouseEvent) => {
    ev.preventDefault();
    if (fromKeyboard(ev)) return keyMenu();
    t.selection[1]([]);
    openMenu(ev.clientX, ev.clientY, backgroundMenu(t, ev.clientX, ev.clientY));
    addShellEntries(t, [], true);
  };

  // ---------- render ----------

  // Columns drop out as the list narrows (Type first, then Modified), or when the user hides
  // them (right-click the headings). Widths come from dragging the lines between headings.
  const hiddenCol = (k: ListColumn) => settings.hiddenColumns.includes(k);
  const showOrigin = () => (bin() || found()) && width() >= 600;
  const showType = () => !bin() && width() >= 600 && !hiddenCol("type");
  const showDate = () => width() >= 430 && !hiddenCol("modified");
  const showSize = () => !hiddenCol("size");
  const colW = (k: ListColumn) => settings.columnWidths[k] ?? COLUMN_WIDTHS[k];
  const fixedCols = () => ([showDate() && "modified", showType() && "type", showSize() && "size"].filter(Boolean)) as ListColumn[];
  // Name keeps 160 px, except in a narrow pane (a split view in a small window), where it gives
  // way so the headings don't run into each other.
  const columns = () => [width() < 360 ? "minmax(48px, 1fr)" : "minmax(160px, 1fr)", showOrigin() && "minmax(120px, 0.9fr)", ...fixedCols().map((k) => `${colW(k)}px`)].filter(Boolean).join(" ");

  /** Dragging the line at a column's left edge moves that line: the column on its left grows
   *  as this one shrinks (or Name, which takes the rest). */
  const resizeColumn = (e: MouseEvent, k: ListColumn) => {
    e.preventDefault();
    e.stopPropagation();
    const order = fixedCols();
    const left = order[order.indexOf(k) - 1];
    const start = { ...settings.columnWidths };
    const room = width() - 20 - (showOrigin() ? 280 : 160) - order.reduce((a, c) => a + colW(c), 0);
    const x0 = e.clientX;
    document.body.classList.add("col-resizing");
    const move = (m: MouseEvent) => {
      const dx = m.clientX - x0;
      const next = { ...start };
      if (left) {
        const d = Math.max(MIN_COL - colW(left), Math.min(colW(k) - MIN_COL, dx));
        next[left] = start[left] + d;
        next[k] = start[k] - d;
      } else next[k] = Math.max(MIN_COL, Math.min(start[k] + Math.max(0, room), start[k] - dx));
      setSettings("columnWidths", next);
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("col-resizing");
      persistSettings();
      // The grip stops at a column's minimum width, so the release can land on a heading, and
      // its click would sort the list.
      const swallow = (c: MouseEvent) => { c.stopPropagation(); c.preventDefault(); };
      window.addEventListener("click", swallow, { capture: true, once: true });
      setTimeout(() => window.removeEventListener("click", swallow, true));
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };
  const resetColumns = () => { setSettings("columnWidths", { ...COLUMN_WIDTHS }); persistSettings(); };
  const headingsMenu = (e: MouseEvent) => {
    e.preventDefault();
    // The menu stays open while columns are switched on and off, so several take one visit.
    const toggle = (k: ListColumn) => {
      setSettings("hiddenColumns", (h) => (h.includes(k) ? h.filter((x) => x !== k) : [...h, k]));
      persistSettings();
      setUi("menu", "items", items());
    };
    const col = (label: string, k: ListColumn) => ({ label, checked: !hiddenCol(k), keepOpen: true, action: () => toggle(k) });
    const items = () => [
      { heading: true, label: "Columns" },
      { label: "Name", checked: true, disabled: true },
      col(bin() ? "Date deleted" : "Modified", "modified"),
      ...(bin() ? [] : [col("Type", "type")]),
      col("Size", "size"),
      { separator: true },
      { label: "Reset column widths", action: resetColumns },
    ];
    openMenu(e.clientX, e.clientY, items());
  };
  const dropAttr = (e: Entry) => (e.isDir && !e.inArchive && !bin() ? e.path : undefined);
  /** Size column: a file's size, or a folder's counted size (dimmed while counting; an
   *  earlier count with a spinner while it's counted again). */
  const sizeCell = (e: Entry): { text: string; counting?: boolean; refreshing?: boolean; title?: string } => {
    if (!e.isDir || bin()) return { text: formatSize(e.size) };
    if (!showFolderSizes() || e.inArchive) return { text: "" };
    const why = uncounted(e.path);
    if (why) return { text: "", title: why };
    const s = folderSizes[pathKey(e.path)];
    // Paused during a file job: the last known size, without "counting" marks.
    if (sizesPaused()) return s?.done ? { text: formatSize(s.bytes), title: "Counted again once the current operation is done" } : { text: "" };
    if (!s) return { text: "…", counting: true, title: "Counting…" };
    if (!s.done) return { text: formatSize(s.bytes), counting: true, title: "Counting…" };
    const details = [s.files !== null && plural(s.files, "file"), s.partial && "some folders inside couldn't be read"];
    const counted = `counted ${timeAgo(s.countedAt)}`;
    return {
      text: formatSize(s.bytes),
      refreshing: s.refreshing,
      title: [s.refreshing && "Updating…", ...details, counted].filter(Boolean).join(" · "),
    };
  };

  return (
    // A click or the keyboard in this view makes it the pane in use (split view).
    <div class="fileview" classList={{ grid: view().mode === "grid", bin: bin() }} data-tab={t.id} onFocusIn={() => focusPane(t)}>
      <Show when={bin()}><BinBar tab={t} /></Show>
      <Show when={found()}><SearchBar tab={t} /></Show>
      <Show when={insideArchive(t)}>{(archive) => <ArchiveBar tab={t} archive={archive()} />}</Show>
      <Show when={view().mode === "list"}>
        <div class="colheads" style={{ "grid-template-columns": columns(), "padding-right": `${10 + gutter()}px` }}
          onContextMenu={headingsMenu}>
          <ColHead tab={t} k="name" label="Name" />
          <Show when={showOrigin()}><span class="colhead static">{bin() ? "Original location" : "Folder"}</span></Show>
          <Show when={showDate()}>
            <ColHead tab={t} k="modified" label={bin() ? "Date deleted" : "Modified"} grip={(e) => resizeColumn(e, "modified")} onReset={resetColumns} />
          </Show>
          <Show when={showType()}><ColHead tab={t} k="type" label="Type" grip={(e) => resizeColumn(e, "type")} onReset={resetColumns} /></Show>
          <Show when={showSize()}><ColHead tab={t} k="size" label="Size" right grip={(e) => resizeColumn(e, "size")} onReset={resetColumns} /></Show>
        </div>
      </Show>
      <div
        class="files"
        ref={scroller}
        tabIndex={0}
        data-drop={found() || t.p.match(/\.(zip|7z|rar|tar|gz|xz|zst|bz2|tgz|jar)(\\|$)/i) ? undefined : t.p}
        onScroll={() => setScrollTop(scroller.scrollTop)}
        onKeyDown={(e) => { cancelSlowClick(); onKeyDown(e); }}
        onMouseDown={(e) => {
          const el = e.target as HTMLElement;
          if (!el.closest(".row, .cell")) cancelSlowClick();
          if (e.button === 0 && (el === scroller || el.classList.contains("files-inner") || el.classList.contains("grid-row"))) {
            scroller.focus({ preventScroll: true });
            startBand(e);
          }
        }}
        onContextMenu={onBackgroundMenu}
        onWheel={(e) => {
          if (e.ctrlKey && view().mode === "grid") {
            e.preventDefault();
            updateView(t, { iconSize: Math.max(48, Math.min(256, view().iconSize + (e.deltaY < 0 ? 16 : -16))) });
          } else if (e.ctrlKey && view().mode === "list" && e.deltaY < 0) {
            e.preventDefault();
            updateView(t, { mode: "grid", iconSize: 64 });
          }
        }}
      >
        <div class="files-inner" style={{ height: `${layout().total}px` }}>
          <For each={visible()}>
            {(row) => (
              <Switch>
                <Match when={row.kind === "header" && (row as Extract<Row, { kind: "header" }>)}>
                  {(h) => <GroupHeader tab={t} row={h()} top={topOf(row)} />}
                </Match>
                <Match when={view().mode === "list" && row.kind === "items" && row.items[0]}>
                  {(e) => (
                    <div
                      class="row"
                      classList={{
                        selected: isSel(e().path), cursor: t.cursor[0]() === e().path, dim: e().hidden, cut: isCut(e().path),
                        "join-up": isSel(e().path) && selectedNeighbor(row, -1), "join-down": isSel(e().path) && selectedNeighbor(row, 1),
                      }}
                      style={{ transform: `translateY(${topOf(row)}px)`, height: `${itemH()}px`, "grid-template-columns": columns() }}
                      data-drop={dropAttr(e())}
                      onMouseDown={(ev) => onItemDown(ev, e())}
                      onClick={(ev) => onItemClick(ev, e())}
                      onDblClick={() => settings.doubleClickOpen && t.renaming[0]() !== e().path && openEntry(t, e())}
                      onContextMenu={(ev) => onItemMenu(ev, e())}
                      title={e().name}
                    >
                      <span class="c-name">
                        <Show when={kindOf(e()) === "exe" && !e().inArchive} fallback={<FileIcon entry={e()} />}>
                          <span class="ficon"><Thumb entry={e()} size={16} fallbackSize={16} /></span>
                        </Show>
                        <Show when={t.renaming[0]() === e().path} fallback={<span class="name">{e().name}</span>}>
                          <RenameInput tab={t} entry={e()} />
                        </Show>
                      </span>
                      <Show when={showOrigin()}>
                        {/* Search results cut the path from the left, keeping the folder names that tell them apart
                            (the left-to-right marks keep "C:\a\b" from reading "a\b\C:" in the right-to-left box). */}
                        <span class="c-type c-origin" classList={{ tail: found() || bin() }} title={e().origin}>{found() || bin() ? `‎${e().origin}‎` : e().origin}</span>
                      </Show>
                      <Show when={showDate()}><span class="c-date">{formatDate(e().modified)}</span></Show>
                      <Show when={showType()}><span class="c-type">{typeLabel(e())}</span></Show>
                      <Show when={showSize()}>
                        <span class="c-size" classList={{ counting: !!sizeCell(e()).counting }} title={sizeCell(e()).title}>
                          <Show when={sizeCell(e()).refreshing}><LoaderCircle size={10} class="spin size-refresh" /></Show>
                          {sizeCell(e()).text}
                        </span>
                      </Show>
                    </div>
                  )}
                </Match>
                <Match when={view().mode === "grid"}>
                  <div class="grid-row" style={{ transform: `translateY(${topOf(row)}px)`, height: `${gridRowH()}px` }}>
                    <For each={(row as Extract<Row, { kind: "items" }>).items}>
                      {(e) => (
                        <div
                          class="cell"
                          classList={{ selected: isSel(e.path), cursor: t.cursor[0]() === e.path, dim: e.hidden, cut: isCut(e.path) }}
                          style={{ width: `${slotW() - 6}px` }}
                          data-drop={dropAttr(e)}
                          onMouseDown={(ev) => onItemDown(ev, e)}
                          onClick={(ev) => onItemClick(ev, e)}
                          onDblClick={() => settings.doubleClickOpen && t.renaming[0]() !== e.path && openEntry(t, e)}
                          onContextMenu={(ev) => onItemMenu(ev, e)}
                          title={found() ? `${e.name}\nIn ${e.origin}\n${formatDate(e.modified)}` : bin() ? `${e.name}\nFrom ${e.origin}\nDeleted ${formatDate(e.modified)} · ${formatSize(e.size)}` : `${e.name}\n${typeLabel(e)}${e.isDir ? (folderSizes[pathKey(e.path)]?.done ? ` · ${formatSize(folderSizes[pathKey(e.path)].bytes)}` : "") : ` · ${formatSize(e.size)}`}\n${formatDate(e.modified)}`}
                        >
                          <div class="cell-icon" style={{ height: `${view().iconSize}px` }}>
                            <Thumb entry={e} size={view().iconSize} />
                            <Show when={kindOf(e) === "video" && !e.inArchive}>
                              <span class="vid-badge"><Play size={Math.max(10, Math.round(view().iconSize / 9))} fill="currentColor" /></span>
                            </Show>
                            <Show when={!e.isDir && e.ext && !THUMB_KINDS.has(kindOf(e))}>
                              <span class="ext-badge">{e.ext}</span>
                            </Show>
                          </div>
                          <Show when={t.renaming[0]() === e.path} fallback={<div class="cell-name">{e.name}</div>}>
                            <RenameInput tab={t} entry={e} grid />
                          </Show>
                        </div>
                      )}
                    </For>
                  </div>
                </Match>
              </Switch>
            )}
          </For>
          <Show when={band()}>
            {(b) => (
              <div class="band" style={{
                left: `${Math.min(b().x0, b().x1)}px`, top: `${Math.min(b().y0, b().y1)}px`,
                width: `${Math.abs(b().x1 - b().x0)}px`, height: `${Math.abs(b().y1 - b().y0)}px`,
              }} />
            )}
          </Show>
        </div>
        <Show when={!t.loading[0]() && ordered().length === 0 && rows().length === 0}>
          <Show when={t.error[0]() !== OFFLINE} fallback={
            // A server folder whose connection isn't open: only a click connects.
            <div class="files-empty files-offline">
              <span>Not connected to {connectionOf(t.p)?.name ?? "this server"}</span>
              <button class="btn accent" onClick={() => navigate(t, t.p, { history: "none" })}><Plug size={14} />Connect</button>
            </div>
          }>
            <div class="files-empty">
              {t.filter[0]() ? `Nothing here matches “${t.filter[0]()}”` : t.entries[0]().length ? "Only hidden items here. Press Ctrl+H to show them." : bin() ? "The Recycle Bin is empty" : found() ? "Nothing matches this search right now" : "This folder is empty"}
            </div>
          </Show>
        </Show>
      </div>
      <Show when={stickyHeader()}>
        {(row) => (
          <div class="sticky-head" style={{ top: view().mode === "list" ? "32px" : "0" }}>
            <GroupHeader tab={t} row={row()} top={0} sticky />
          </div>
        )}
      </Show>
    </div>
  );
}

function RenameInput(props: { tab: Tab; entry: Entry; grid?: boolean }) {
  let el!: HTMLTextAreaElement;
  let busy = false;
  let closed = false;
  const [error, setError] = createSignal<string | null>(null);
  const focusList = refocusList;
  const close = () => {
    closed = true;
    if (props.tab.renaming[0]() === props.entry.path) props.tab.renaming[1](null);
    focusList();
    releaseKeys();
  };
  const selectStem = () => {
    const dot = props.entry.isDir ? -1 : el.value.lastIndexOf(".");
    el.setSelectionRange(0, dot > 0 ? dot : el.value.length);
  };

  /** `keepOpen`: on failure keep editing with the error shown (Enter); otherwise revert (blur). */
  const commit = async (keepOpen: boolean) => {
    if (busy || closed) return;
    // Windows drops trailing dots and spaces from names; trim them first, as Explorer does.
    const value = el.value.replace(/[\r\n]/g, "").trim().replace(/[. ]+$/, "");
    if (!value || value === props.entry.name) return close();
    // A name Windows won't take: say so, without asking about the extension first.
    const bad = /[<>:"/\\|?*]/.exec(value);
    if (bad) {
      setError(`A name can't contain ${bad[0]}`);
      if (keepOpen) { el.focus(); selectStem(); }
      else { toast(`Couldn't rename: a name can't contain ${bad[0]}`, "error"); close(); }
      return;
    }
    busy = true;
    // Like Explorer: a changed extension can leave the file opening in the wrong app, or none.
    const ext = (n: string) => { const d = n.lastIndexOf("."); return d > 0 ? n.slice(d).toLowerCase() : ""; };
    if (!props.entry.isDir && ext(value) !== ext(props.entry.name)) {
      const from = ext(props.entry.name), to = ext(value);
      const ok = await ask({
        title: "Change the file extension?",
        message: to
          ? `Changing “${from || "no extension"}” to “${to}” can make the file open in the wrong app, or not open at all. Its contents stay the same.`
          : `Removing “${from}” can stop Windows from knowing which app opens this file. Its contents stay the same.`,
        confirmLabel: to ? `Use ${to}` : "Remove it",
      });
      if (ok === null) {
        // Back to editing, with the name as typed.
        busy = false;
        if (el.isConnected) { el.focus(); selectStem(); } else close();
        return;
      }
    }
    holdKeys();
    try {
      const to = await renamePath(props.entry.path, value);
      closed = true;
      const t = props.tab;
      if (t.renaming[0]() === props.entry.path) t.renaming[1](null);
      await reload(t);
      batch(() => { t.selection[1]([to]); t.cursor[1](to); t.anchor[1](to); t.reveal[1]((n) => n + 1); });
      focusList();
      releaseKeys();
    } catch (e) {
      releaseKeys(true);
      const msg = String(e).replace(/ \(os error \d+\)$/, "");
      if (keepOpen && el.isConnected) {
        setError(msg);
        el.focus();
        selectStem();
      } else {
        toast(`Couldn't rename: ${msg}`, "error");
        close();
      }
    } finally {
      busy = false;
    }
  };

  return (
    <span class="rename-wrap" classList={{ grid: props.grid }}>
      <textarea
        ref={(node) => {
          el = node;
          queueMicrotask(() => {
            el.focus();
            // Select the name without its extension, like Explorer.
            selectStem();
            if (props.grid) { el.style.height = "auto"; el.style.height = `${el.scrollHeight}px`; }
            // Replay anything typed before the box existed (it replaces the selected part).
            const ahead = takeTypeAhead();
            if (ahead && (ahead.text || ahead.enter)) {
              if (ahead.text) {
                const dot = props.entry.isDir ? -1 : props.entry.name.lastIndexOf(".");
                el.value = ahead.text + (dot > 0 ? props.entry.name.slice(dot) : "");
                el.setSelectionRange(ahead.text.length, ahead.text.length);
              }
              if (ahead.enter) commit(true);
            }
          });
        }}
        class="rename"
        classList={{ grid: props.grid, invalid: !!error() }}
        rows={1}
        spellcheck={false}
        value={props.entry.name}
        onMouseDown={(e) => e.stopPropagation()}
        onDblClick={(e) => e.stopPropagation()}
        onInput={(e) => {
          const v = e.currentTarget.value.trim();
          const bad = /[<>:"/\\|?*]/.exec(v);
          const clash = !bad && v.toLowerCase() !== props.entry.name.toLowerCase() &&
            props.tab.entries[0]().some((x) => x.name.toLowerCase() === v.toLowerCase());
          setError(bad ? `A name can't contain ${bad[0]}` : clash ? `“${v}” already exists here` : null);
          if (props.grid) { e.currentTarget.style.height = "auto"; e.currentTarget.style.height = `${e.currentTarget.scrollHeight}px`; }
        }}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter" || e.key === "Tab") { e.preventDefault(); commit(true); }
          else if (e.key === "Escape") { e.preventDefault(); close(); }
          else if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "n") { e.preventDefault(); commit(false); newFolder(props.tab); }
        }}
        onBlur={() => {
          // A re-render can remove the input; only a real focus change commits.
          setTimeout(() => { if (el.isConnected && document.activeElement !== el && !busy) commit(false); }, 0);
        }}
      />
      <Show when={error()}>
        {(msg) => {
          const r = el.getBoundingClientRect();
          // Portal: rows are transformed, which would anchor position:fixed to the row.
          return <Portal><span class="rename-error" role="alert" style={{ left: `${r.left}px`, top: `${r.bottom + 4}px` }}>{msg()}</span></Portal>;
        }}
      </Show>
    </span>
  );
}

/** Recycle Bin summary and actions above the list. */
function BinBar(props: { tab: Tab }) {
  const t = props.tab;
  const all = () => t.entries[0]();
  const selected = () => t.selection[0]();
  const total = () => all().reduce((a, e) => a + e.size, 0);
  return (
    <div class="bin-bar">
      <Trash2 size={15} />
      <span class="bin-count">{all().length ? `${plural(all().length, "item")} · ${formatSize(total())}` : "Empty"}</span>
      <span class="bin-hint">Kept until you empty the bin</span>
      <button class="btn" disabled={!all().length}
        onClick={() => restoreFromBin(selected().length ? [...selected()] : visibleEntries(t).map((e) => e.path))}>
        <ArchiveRestore size={14} />{selected().length ? `Restore ${plural(selected().length, "item")}` : "Restore all"}
      </button>
      <button class="btn warn" disabled={!all().length} onClick={() => emptyBin()}><Trash2 size={14} />Empty Recycle Bin</button>
    </div>
  );
}

/** The archive the tab is browsing (inside it, or a folder in it), if it is: its items come
 *  from the archive. (An empty one has no items to tell by.) */
function insideArchive(t: Tab): string | null {
  const root = archiveRoot(t.p);
  if (!root || t.loading[0]()) return null;
  const items = t.entries[0]();
  return !items.length || items.some((e) => e.inArchive) ? root : null;
}

/** Inside an archive, as Explorer's "Extract all": extracting it (or what's selected) without
 *  going back out to it. Then the extracted files are shown. */
function ArchiveBar(props: { tab: Tab; archive: string }) {
  const t = props.tab;
  const selected = () => t.selection[0]();
  // Its name and the folder in it are in the tab and the path bar already.
  return (
    <div class="bin-bar archive-bar">
      <FileArchive size={15} />
      <span class="bin-count">{typeLabel(folderLook(props.archive))}</span>
      <span class="bin-hint">Read-only: extract it to make changes</span>
      <Show when={selected().length}>
        <button class="btn" onClick={() => extractFromInside(t, props.archive, "selected")}>
          <PackageOpen size={14} />Extract {plural(selected().length, "selected item")}…
        </button>
      </Show>
      <button class="btn" onClick={() => extractFromInside(t, props.archive, "to")}><FolderOpen size={14} />Extract to…</button>
      <button class="btn accent" title={`Next to ${basename(props.archive)} (in a folder of its own unless it holds a single folder)`}
        onClick={() => extractFromInside(t, props.archive, "all")}>
        <PackageOpen size={14} />Extract all
      </button>
    </div>
  );
}

/** Most results a search tab lists (the backend's limit). */
const SEARCH_TAB_LIMIT = 2000;

function SearchBar(props: { tab: Tab }) {
  const t = props.tab;
  const q = () => searchQuery(t.p);
  const n = () => t.entries[0]().length;
  const saved = () => savedSearch(q());
  return (
    <div class="bin-bar search-bar">
      <Search size={15} />
      <span class="bin-count" title="Always current · F5 searches again">{t.loading[0]() ? "Searching…" : n() >= SEARCH_TAB_LIMIT ? `First ${SEARCH_TAB_LIMIT.toLocaleString()} matches` : n() === 1 ? "1 match" : `${n().toLocaleString()} matches`}</span>
      <code class="search-bar-query" title={q()}>{q()}</code>
      <span class="bin-hint">Always current · F5 searches again</span>
      <button class="btn" onClick={() => openSearchPalette(q())}><Pencil size={14} />Edit search</button>
      <button class="btn" onClick={() => promptSaveSearch(q())}>
        <Star size={14} fill={saved() ? "currentColor" : "none"} />{saved() ? "Rename" : "Save to sidebar"}
      </button>
    </div>
  );
}

const COLUMN_WIDTHS: Record<ListColumn, number> = { modified: 150, type: 140, size: 90 };
const MIN_COL = 64;

function ColHead(props: { tab: Tab; k: SortBy; label: string; right?: boolean; grip?: (e: MouseEvent) => void; onReset?: () => void }) {
  const v = () => props.tab.view[0]();
  return (
    // A click sorts without taking the focus from the list (as in Explorer), so the keys still act
    // on the files; Tab still reaches the headings.
    <button class="colhead" classList={{ active: v().sortBy === props.k, right: props.right }}
      onMouseDown={(e) => e.preventDefault()}
      onClick={() => { sortBy(props.tab, props.k); refocusList(); }}>
      <Show when={props.grip}>
        <span class="col-grip" title="Drag to resize · double-click to reset"
          onMouseDown={(e) => props.grip!(e)} onClick={(e) => e.stopPropagation()}
          onDblClick={(e) => { e.stopPropagation(); props.onReset?.(); }} />
      </Show>
      <span>{props.label}</span>
      <Show when={v().sortBy === props.k}>
        {v().sortDir === "asc" ? <ArrowUp size={12} /> : <ArrowDown size={12} />}
      </Show>
    </button>
  );
}

function GroupHeader(props: { tab: Tab; row: Extract<Row, { kind: "header" }>; top: number; sticky?: boolean }) {
  const toggle = () => {
    const s = new Set(props.tab.collapsed[0]());
    s.has(props.row.group.key) ? s.delete(props.row.group.key) : s.add(props.row.group.key);
    props.tab.collapsed[1](s);
  };
  const selectGroup = (e: MouseEvent) => {
    e.stopPropagation();
    props.tab.selection[1](props.row.group.items.map((x) => x.path));
  };
  return (
    <div class="group-head" classList={{ sticky: props.sticky }}
      style={props.sticky ? {} : { transform: `translateY(${props.top}px)`, height: `${HEADER_H}px` }}
      onMouseDown={(e) => e.stopPropagation()}
      onClick={toggle}
      title="Click to collapse · double-click to select the group"
      onDblClick={selectGroup}>
      {props.row.collapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
      <span class="group-label">{props.row.group.label}</span>
      <span class="group-count">{props.row.group.items.length}</span>
      <span class="group-line" />
    </div>
  );
}
