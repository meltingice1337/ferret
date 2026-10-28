import { createEffect, For, on, onCleanup, onMount, Show } from "solid-js";
import ContextMenu from "./components/ContextMenu";
import { AskDialog, BatchRenameDialog, ConflictDialog, PropertiesDialog } from "./components/Dialogs";
import Panes from "./components/Panes";
import { DragGhost, JobsPanel } from "./components/Jobs";
import MediaEditor, { closeMediaEditor, escLeavesFullscreen } from "./components/MediaEditor";
import UpdateCard, { WhatsNewDialog } from "./components/UpdateCard";
import { checkForUpdate, setUpdater, startUpdater, updater } from "./lib/updater";
import { openKeyboardMenu } from "./components/menus";
import PreviewPane, { QuickLook } from "./components/PreviewPane";
import SettingsDialog from "./components/SettingsDialog";
import SearchPalette, { type PaletteCommand } from "./components/SearchPalette";
import Sidebar from "./components/Sidebar";
import StatusBar from "./components/StatusBar";
import TitleBar from "./components/TitleBar";
import Toolbar from "./components/Toolbar";
import { api } from "./lib/api";
import "./lib/drag";
import "./lib/autoscroll";
import {
  copyPaths, copyToClipboard, deleteItems, emptyBin, newFile, newFolder, openInVscode, openTerminal, paste, redo, redoLabel,
  startRename, syncClipboard, targetsOf, toOtherPane, undo, undoLabel,
} from "./lib/ops";
import { fileList, otherPane, stripTab, switchPane, toggleSplit, adminWindow, applyAppearance, elevateWindow, searchTitle, settings, isFavorite, navigate, openNewWindow, persistSettings, places, resetView, setSettings } from "./lib/state";
import { basename, isRecycle, isSearch, RECYCLE } from "./lib/path";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  activeTab, boot, closeTab, cycleTab, dismissToast, goBack, goForward, goUp, newTab,
  persistSession, previewSpace, ready, sidebarDocked, refresh, reopenClosedTab, setUi, stamp, toast, toggleFavorite, ui, updateView,
} from "./lib/state";
import type { GroupBy } from "./lib/types";
import { feedTypeAhead, keysHeld, queueKey, winWidth } from "./lib/state";

const GROUP_CYCLE: GroupBy[] = ["none", "date", "type", "size", "letter"];

/** Everything the command palette (Ctrl+Shift+P) can run. */
function paletteCommands(): PaletteCommand[] {
  const t = activeTab();
  const v = t.view[0]();
  const cmd = (label: string, run: () => void, keys?: string, hint?: string): PaletteCommand => ({ label, run, keys, hint });
  return [
    cmd("Search everywhere", () => setUi("palette", "files"), "Ctrl+K"),
    cmd("New tab", () => newTab(t.p), "Ctrl+T"),
    cmd("Close tab", () => closeTab(t.id), "Ctrl+W"),
    cmd(stripTab().split[0]() ? "Close split view" : "Split view (two folders side by side)", () => toggleSplit(), "Ctrl+\\"),
    ...(otherPane() ? [
      cmd("Switch pane", () => switchPane(), "Tab"),
      cmd("Copy to the other pane", () => toOtherPane("copy"), undefined, otherPane()!.p),
      cmd("Move to the other pane", () => toOtherPane("move"), undefined, otherPane()!.p),
    ] : []),
    cmd("Reopen closed tab", () => reopenClosedTab(), "Ctrl+Shift+T"),
    cmd("New folder", () => newFolder(t), "Ctrl+Shift+N"),
    cmd("New text document", () => newFile(t, "New Text Document.txt")),
    cmd("New Markdown file", () => newFile(t, "New Document.md")),
    cmd("Paste", () => paste(t.p), "Ctrl+V"),
    cmd("Undo", () => undo(), "Ctrl+Z", undoLabel() ?? undefined),
    cmd("Redo", () => redo(), "Ctrl+Y", redoLabel() ?? undefined),
    cmd("Refresh", () => refresh(t), "F5"),
    cmd("Go up", () => goUp(t), "Alt+↑"),
    cmd("Edit path", () => setUi("editingPath", true), "Ctrl+L"),
    cmd("Filter this folder", () => setUi("filterOpen", true), "Ctrl+F"),
    cmd(v.showHidden ? "Hide hidden items" : "Show hidden items", () => updateView(t, { showHidden: !v.showHidden }), "Ctrl+H"),
    cmd("View: list", () => updateView(t, { mode: "list" }), "Ctrl+1"),
    cmd("View: grid", () => updateView(t, { mode: "grid" }), "Ctrl+2"),
    cmd("Group by date", () => updateView(t, { groupBy: "date" })),
    cmd("Group by type", () => updateView(t, { groupBy: "type" })),
    cmd("Group by size", () => updateView(t, { groupBy: "size" })),
    cmd("No grouping", () => updateView(t, { groupBy: "none" })),
    cmd("Sort by name", () => updateView(t, { sortBy: "name", sortDir: "asc" })),
    cmd("Sort by date modified", () => updateView(t, { sortBy: "modified", sortDir: "desc" })),
    cmd("Sort by size", () => updateView(t, { sortBy: "size", sortDir: "desc" })),
    cmd("Reset view to default", () => resetView(t)),
    cmd("Toggle preview pane", () => { setUi("previewOpen", !ui.previewOpen); persistSession(); }, "Alt+P"),
    cmd("Toggle sidebar", () => toggleSidebar(), "Ctrl+B"),
    cmd("Open in Terminal", () => openTerminal(t.p), "Ctrl+`"),
    cmd("Open in VS Code", () => openInVscode(t.p)),
    cmd("New window", () => openNewWindow(), "Ctrl+N"),
    cmd("Check for updates", () => checkForUpdate(true)),
    ...(adminWindow() ? [] : [cmd("Run this window as administrator", () => elevateWindow(), undefined, "Same tabs, after a UAC prompt")]),
    cmd("Copy folder path", () => copyPaths([t.p], false)),
    cmd(isFavorite(t.p) ? "Remove folder from favorites" : "Add folder to favorites", () => toggleFavorite(t.p, true), "Ctrl+D"),
    ...places().known.map((k) => cmd(`Go to ${k.name}`, () => navigate(t, k.path), undefined, k.path)),
    ...places().drives.map((d) => cmd(`Go to ${d.label || "Local Disk"} (${d.path.replace(/\\$/, "")})`, () => navigate(t, d.path))),
    ...places().wsl.map((d) => cmd(`Go to ${d.name} (WSL)`, () => navigate(t, d.path))),
    cmd("Go to Recycle Bin", () => navigate(t, RECYCLE), undefined, places().recycle.items ? `${places().recycle.items} items` : "empty"),
    cmd("Empty Recycle Bin", () => emptyBin()),
    cmd("Theme: dark", () => { setSettings("theme", "dark"); persistSettings(); applyAppearance(); }),
    cmd("Theme: light", () => { setSettings("theme", "light"); persistSettings(); applyAppearance(); }),
    cmd("Settings", () => setUi("settingsOpen", true), "Ctrl+,"),
  ];
}

function toggleSidebar() {
  if (winWidth() < 860) return setUi("sidebarOverlay", !ui.sidebarOverlay);
  setUi("sidebarHidden", !ui.sidebarHidden);
  setUi("sidebarOverlay", false);
  persistSession();
}

function closeTopModal() {
  if (ui.dialog) { const d = ui.dialog; setUi("dialog", null); d.resolve(null); }
  else if (ui.conflict) { const c = ui.conflict; setUi("conflict", null); c.resolve(null); }
  else if (ui.batchRename) setUi("batchRename", null);
  else if (ui.media) closeMediaEditor();
  else if (ui.properties) setUi("properties", null);
  else if (updater.notesOpen) setUpdater("notesOpen", false);
  else if (ui.settingsOpen) setUi("settingsOpen", false);
}

const modalOpen = () => !!(ui.dialog || ui.conflict || ui.properties || ui.batchRename || ui.media || ui.settingsOpen || ui.quickLook || ui.palette || updater.notesOpen);

const keyboardMenu = () => openKeyboardMenu(activeTab());

/** F6 / Shift+F6, as in Explorer: the keyboard goes to the next area of the window (path bar,
 *  sidebar, files, preview), to the item that matters there, instead of Tab through each one. */
function cycleArea(dir: 1 | -1) {
  const q = (s: string) => document.querySelector<HTMLElement>(s);
  const areas = [
    { box: q(".toolbar"), target: () => q(".path-edit input") ?? q(".crumbs .crumb.last") },
    { box: q(".sidebar"), target: () => q(".sidebar .side-item.active") ?? q(".sidebar .side-item") },
    { box: q(".panes"), target: () => fileList() },
    { box: q(".preview"), target: () => q(".preview button, .preview [tabindex='0']") },
  ].filter((a) => a.box?.offsetParent && a.target());
  if (!areas.length) return;
  const here = areas.findIndex((a) => a.box!.contains(document.activeElement));
  // From anywhere else (the tab strip, nothing), F6 starts at the files.
  const from = here >= 0 ? here : areas.findIndex((a) => a.box === q(".panes")) - dir;
  areas[(from + dir + areas.length) % areas.length].target()?.focus({ preventScroll: true });
}

/** "Downloads - Ferret", "Local Disk (C:) - Ferret (Administrator)"… */
function windowTitle(path: string) {
  const drive = /^([a-z]):\\?$/i.exec(path);
  const folder = isRecycle(path) ? "Recycle Bin"
    : isSearch(path) ? searchTitle(path)
    : drive ? `${places().drives.find((d) => d.path[0].toUpperCase() === drive[1].toUpperCase())?.label || "Local Disk"} (${drive[1].toUpperCase()}:)`
    : basename(path) || path;
  const marks = [import.meta.env.DEV && "Dev", adminWindow() && "Administrator"].filter(Boolean);
  return `${folder} - Ferret${marks.length ? ` (${marks.join(", ")})` : ""}`;
}

/** How long after the page starts loading a start counts as slow, and shows the splash first
 *  (the same in public/splash.js). */
const SPLASH_AFTER_MS = 400;

/** The app is ready (or starting failed, so the error shows): the window, hidden until now,
 *  appears with the app drawn, so a normal start never shows the splash (index.html). A slow
 *  start shows the window with the splash (public/splash.js, or here), which fades into the
 *  app. In dev, `?splash` in the address keeps the splash on screen to look at. */
function showApp() {
  const el = document.getElementById("splash");
  const show = () => { stamp("shown"); api.showWindow().catch(() => {}); };
  if (!el || (import.meta.env.DEV && new URLSearchParams(location.search).has("splash"))) return show();
  if (!el.dataset.shown && performance.now() < SPLASH_AFTER_MS) {
    el.remove();
    // After a frame with the app in it, so the window doesn't first appear empty, and with
    // nothing still easing in (the file list's fade): Windows animates the window itself.
    let done = false;
    const once = () => { if (!done) { done = true; show(); } };
    requestAnimationFrame(() => {
      for (const a of document.getAnimations()) try { a.finish(); } catch { /* an endless one (a spinner) */ }
      requestAnimationFrame(once);
    });
    setTimeout(once, 100);
    return;
  }
  // Slow: the window may still hold the splash's last frame (the app's first frame takes a
  // moment longer when it's a big folder), so it appears with the splash and fades into the app.
  if (el.dataset.shown) stamp("shown");
  else { el.dataset.shown = "1"; show(); }
  if (document.documentElement.dataset.motion === "off" || matchMedia("(prefers-reduced-motion: reduce)").matches) return el.remove();
  el.classList.add("out");
  // Its own fade, not one of its children's animations ending.
  el.addEventListener("animationend", (e) => { if (e.target === el) el.remove(); });
  setTimeout(() => el.remove(), 800);
}

export default function App() {
  // Return keyboard focus to the file list whenever a dialog or menu closes.
  createEffect(on(() => modalOpen() || !!ui.menu, (open, wasOpen) => {
    if (wasOpen && !open) requestAnimationFrame(() => {
      const a = document.activeElement;
      if (!a || a === document.body) fileList()?.focus({ preventScroll: true });
    });
  }));

  // Settings → Animations (the CSS entrances follow it; lib/motion.ts reads the setting itself).
  // Until the settings are loaded, the splash keeps the one from the last run (public/splash.js).
  createEffect(() => { if (ready()) document.documentElement.dataset.motion = settings.animations === false ? "off" : "on"; });
  createEffect(() => { if (ready()) showApp(); });
  // For the next start's splash, which shows before the settings load (public/splash.js).
  createEffect(() => {
    if (!ready()) return;
    const look = { theme: settings.theme, accent: settings.accent, motion: settings.animations !== false };
    try { localStorage.setItem("ferret:appearance", JSON.stringify(look)); } catch { /* the splash keeps the defaults */ }
  });

  // Like Explorer, the window (taskbar, Alt+Tab) is named after the folder it shows.
  createEffect(() => {
    if (!ready()) return;
    const t = activeTab();
    if (t) getCurrentWindow().setTitle(windowTitle(t.p)).catch(() => {});
  });

  onMount(() => {
    boot().catch((e) => { showApp(); toast(`Startup failed: ${e}`, "error"); });
    startUpdater();

    const onKey = (e: KeyboardEvent) => {
      if (!ready() || ui.menu) return;
      const t = activeTab();
      const target = e.target as HTMLElement;
      const inField = !!target.closest("input, textarea, select, [contenteditable]");
      // Typing right after "New folder", before its rename box exists, is buffered.
      if (!inField && feedTypeAhead(e)) { e.preventDefault(); return; }
      // Focus fell back to <body> (e.g. after closing the path bar): hand list keys to the file list.
      if (target === document.body && !modalOpen() && !e.ctrlKey && !e.altKey &&
          e.key !== "/" && (e.key.startsWith("Arrow") || ["Home", "End", "PageUp", "PageDown", "Enter", " ", "Backspace"].includes(e.key) || e.key.length === 1)) {
        const files = fileList();
        if (files) {
          e.preventDefault();
          files.focus({ preventScroll: true });
          files.dispatchEvent(new KeyboardEvent("keydown", { key: e.key, code: e.code, shiftKey: e.shiftKey, bubbles: true, cancelable: true }));
          return;
        }
      }
      const ctrl = e.ctrlKey || e.metaKey;
      const k = e.key.toLowerCase();
      if (e.key === "Escape" && ui.sidebarOverlay && !modalOpen()) { e.preventDefault(); return setUi("sidebarOverlay", false); }
      if (modalOpen()) {
        // Escape closes the top-most dialog even when focus fell back to <body>.
        if (e.key === "Escape" && !ui.quickLook) { e.preventDefault(); if (!escLeavesFullscreen()) closeTopModal(); }
        return;
      }

      if (e.altKey && !ctrl) {
        if (e.key === "ArrowLeft") { e.preventDefault(); return goBack(t); }
        if (e.key === "ArrowRight") { e.preventDefault(); return goForward(t); }
        if (e.key === "ArrowUp") { e.preventDefault(); return goUp(t); }
        if (k === "p") {
          e.preventDefault();
          // Too narrow: say so, and leave the setting alone so the pane returns when there's room.
          if (!previewSpace()) return toast("The window is too narrow for the preview pane. Widen it or use Quick Look (Space)");
          setUi("previewOpen", !ui.previewOpen);
          return persistSession();
        }
        if (e.key === "Enter" && !inField) { e.preventDefault(); const tg = targetsOf(t); return setUi("properties", tg.length ? tg.map((x) => x.path) : [t.p]); }
      }

      if (e.key === "F6" && !ctrl && !e.altKey) { e.preventDefault(); return cycleArea(e.shiftKey ? -1 : 1); }

      // Split view: Tab gives the keyboard to the other pane.
      if (e.key === "Tab" && !ctrl && !e.altKey && !inField && otherPane()) { e.preventDefault(); return switchPane(); }
      // Tab past the last control (Shift+Tab before the first) comes round to the other end,
      // instead of leaving the page with nothing visibly focused.
      if (e.key === "Tab" && !ctrl && !e.altKey) {
        const all = [...document.querySelectorAll<HTMLElement>("button, [href], input, select, textarea, [tabindex]")]
          .filter((el) => el.tabIndex >= 0 && !(el as HTMLButtonElement).disabled && el.offsetParent);
        const i = all.indexOf(target);
        if (all.length && (target === document.body || (!e.shiftKey && i === all.length - 1) || (e.shiftKey && i === 0))) {
          e.preventDefault();
          return (e.shiftKey ? all[all.length - 1] : all[0]).focus();
        }
      }

      // ---- file actions (not while typing) ----
      if (!inField) {
        const paths = () => targetsOf(t).map((x) => x.path);
        // Destructive keys only act when the file list (or nothing in particular) has focus.
        const inFiles = !!target.closest(".fileview") || target === document.body;
        if (e.key === "Delete" && inFiles) { e.preventDefault(); return deleteItems(paths(), e.shiftKey); }
        if (e.key === "F2" && inFiles) { e.preventDefault(); return startRename(t); }
        if ((e.key === "F10" && e.shiftKey) || e.key === "ContextMenu") { e.preventDefault(); return keyboardMenu(); }
      }

      // "/" opens search when not typing somewhere else.
      if (e.key === "/" && !ctrl && !e.altKey && !inField) { e.preventDefault(); return setUi("palette", "files"); }
      if (!ctrl) return;
      if (k === "k") { e.preventDefault(); return setUi("palette", "files"); }
      if (k === "p" && e.shiftKey) { e.preventDefault(); return setUi("palette", "commands"); }
      if (k === "tab") { e.preventDefault(); return cycleTab(e.shiftKey ? -1 : 1); }
      if (k === "t" && e.shiftKey) { e.preventDefault(); return reopenClosedTab(); }
      if (k === "t") { e.preventDefault(); return newTab(t.p); }
      if (k === "n" && !e.shiftKey) { e.preventDefault(); return openNewWindow(); }
      if (k === "w") { e.preventDefault(); return closeTab(t.id); }
      if (e.code === "Backslash" || e.key === "\\") { e.preventDefault(); return toggleSplit(); }
      if (k === "l") {
        e.preventDefault();
        // Like a browser's address bar: the whole path is selected, even if the editor is already open.
        const open = document.querySelector<HTMLInputElement>(".path-edit input");
        if (open) { open.focus(); open.select(); return; }
        return setUi("editingPath", true);
      }
      if (k === "f") {
        e.preventDefault();
        // Like Ctrl+L: back to the filter box if it's already open, with its text selected.
        const open = document.querySelector<HTMLInputElement>(".filter input");
        if (open) { open.focus(); open.select(); return; }
        return setUi("filterOpen", true);
      }
      if (k === ",") { e.preventDefault(); return setUi("settingsOpen", true); }
      if (k === "b") { e.preventDefault(); return toggleSidebar(); }
      if (e.code === "Backquote") { e.preventDefault(); return openTerminal(t.p); }
      if (inField) return;

      const paths = targetsOf(t).map((x) => x.path);
      if (k === "c" && e.shiftKey) { e.preventDefault(); return copyPaths(paths.length ? paths : [t.p]); }
      if (k === "c") { e.preventDefault(); return copyToClipboard(paths, false); }
      if (k === "x") { e.preventDefault(); return copyToClipboard(paths, true); }
      if (k === "v") { e.preventDefault(); return paste(t.p); }
      if (k === "z" && !e.shiftKey) { e.preventDefault(); return undo(); }
      if (k === "y" || (k === "z" && e.shiftKey)) { e.preventDefault(); return redo(); }
      if (k === "n" && e.shiftKey) { e.preventDefault(); return newFolder(t); }
      if (k === "d") {
        e.preventDefault();
        const sel = targetsOf(t);
        return sel.length === 1 ? toggleFavorite(sel[0].path, sel[0].isDir) : toggleFavorite(t.p, true);
      }
      if (k === "h") { e.preventDefault(); return updateView(t, { showHidden: !t.view[0]().showHidden }); }
      if (k === "1") { e.preventDefault(); return updateView(t, { mode: "list" }); }
      if (k === "2") { e.preventDefault(); return updateView(t, { mode: "grid" }); }
      if (k === "g") {
        e.preventDefault();
        const next = GROUP_CYCLE[(GROUP_CYCLE.indexOf(t.view[0]().groupBy) + 1) % GROUP_CYCLE.length];
        updateView(t, { groupBy: next });
        return toast(next === "none" ? "Grouping off" : `Grouped by ${next === "letter" ? "first letter" : next}`);
      }
      if ((k === "=" || k === "+" || k === "-") && t.view[0]().mode === "grid") {
        e.preventDefault();
        const size = t.view[0]().iconSize + (k === "-" ? -16 : 16);
        return updateView(t, { iconSize: Math.max(48, Math.min(256, size)) });
      }
    };

    // Mouse back / forward buttons.
    const onMouse = (e: MouseEvent) => {
      if (!ready()) return;
      if (e.button === 3) { e.preventDefault(); goBack(activeTab()); }
      if (e.button === 4) { e.preventDefault(); goForward(activeTab()); }
    };
    const noMenu = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest("input, textarea")) e.preventDefault();
    };

    // Held keystrokes (see holdKeys) are queued before anything else sees them.
    const onHold = (e: KeyboardEvent) => {
      if (!keysHeld() || ["Shift", "Control", "Alt", "Meta"].includes(e.key)) return;
      if ((e.target as HTMLElement).closest(".modal, .palette, .menu")) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      queueKey(e);
    };
    // Tab and Shift+Tab cycle inside the top-most dialog instead of reaching the app behind it.
    const trapTab = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const dialogs = [...document.querySelectorAll<HTMLElement>(".modal, .ql")];
      const d = dialogs[dialogs.length - 1];
      if (!d) return;
      const stops = [...d.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
      )].filter((el) => el.offsetParent !== null);
      const here = document.activeElement as HTMLElement | null;
      if (!stops.length) { e.preventDefault(); d.focus(); return; }
      const first = stops[0], last = stops[stops.length - 1];
      if (!here || !d.contains(here)) { e.preventDefault(); (e.shiftKey ? last : first).focus(); }
      else if (e.shiftKey && (here === first || here === d)) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && here === last) { e.preventDefault(); first.focus(); }
    };
    // F5 / Ctrl+R (with Shift or Ctrl too) would reload the whole window and land it on the
    // home folder: they refresh the current folder instead, whatever has focus.
    const noReload = (e: KeyboardEvent) => {
      if (e.key !== "F5" && !((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "r")) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.repeat || !ready() || ui.menu || (modalOpen() && !ui.palette)) return;
      refresh(activeTab());
    };
    window.addEventListener("keydown", noReload, true);
    window.addEventListener("keydown", trapTab, true);
    window.addEventListener("keydown", onHold, true);
    window.addEventListener("keydown", onKey);
    window.addEventListener("mouseup", onMouse);
    window.addEventListener("contextmenu", noMenu);
    // Something copied in another app replaces a cut made here.
    window.addEventListener("focus", syncClipboard);
    onCleanup(() => {
      window.removeEventListener("keydown", noReload, true);
      window.removeEventListener("keydown", trapTab, true);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onHold, true);
      window.removeEventListener("mouseup", onMouse);
      window.removeEventListener("contextmenu", noMenu);
      window.removeEventListener("focus", syncClipboard);
    });
  });

  return (
    // The gaps around the panels move the window (double-click maximizes).
    <div class="app" data-tauri-drag-region style={{ "--sidebar-w": `${sidebarDocked() ? ui.sidebarWidth : 0}px` }}>
      <Show when={ready()}>
        {/* Narrow windows: the sidebar slides over a dimmed list (clicking it closes the sidebar). */}
        <Show when={!sidebarDocked() && ui.sidebarOverlay}><div class="scrim" /></Show>
        <Show when={sidebarDocked() || ui.sidebarOverlay}>
          <Sidebar overlay={!sidebarDocked()} />
        </Show>
        {/* The preview is a column inside the files panel, so that panel (and the window
            buttons in its corner) always reaches the right edge of the window. */}
        <main class="main panel" style={{ "--pv-w": `${ui.previewOpen ? previewSpace() : 0}px` }}>
          <TitleBar />
          <Toolbar />
          <div class="content">
            <Show when={stripTab()} keyed>
              {(t) => <Panes tab={t} />}
            </Show>
            <Show when={ui.previewOpen && previewSpace() > 0}><PreviewPane /></Show>
          </div>
          <StatusBar />
          <JobsPanel />
          <UpdateCard />
        </main>
        <Show when={ui.quickLook}><QuickLook /></Show>
      </Show>
      <Show when={ui.palette} keyed>
        {(mode) => <SearchPalette mode={mode} commands={paletteCommands()} />}
      </Show>
      <ContextMenu />
      <SettingsDialog />
      <WhatsNewDialog />
      <AskDialog />
      <ConflictDialog />
      <PropertiesDialog />
      <BatchRenameDialog />
      <MediaEditor />
      <DragGhost />
      <div class="toasts">
        <For each={ui.toasts}>
          {(t) => (
            <div class="toast" classList={{ error: t.kind === "error" }}>
              <span>{t.text}</span>
              <Show when={t.action}>
                <button class="toast-action" onClick={() => { t.action!.run(); dismissToast(t.id); }}>{t.action!.label}</button>
              </Show>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}
