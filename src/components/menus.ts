import {
  AppWindow, ArrowDownUp, ClipboardPaste, Code2, Copy, CopyPlus, Eye, FileArchive, FilePlus, FolderInput, FolderOpen,
  FolderOutput, FolderPlus, Info, Layers, LayoutGrid, List, MousePointerClick, PackageOpen, Pencil, Redo2, RotateCw,
  Scissors, SquareTerminal, Star, Trash2, Undo2, ExternalLink, MoreHorizontal, Hash, PanelRight, BookmarkPlus,
  ArchiveRestore, FolderSearch, ShieldAlert,
} from "lucide-solid";
import { api } from "../lib/api";
import { kindOf } from "../lib/format";
import { basename, isRecycle, isWsl, parent } from "../lib/path";
import {
  favorites, isFavorite, recents, reload, resetView, saveViewForFolder, setUi,
  toggleFavorite, ui, updateView, type MenuItem, type Tab,
} from "../lib/state";
import {
  compress, copyPaths, copyToClipboard, deleteItems, duplicate, emptyBin, extractSmart, extractTo, newFile, newFolder,
  NEW_FILE_TYPES, openFile, openInVscode, openTerminal, paste, purgeFromBin, realPath, redo, redoLabel, restoreFromBin,
  showNativeMenu, startRename, transferTo, transferWithPicker, undo, undoLabel, visibleEntries, zipNameFor,
} from "../lib/ops";
import { adminWindow, elevateWindow, goTo, openEntry, openNewWindow, places, toast } from "../lib/state";
import type { Entry, GroupBy, ShellMenuItem, SortBy } from "../lib/types";

export const SORT_LABELS: Record<SortBy, string> = {
  name: "Name", modified: "Date modified", created: "Date created", size: "Size", type: "Type",
};
export const GROUP_LABELS: Record<GroupBy, string> = {
  none: "None", date: "Date", type: "Type", size: "Size", letter: "First letter",
};

const ARCHIVE = /\.(zip|7z|rar|tar|tgz|txz|tzst|tbz2?|gz|xz|zst|bz2|jar|apk|cbz|cbr|xpi|nupkg|vsix|whl|epub)$/i;
export const isArchive = (e: Pick<Entry, "isDir" | "name">) => !e.isDir && ARCHIVE.test(e.name);

/** Destinations for "Copy to ▸" / "Move to ▸": favorites, recents, picker. */
function destinations(op: "copy" | "move", paths: string[]): MenuItem[] {
  const seen = new Set<string>();
  const items: MenuItem[] = [];
  const add = (p: string, label: string, icon: any) => {
    const k = p.toLowerCase();
    if (seen.has(k) || paths.some((x) => x.toLowerCase() === k) || /\.(zip|7z|rar|tar|tgz|gz|xz|zst|bz2|jar|apk|cbz|cbr)(\\|$)/i.test(p)) return;
    seen.add(k);
    items.push({ label, icon, action: () => transferTo(op, paths, p) });
  };
  for (const f of favorites().filter((f) => f.isDir)) add(f.path, f.name, Star);
  for (const r of recents().slice(0, 5)) add(r.path, basename(r.path), FolderOpen);
  if (items.length) items.push({ separator: true });
  items.push({ label: "Choose folder…", icon: FolderOpen, action: () => transferWithPicker(op, paths) });
  return items;
}

function openWithItems(e: Entry): () => Promise<MenuItem[]> {
  return async () => {
    const path = await realPath(e);
    if (!path) return [];
    let apps = await api.listOpenWith(path).catch(() => []);
    if (!apps.length && /\.[^\\.]+$/.test(path)) {
      await new Promise((r) => setTimeout(r, 250));
      apps = await api.listOpenWith(path).catch(() => []);
    }
    const items: MenuItem[] = apps.map((a) => ({
      label: a.isDefault ? `${a.name} (default)` : a.name,
      iconUrl: a.icon, icon: AppWindow,
      action: () => api.openWith(path, a.id),
    }));
    if (items.length) items.push({ separator: true });
    items.push({ label: "Choose another app…", icon: MoreHorizontal, action: () => api.openWithDialog(path) });
    return items;
  };
}

export function itemMenu(t: Tab, e: Entry, targets: Entry[], x: number, y: number): MenuItem[] {
  if (isRecycle(t.p)) return binItemMenu(t, e, targets);
  const paths = targets.map((x) => x.path);
  const multi = targets.length > 1;
  const single = !multi;
  const archived = targets.some((x) => x.inArchive);
  const dirs = targets.filter((x) => x.isDir);
  const folderForShell = e.isDir ? e.path : parent(e.path)!;
  const items: MenuItem[] = [];

  // --- open ---
  items.push({ label: "Open", icon: e.isDir ? FolderOpen : ExternalLink, shortcut: "Enter", action: () => openEntry(t, e) });
  if (e.isDir) items.push({ label: "Open in new tab", icon: FolderOpen, shortcut: "Ctrl+Enter", action: () => dirs.forEach((d) => openEntry(t, d, true)) });
  if (e.isDir && single && !e.inArchive) items.push({ label: "Open in new window", icon: AppWindow, action: () => openNewWindow(e.path) });
  if (e.isDir && single && !e.inArchive && !adminWindow()) items.push({ label: "Open as administrator", icon: ShieldAlert, action: () => elevateWindow(e.path) });
  if (!e.isDir && single) {
    items.push({ label: "Open with", icon: AppWindow, loadSubmenu: openWithItems(e) });
    items.push({ label: "Preview", icon: Eye, shortcut: "Space", action: () => setUi("quickLook", true) });
  }
  if (isArchive(e) && single && !archived) {
    items.push({ label: "Browse archive", icon: FolderOpen, action: () => openEntry(t, { ...e, isDir: true }) });
  }
  items.push({ separator: true });

  // --- clipboard ---
  items.push(
    { label: "Cut", icon: Scissors, shortcut: "Ctrl+X", disabled: archived, action: () => copyToClipboard(paths, true) },
    { label: "Copy", icon: Copy, shortcut: "Ctrl+C", disabled: archived, action: () => copyToClipboard(paths, false) },
  );
  if (e.isDir && single && !archived) items.push({ label: "Paste into folder", icon: ClipboardPaste, shortcut: "Ctrl+V", action: () => paste(e.path) });
  items.push(
    { label: "Copy to", icon: FolderInput, submenu: destinations("copy", paths) },
    { label: "Move to", icon: FolderOutput, disabled: archived, submenu: destinations("move", paths) },
  );
  if (!archived) items.push({ label: "Duplicate", icon: CopyPlus, action: () => duplicate(paths) });
  items.push({ separator: true });

  // --- edit ---
  if (!archived) {
    items.push(
      { label: multi ? `Rename ${targets.length} items…` : "Rename", icon: Pencil, shortcut: "F2", action: () => startRename(t) },
      { label: "Delete", icon: Trash2, shortcut: "Del", action: () => deleteItems(paths, false) },
      { label: "Delete permanently", icon: Trash2, shortcut: "Shift+Del", danger: true, action: () => deleteItems(paths, true) },
      { separator: true },
    );
  }

  // --- archives ---
  if (isArchive(e) && single && !archived) {
    const stem = e.name.replace(/(\.tar)?\.[^.]+$/i, "");
    items.push(
      { label: "Extract here", icon: PackageOpen, action: () => extractSmart(e.path) },
      { label: `Extract to “${stem}\\”`, icon: PackageOpen, action: async () => {
        const dir = await api.createFolder(parent(e.path)!, stem).catch(() => null);
        if (dir) extractTo(e.path, dir);
      } },
      { label: "Extract to…", icon: PackageOpen, action: () => extractTo(e.path) },
    );
  }
  if (archived) items.push({ label: "Extract to…", icon: PackageOpen, action: () => extractTo(e.path, undefined, paths) });
  else items.push({ label: `Compress to “${zipNameFor(paths)}”`, icon: FileArchive, action: () => compress(paths) });
  items.push({ separator: true });

  // --- paths & places ---
  items.push(
    { label: multi ? "Copy as paths" : "Copy as path", icon: Hash, shortcut: "Ctrl+Shift+C", action: () => copyPaths(paths) },
    { label: multi ? "Copy names" : "Copy name", icon: Copy, action: () => navigator.clipboard.writeText(targets.map((x) => x.name).join("\r\n")) },
  );
  if (single && !archived) {
    items.push({ label: isFavorite(e.path) ? "Remove from favorites" : "Add to favorites", icon: Star, shortcut: "Ctrl+D", action: () => toggleFavorite(e.path, e.isDir) });
  }
  if (!archived) {
    items.push(
      { label: "Open in Terminal", icon: SquareTerminal, shortcut: "Ctrl+`", action: () => openTerminal(folderForShell) },
      { label: "Open in VS Code", icon: Code2, action: () => openInVscode(single ? e.path : folderForShell) },
      { label: "Show in Explorer", icon: ExternalLink, action: () => api.revealInExplorer(e.path) },
    );
  }
  items.push({ separator: true });
  items.push({ label: "Properties", icon: Info, shortcut: "Alt+Enter", disabled: archived, action: () => setUi("properties", paths) });
  if (!archived) items.push({ label: "Show more options", icon: MoreHorizontal, shortcut: "Shift+F10", action: () => showNativeMenu(t, paths, x, y) });
  return items;
}

export function viewSubmenus(t: Tab): MenuItem[] {
  const v = t.view[0]();
  return [
    { label: "View", icon: LayoutGrid, submenu: [
      { label: "List", icon: List, checked: v.mode === "list", shortcut: "Ctrl+1", action: () => updateView(t, { mode: "list" }) },
      { label: "Grid", icon: LayoutGrid, checked: v.mode === "grid", shortcut: "Ctrl+2", action: () => updateView(t, { mode: "grid" }) },
      { separator: true },
      ...[64, 96, 128, 192, 256].map((s) => ({
        label: s <= 64 ? "Small icons" : s <= 96 ? "Medium icons" : s <= 128 ? "Large icons" : s <= 192 ? "Extra large icons" : "Huge icons",
        checked: v.mode === "grid" && v.iconSize === s, action: () => updateView(t, { mode: "grid", iconSize: s }),
      })),
      { separator: true },
      { label: "Preview pane", icon: PanelRight, checked: ui.previewOpen, shortcut: "Alt+P", action: () => setUi("previewOpen", !ui.previewOpen) },
    ] },
    { label: "Sort by", icon: ArrowDownUp, submenu: [
      ...(Object.keys(SORT_LABELS) as SortBy[]).map((k) => ({ label: SORT_LABELS[k], checked: v.sortBy === k, action: () => updateView(t, { sortBy: k }) })),
      { separator: true },
      { label: "Ascending", checked: v.sortDir === "asc", action: () => updateView(t, { sortDir: "asc" }) },
      { label: "Descending", checked: v.sortDir === "desc", action: () => updateView(t, { sortDir: "desc" }) },
      { separator: true },
      { label: "Folders first", checked: v.foldersFirst, action: () => updateView(t, { foldersFirst: !v.foldersFirst }) },
    ] },
    { label: "Group by", icon: Layers, submenu: (Object.keys(GROUP_LABELS) as GroupBy[]).map((k) => ({
      label: GROUP_LABELS[k], checked: v.groupBy === k, action: () => updateView(t, { groupBy: k }),
    })) },
    { label: "Show hidden items", checked: v.showHidden, shortcut: "Ctrl+H", action: () => updateView(t, { showHidden: !v.showHidden }) },
  ];
}

export function backgroundMenu(t: Tab, x: number, y: number): MenuItem[] {
  if (isRecycle(t.p)) return binBackgroundMenu(t);
  const archived = /\.(zip|7z|rar|tar|gz|xz|zst|bz2|tgz|jar|apk|cbz|cbr)(\\|$)/i.test(t.p);
  const items: MenuItem[] = [
    ...viewSubmenus(t),
    { label: "Refresh", icon: RotateCw, shortcut: "F5", action: () => reload(t) },
    { separator: true },
    { label: "Paste", icon: ClipboardPaste, shortcut: "Ctrl+V", disabled: archived, action: () => paste(t.p) },
    { label: undoLabel() ? `Undo ${undoLabel()}` : "Undo", icon: Undo2, shortcut: "Ctrl+Z", disabled: !undoLabel(), action: () => undo() },
    { label: redoLabel() ? `Redo ${redoLabel()}` : "Redo", icon: Redo2, shortcut: "Ctrl+Y", disabled: !redoLabel(), action: () => redo() },
    { separator: true },
    { label: "New", icon: FilePlus, disabled: archived, submenu: [
      { label: "Folder", icon: FolderPlus, shortcut: "Ctrl+Shift+N", action: () => newFolder(t) },
      { separator: true },
      ...NEW_FILE_TYPES.map((f) => ({ label: f.label, icon: FilePlus, action: () => newFile(t, f.name, f.content) })),
    ] },
    { separator: true },
    { label: "Select all", icon: MousePointerClick, shortcut: "Ctrl+A", action: () => t.selection[1](visibleEntries(t).map((e) => e.path)) },
    { label: "Invert selection", action: () => {
      const sel = new Set(t.selection[0]().map((p) => p.toLowerCase()));
      t.selection[1](visibleEntries(t).filter((e) => !sel.has(e.path.toLowerCase())).map((e) => e.path));
    } },
    { separator: true },
  ];
  if (!archived) {
    items.push(
      { label: "Open in Terminal", icon: SquareTerminal, shortcut: "Ctrl+`", action: () => openTerminal(t.p) },
      { label: "Open in VS Code", icon: Code2, action: () => openInVscode(t.p) },
      { label: "Open in Explorer", icon: ExternalLink, action: () => api.openInExplorer(t.p) },
    );
    if (!adminWindow()) items.push({ label: "Run as administrator", icon: ShieldAlert, action: () => elevateWindow() });
  }
  items.push(
    { label: "Copy folder path", icon: Hash, action: () => copyPaths([t.p], false) },
    { label: isFavorite(t.p) ? "Remove folder from favorites" : "Add folder to favorites", icon: BookmarkPlus, shortcut: "Ctrl+D", action: () => toggleFavorite(t.p, true) },
    { label: "Remembered view", icon: LayoutGrid, submenu: [
      { label: "Save for this folder", action: () => saveViewForFolder(t) },
      { label: "Apply to this folder and all subfolders", action: () => saveViewForFolder(t, true) },
      { label: "Reset to default", disabled: t.source[0]() !== "folder", action: () => resetView(t) },
    ] },
    { separator: true },
    { label: "Properties", icon: Info, shortcut: "Alt+Enter", disabled: archived, action: () => setUi("properties", [t.p]) },
  );
  if (!archived) items.push({ label: "Show more options", icon: MoreHorizontal, shortcut: "Shift+F10", action: () => showNativeMenu(t, [], x, y) });
  return items;
}

/** Shared context menu for folders shown in the sidebar (favorites, places, drives, WSL). */
export function placeMenu(path: string, goTo: (p: string, newTab?: boolean) => void, x: number, y: number, extra: MenuItem[] = []): MenuItem[] {
  return [
    { label: "Open", icon: FolderOpen, action: () => goTo(path) },
    { label: "Open in new tab", icon: FolderOpen, action: () => goTo(path, true) },
    { separator: true },
    { label: "Paste into folder", icon: ClipboardPaste, action: () => paste(path) },
    { label: "Open in Terminal", icon: SquareTerminal, action: () => openTerminal(path) },
    { label: "Open in VS Code", icon: Code2, action: () => openInVscode(path) },
    { label: "Copy as path", icon: Hash, action: () => copyPaths([path]) },
    ...extra,
    { separator: true },
    { label: "Properties", icon: Info, action: () => api.showProperties([path]) },
    { label: "Show more options", icon: MoreHorizontal, action: () => api.showNativeMenu([path], x, y).catch(() => {}) },
  ];
}

// ---------- Recycle Bin ----------

function binItemMenu(t: Tab, e: Entry, targets: Entry[]): MenuItem[] {
  const paths = targets.map((x) => x.path);
  const originals = targets.map((x) => `${(x.origin ?? "").replace(/\\$/, "")}\\${x.name}`);
  const items: MenuItem[] = [
    { label: targets.length > 1 ? `Restore ${targets.length} items` : "Restore", icon: ArchiveRestore, action: () => restoreFromBin(paths) },
  ];
  if (!e.isDir && targets.length === 1) items.push({ label: "Preview", icon: Eye, shortcut: "Space", action: () => openEntry(t, e) });
  if (targets.length === 1 && e.origin) {
    items.push({ label: "Open original location", icon: FolderSearch, action: () => goTo(e.origin!) });
  }
  items.push(
    { separator: true },
    { label: "Delete permanently", icon: Trash2, shortcut: "Del", danger: true, action: () => purgeFromBin(paths) },
    { separator: true },
    { label: targets.length > 1 ? "Copy original paths" : "Copy original path", icon: Hash, action: () => copyPaths(originals) },
    { label: targets.length > 1 ? "Copy names" : "Copy name", icon: Copy, action: () => navigator.clipboard.writeText(targets.map((x) => x.name).join("\r\n")) },
  );
  return items;
}

function binBackgroundMenu(t: Tab): MenuItem[] {
  const all = () => visibleEntries(t).map((e) => e.path);
  const empty = !t.entries[0]().length;
  return [
    ...viewSubmenus(t),
    { label: "Refresh", icon: RotateCw, shortcut: "F5", action: () => reload(t) },
    { separator: true },
    { label: "Restore all items", icon: ArchiveRestore, disabled: empty, action: () => restoreFromBin(all()) },
    { label: "Empty Recycle Bin", icon: Trash2, danger: true, disabled: empty && !places().recycle.items, action: () => emptyBin() },
    { separator: true },
    { label: undoLabel() ? `Undo ${undoLabel()}` : "Undo", icon: Undo2, shortcut: "Ctrl+Z", disabled: !undoLabel(), action: () => undo() },
    { label: "Select all", icon: MousePointerClick, shortcut: "Ctrl+A", action: () => t.selection[1](all()) },
    { separator: true },
    { label: "Open in Explorer", icon: ExternalLink, action: () => api.openInExplorer(t.p) },
  ];
}

/** Sidebar menu for the Recycle Bin place. */
export function binPlaceMenu(onHide: () => void): MenuItem[] {
  return [
    { label: "Open", icon: FolderOpen, action: () => goTo("shell:RecycleBinFolder") },
    { label: "Open in new tab", icon: FolderOpen, action: () => goTo("shell:RecycleBinFolder", true) },
    { separator: true },
    { label: "Empty Recycle Bin", icon: Trash2, danger: true, disabled: !places().recycle.items, action: () => emptyBin() },
    { separator: true },
    { label: "Hide from sidebar", icon: Eye, action: onHide },
  ];
}

// ---------- entries other apps add (7-Zip, VS Code, Notepad, Zed…) ----------

function shellToMenu(t: Tab, token: number, it: ShellMenuItem): MenuItem {
  if (it.separator) return { separator: true };
  const kids = it.children.filter((c) => !c.separator || it.children.length > 1);
  return {
    label: it.label, iconUrl: it.icon, disabled: it.disabled,
    submenu: kids.length ? kids.map((c) => shellToMenu(t, token, c)) : undefined,
    action: kids.length ? undefined : () => api.shellMenuInvoke(token, it.id)
      .then((verb) => (verb === "rename" ? startRename(t) : setTimeout(() => reload(t), 500)))
      .catch((e) => toast(`${it.label}: ${e}`, "error")),
  };
}

/** Adds other apps' entries to the menu that was just opened, once Windows has listed them
 *  (above Properties). Does nothing if that menu has closed in the meantime. */
export async function addShellEntries(t: Tab, paths: string[], background: boolean) {
  const opened = ui.menu;
  const target = background ? [t.p] : paths;
  if (!opened || !target.length || isRecycle(t.p) || target.some((p) => isWsl(p) || /\.(zip|7z|rar|tar|gz|xz|zst|bz2|tgz|jar|apk|cbz|cbr)\\/i.test(p))) return;
  const res = await api.shellMenuItems(target, background).catch(() => null);
  if (!res?.items.length || ui.menu !== opened) return;
  const items = [...opened.items];
  const key = (label?: string) => (label ?? "").replace(/…$/, "").trim().toLowerCase();
  // Windows' "New ▸" adds its extra types (Office documents, shortcuts…) to Ferret's own New menu.
  const shellNew = res.items.find((it) => !it.separator && key(it.label) === "new" && it.children.length);
  const ownNew = items.findIndex((it) => key(it.label) === "new" && it.submenu);
  if (shellNew && ownNew >= 0) {
    const have = new Set(items[ownNew].submenu!.map((x) => key(x.label)));
    const add = shellNew.children.filter((c) => !c.separator && !have.has(key(c.label)) && !["text document", "folder"].includes(key(c.label)));
    if (add.length) items[ownNew] = { ...items[ownNew], submenu: [...items[ownNew].submenu!, { separator: true }, ...add.map((c) => shellToMenu(t, res.token, c))] };
  }
  // Skip what Ferret's menu already has (e.g. "Open in Terminal"), then tidy separators.
  const own = new Set(items.map((it) => key(it.label)).filter(Boolean));
  const kept = res.items.filter((it) => it !== shellNew && (it.separator || !own.has(key(it.label))));
  const extra = kept.map((it) => shellToMenu(t, res.token, it))
    .filter((it, i, all) => !it.separator || (i > 0 && i < all.length - 1 && !all[i - 1].separator));
  if (!extra.length) return setUi("menu", "items", items);
  const at = items.findIndex((it) => it.label === "Properties");
  const before = at > 0 && items[at - 1].separator ? at - 1 : Math.max(0, at);
  items.splice(at < 0 ? items.length : before, 0, { separator: true }, ...extra);
  setUi("menu", "items", items);
}

export const isPreviewable = (e: Entry) => kindOf(e) !== "folder";
export { openFile };
