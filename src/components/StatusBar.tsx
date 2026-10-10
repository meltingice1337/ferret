import { ArrowDownUp, Eye, EyeOff, Layers, LayoutGrid, List, LoaderCircle, PanelRight } from "lucide-solid";
import { createMemo, Show } from "solid-js";
import { formatSize, plural } from "../lib/format";
import { basename, isRecycle, pathKey } from "../lib/path";
import {
  activeTab, openMenu, persistSession, previewSpace, resetView, saveViewForFolder, setUi, settings, ui, updateView, type MenuItem,
} from "../lib/state";
import type { GroupBy, SortBy } from "../lib/types";
import { GROUP_LABELS, SORT_LABELS } from "./FileView";

const PROFILE_LABEL = { media: "Media", code: "Code", downloads: "Downloads" } as const;

export default function StatusBar() {
  const t = () => activeTab();
  const v = () => t().view[0]();

  const stats = createMemo(() => {
    const entries = t().entries[0]();
    const shown = v().showHidden ? entries : entries.filter((e) => !e.hidden);
    const sel = new Set(t().selection[0]().map(pathKey));
    const selected = entries.filter((e) => sel.has(pathKey(e.path)));
    const bytes = selected.reduce((a, e) => a + (e.isDir ? 0 : e.size), 0);
    return { total: shown.length, hidden: entries.length - shown.length, selected: selected.length, bytes, anyDir: selected.some((e) => e.isDir), anyFile: selected.some((e) => !e.isDir) };
  });

  const source = () => {
    if (t().dirty[0]()) return { text: "Unsaved view", cls: "dirty" };
    switch (t().source[0]()) {
      case "folder": return { text: "Remembered for this folder", cls: "folder" };
      case "inherited": return { text: `Inherited from ${basename(t().sourcePath[0]() ?? "")}`, cls: "inherited" };
      case "profile": return { text: `${PROFILE_LABEL[t().profile[0]()!] ?? "Folder"} profile`, cls: "profile" };
      default: return { text: "Default view", cls: "global" };
    }
  };

  const sourceMenu = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuItem[] = [
      { heading: true, label: "Remembered view" },
      { label: "Save for this folder", disabled: t().source[0]() === "folder" && !t().dirty[0](), action: () => saveViewForFolder(t()) },
      { label: "Apply to this folder and all subfolders", action: () => saveViewForFolder(t(), true) },
      { label: "Reset to default", disabled: t().source[0]() !== "folder", action: () => resetView(t()) },
      { separator: true },
      { label: "View settings…", action: () => setUi("settingsOpen", true) },
    ];
    openMenu(r.left, r.top - 8, items);
  };

  const sortMenu = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openMenu(r.left, r.top - 8, [
      { heading: true, label: "Sort by" },
      ...(Object.keys(SORT_LABELS) as SortBy[]).map((k) => ({ label: SORT_LABELS[k], checked: v().sortBy === k, action: () => updateView(t(), { sortBy: k }) })),
      { separator: true },
      { label: "Ascending", checked: v().sortDir === "asc", action: () => updateView(t(), { sortDir: "asc" }) },
      { label: "Descending", checked: v().sortDir === "desc", action: () => updateView(t(), { sortDir: "desc" }) },
      { separator: true },
      { label: "Folders first", checked: v().foldersFirst, action: () => updateView(t(), { foldersFirst: !v().foldersFirst }) },
    ]);
  };

  const groupMenu = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openMenu(r.left, r.top - 8, [
      { heading: true, label: "Group by" },
      ...(Object.keys(GROUP_LABELS) as GroupBy[]).map((k) => ({ label: GROUP_LABELS[k], checked: v().groupBy === k, shortcut: k === "none" ? "Ctrl+G cycles" : undefined, action: () => updateView(t(), { groupBy: k }) })),
    ]);
  };

  return (
    <div class="statusbar">
      <div class="status-left">
        <Show when={t().loading[0]()}><LoaderCircle size={13} class="spin" /></Show>
        <span>{plural(stats().total, "item")}</span>
        <Show when={stats().hidden && !v().showHidden}><span class="muted">· {stats().hidden} hidden</span></Show>
        <Show when={stats().selected}>
          <span class="sel-count">· {stats().selected} selected{stats().anyFile ? ` (${formatSize(stats().bytes)}${stats().anyDir ? "+" : ""})` : ""}</span>
        </Show>
        <Show when={t().filter[0]()}><span class="muted">· filtered</span></Show>
      </div>
      <div class="status-right">
        <button class={`view-source ${source().cls}`} onClick={sourceMenu} title="Where this folder's view settings come from">
          <span class="dot" />{source().text}
        </button>
        <span class="sb-sep" />
        <button class="sb-btn" onClick={sortMenu} title="Sort">
          <ArrowDownUp size={13} /><span>{isRecycle(t().p) && v().sortBy === "modified" ? "Date deleted" : SORT_LABELS[v().sortBy]}</span>
        </button>
        <button class="sb-btn" classList={{ on: v().groupBy !== "none" }} onClick={groupMenu} title="Group (Ctrl+G)">
          <Layers size={13} /><span>{v().groupBy === "none" ? "No grouping" : GROUP_LABELS[v().groupBy]}</span>
        </button>
        <button class="sb-btn icon" classList={{ on: v().showHidden }} title="Show hidden items (Ctrl+H)"
          onClick={() => updateView(t(), { showHidden: !v().showHidden })}>
          {v().showHidden ? <Eye size={14} /> : <EyeOff size={14} />}
        </button>
        <Show when={v().mode === "grid"}>
          <input class="size-slider" type="range" min="48" max="256" step="8" value={v().iconSize}
            title="Icon size (Ctrl+wheel)"
            onInput={(e) => updateView(t(), { iconSize: +e.currentTarget.value })} />
        </Show>
        <button class="sb-btn icon" classList={{ on: ui.previewOpen && previewSpace() > 0 }} disabled={!previewSpace()}
          title={previewSpace() ? "Preview pane (Alt+P)" : "Window too narrow for the preview pane: use Quick Look (Space)"}
          onClick={() => { setUi("previewOpen", !ui.previewOpen); persistSession(); }}>
          <PanelRight size={14} />
        </button>
        <div class="seg">
          <button classList={{ on: v().mode === "list" }} title="List (Ctrl+1)" onClick={() => updateView(t(), { mode: "list" })}><List size={14} /></button>
          <button classList={{ on: v().mode === "grid" }} title="Grid (Ctrl+2)" onClick={() => updateView(t(), { mode: "grid" })}><LayoutGrid size={14} /></button>
        </div>
      </div>
      <Show when={!settings.rememberPerFolder && t().dirty[0]()}>
        <button class="save-view" onClick={() => saveViewForFolder(t())}>Save view for this folder</button>
      </Show>
    </div>
  );
}
