import { createEffect, For, on, onCleanup, onMount, Show } from "solid-js";
import ContextMenu from "./components/ContextMenu";
import { AskDialog, BatchRenameDialog, ConflictDialog, PropertiesDialog } from "./components/Dialogs";
import FileView from "./components/FileView";
import { DragGhost, JobsPanel } from "./components/Jobs";
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
import "./lib/drag";
import {
  copyPaths, copyToClipboard, deleteItems, emptyBin, newFile, newFolder, openInVscode, openTerminal, paste, redo, redoLabel,
  startRename, targetsOf, undo, undoLabel,
} from "./lib/ops";
import { adminWindow, applyAppearance, elevateWindow, isFavorite, navigate, openNewWindow, persistSettings, places, resetView, setSettings } from "./lib/state";
import { RECYCLE } from "./lib/path";
import {
  activeTab, boot, closeTab, cycleTab, dismissToast, goBack, goForward, goUp, newTab,
  persistSession, previewSpace, ready, sidebarDocked, reload, reopenClosedTab, setUi, toast, toggleFavorite, ui, updateView,
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
    cmd("Reopen closed tab", () => reopenClosedTab(), "Ctrl+Shift+T"),
    cmd("New folder", () => newFolder(t), "Ctrl+Shift+N"),
    cmd("New text document", () => newFile(t, "New Text Document.txt")),
    cmd("New Markdown file", () => newFile(t, "New Document.md")),
    cmd("Paste", () => paste(t.p), "Ctrl+V"),
    cmd("Undo", () => undo(), "Ctrl+Z", undoLabel() ?? undefined),
    cmd("Redo", () => redo(), "Ctrl+Y", redoLabel() ?? undefined),
    cmd("Refresh", () => reload(t), "F5"),
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
  else if (ui.properties) setUi("properties", null);
  else if (updater.notesOpen) setUpdater("notesOpen", false);
  else if (ui.settingsOpen) setUi("settingsOpen", false);
}

const modalOpen = () => !!(ui.dialog || ui.conflict || ui.properties || ui.batchRename || ui.settingsOpen || ui.quickLook || ui.palette || updater.notesOpen);

const keyboardMenu = () => openKeyboardMenu(activeTab());

export default function App() {
  // Return keyboard focus to the file list whenever a dialog or menu closes.
  createEffect(on(() => modalOpen() || !!ui.menu, (open, wasOpen) => {
    if (wasOpen && !open) requestAnimationFrame(() => {
      const a = document.activeElement;
      if (!a || a === document.body) document.querySelector<HTMLElement>(".files")?.focus({ preventScroll: true });
    });
  }));

  onMount(() => {
    boot().catch((e) => toast(`Startup failed: ${e}`, "error"));
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
        const files = document.querySelector<HTMLElement>(".files");
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
        if (e.key === "Escape" && !ui.quickLook) { e.preventDefault(); closeTopModal(); }
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
      if (e.key === "F5") { e.preventDefault(); return reload(t); }

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
      if (k === "l") {
        e.preventDefault();
        // Like a browser's address bar: the whole path is selected, even if the editor is already open.
        const open = document.querySelector<HTMLInputElement>(".path-edit input");
        if (open) { open.focus(); open.select(); return; }
        return setUi("editingPath", true);
      }
      if (k === "f") { e.preventDefault(); return setUi("filterOpen", true); }
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
    window.addEventListener("keydown", trapTab, true);
    window.addEventListener("keydown", onHold, true);
    window.addEventListener("keydown", onKey);
    window.addEventListener("mouseup", onMouse);
    window.addEventListener("contextmenu", noMenu);
    onCleanup(() => {
      window.removeEventListener("keydown", trapTab, true);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onHold, true);
      window.removeEventListener("mouseup", onMouse);
      window.removeEventListener("contextmenu", noMenu);
    });
  });

  return (
    // The gaps around the panels move the window (double-click maximizes).
    <div class="app" data-tauri-drag-region style={{ "--sidebar-w": `${sidebarDocked() ? ui.sidebarWidth : 0}px` }}>
      <Show when={ready()} fallback={<div class="splash"><img class="splash-logo" src="/ferret-256.png" alt="" /><div class="splash-mark">ferret</div></div>}>
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
            <Show when={activeTab()} keyed>
              {(t) => <FileView tab={t} />}
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
