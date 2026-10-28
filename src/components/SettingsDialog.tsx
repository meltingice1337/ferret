import { Trash2, X } from "../lib/icons";
import { createResource, createSignal, For, type JSX, Show } from "solid-js";
import { api } from "../lib/api";
import { formatDate } from "../lib/format";
import {
  applyAppearance, friendly, newTab, persistSettings, places, reresolveAll, setSettings, setUi, settings, toast, ui,
} from "../lib/state";
import { SECTIONS, setPlaceShown, setSectionShown } from "./Sidebar";
import type { GroupBy, ProfileName, Settings, SortBy, ViewSettings } from "../lib/types";
import { GROUP_LABELS, SORT_LABELS } from "./FileView";
import { checkForUpdate, showWhatsNew, updater } from "../lib/updater";

export const ACCENTS = [
  { name: "Rose", value: "#e2457f" },
  { name: "Magenta", value: "#c026d3" },
  { name: "Violet", value: "#7c5cff" },
  { name: "Blue", value: "#2f7dff" },
  { name: "Cyan", value: "#06b6d4" },
  { name: "Emerald", value: "#10b981" },
  { name: "Amber", value: "#f59e0b" },
  { name: "Orange", value: "#f97316" },
  { name: "Red", value: "#ef4444" },
  { name: "Slate", value: "#94a3b8" },
];

const PROFILES: { id: ProfileName; name: string; desc: string }[] = [
  { id: "media", name: "Media", desc: "Folders that are mostly images or videos → grid, grouped by date, newest first" },
  { id: "code", name: "Code", desc: "Folders with .git, package.json, Cargo.toml… → list by name, hidden files shown" },
  { id: "downloads", name: "Downloads", desc: "Your Downloads folder → grouped by date, newest first" },
];

export default function SettingsDialog() {
  return (
    <Show when={ui.settingsOpen}>
      <div class="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && setUi("settingsOpen", false)}>
        <div class="modal" role="dialog" aria-label="Settings" onKeyDown={(e) => { if (e.key === "Escape") setUi("settingsOpen", false); e.stopPropagation(); }} tabIndex={-1}
          ref={(el) => queueMicrotask(() => el.focus())}>
          <header class="modal-head">
            <h2>Settings</h2>
            <button class="icon-btn" onClick={() => setUi("settingsOpen", false)} title="Close (Esc)"><X size={16} /></button>
          </header>
          <div class="modal-body">
            <Appearance />
            <DefaultView />
            <Remembered />
            <Behavior />
            <SidebarCard />
            <Integration />
            <Shortcuts />
            <About />
          </div>
        </div>
      </div>
    </Show>
  );
}

function set<K extends keyof Settings>(key: K, value: Settings[K]) {
  setSettings(key, value as any);
  persistSettings();
}

function Card(props: { title: string; desc?: string; children: JSX.Element }) {
  return (
    <section class="card">
      <h3>{props.title}</h3>
      <Show when={props.desc}><p class="card-desc">{props.desc}</p></Show>
      {props.children}
    </section>
  );
}

function Field(props: { label: string; hint?: string; children: JSX.Element }) {
  return (
    <div class="field">
      <div class="field-label"><span>{props.label}</span><Show when={props.hint}><small>{props.hint}</small></Show></div>
      <div class="field-control">{props.children}</div>
    </div>
  );
}

function Seg<T extends string>(props: { value: T; options: { value: T; label: string }[]; onChange: (v: T) => void }) {
  return (
    <div class="seg wide">
      <For each={props.options}>
        {(o) => <button classList={{ on: props.value === o.value }} onClick={() => props.onChange(o.value)}>{o.label}</button>}
      </For>
    </div>
  );
}

function Toggle(props: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <button class="toggle" classList={{ on: props.checked }} role="switch" aria-checked={props.checked}
      onClick={() => props.onChange(!props.checked)}><span /></button>
  );
}

function Appearance() {
  const appearance = <K extends keyof Settings>(k: K, v: Settings[K]) => { set(k, v); applyAppearance(); };
  return (
    <Card title="Appearance">
      <Field label="Accent color" hint="Used for selection, highlights and the background tint">
        <div class="swatches">
          <For each={ACCENTS}>
            {(a) => (
              <button class="swatch" classList={{ on: settings.accent.toLowerCase() === a.value }} title={a.name}
                style={{ background: a.value }} onClick={() => appearance("accent", a.value)} />
            )}
          </For>
          <label class="swatch custom" title="Custom color" classList={{ on: !ACCENTS.some((a) => a.value === settings.accent.toLowerCase()) }}
            style={{ background: ACCENTS.some((a) => a.value === settings.accent.toLowerCase()) ? undefined : settings.accent }}>
            <input type="color" value={settings.accent} onInput={(e) => appearance("accent", e.currentTarget.value)} />
            <span>+</span>
          </label>
        </div>
      </Field>
      <Field label="Theme">
        <Seg value={settings.theme} onChange={(v) => appearance("theme", v)}
          options={[{ value: "dark", label: "Dark" }, { value: "light", label: "Light" }, { value: "auto", label: "System" }]} />
      </Field>
      <Field label="Window background" hint="Mica / Acrylic need Windows 11">
        <Seg value={settings.backdrop} onChange={(v) => appearance("backdrop", v)}
          options={[{ value: "mica", label: "Mica" }, { value: "acrylic", label: "Acrylic" }, { value: "solid", label: "Solid" }]} />
      </Field>
      <Field label="Density">
        <Seg value={settings.density} onChange={(v) => appearance("density", v)}
          options={[{ value: "comfortable", label: "Comfortable" }, { value: "compact", label: "Compact" }]} />
      </Field>
      <Field label="Animations" hint="Short transitions between previews, folders and tabs. Also off when Windows' animation effects are off">
        <Toggle checked={settings.animations} onChange={(v) => { setSettings("animations", v); persistSettings(); }} />
      </Field>
    </Card>
  );
}

function DefaultView() {
  const gv = () => settings.globalView;
  const setGv = (patch: Partial<ViewSettings>) => {
    setSettings("globalView", patch);
    persistSettings();
    reresolveAll();
  };
  return (
    <Card title="Default view" desc="Used for every folder that has no remembered view or matching profile.">
      <Field label="Layout">
        <Seg value={gv().mode} onChange={(v) => setGv({ mode: v })} options={[{ value: "list", label: "List" }, { value: "grid", label: "Grid" }]} />
      </Field>
      <Field label="Sort by">
        <select value={gv().sortBy} onChange={(e) => setGv({ sortBy: e.currentTarget.value as SortBy })}>
          <For each={Object.entries(SORT_LABELS)}>{([k, l]) => <option value={k}>{l}</option>}</For>
        </select>
        <Seg value={gv().sortDir} onChange={(v) => setGv({ sortDir: v })} options={[{ value: "asc", label: "Asc" }, { value: "desc", label: "Desc" }]} />
      </Field>
      <Field label="Group by">
        <select value={gv().groupBy} onChange={(e) => setGv({ groupBy: e.currentTarget.value as GroupBy })}>
          <For each={Object.entries(GROUP_LABELS)}>{([k, l]) => <option value={k}>{l}</option>}</For>
        </select>
      </Field>
      <Field label="Folders first"><Toggle checked={gv().foldersFirst} onChange={(v) => setGv({ foldersFirst: v })} /></Field>
      <Field label="Show hidden items"><Toggle checked={gv().showHidden} onChange={(v) => setGv({ showHidden: v })} /></Field>
    </Card>
  );
}

function Remembered() {
  const [overrides, { refetch }] = createResource(() => ui.settingsOpen, () => api.listFolderViews());
  const toggleProfile = (id: ProfileName, v: boolean) => {
    setSettings("profiles", id, "enabled", v);
    persistSettings();
    reresolveAll();
  };
  const describe = (v: Partial<ViewSettings>) =>
    [v.mode, v.sortBy && `${SORT_LABELS[v.sortBy]} ${v.sortDir === "desc" ? "↓" : "↑"}`, v.groupBy && v.groupBy !== "none" && `grouped by ${GROUP_LABELS[v.groupBy].toLowerCase()}`]
      .filter(Boolean).join(" · ");

  return (
    <Card title="Remembered views" desc="Order of priority: this folder's own view → a parent applied to subfolders → folder-type profile → default view.">
      <Field label="Remember view per folder" hint="Changing sort, grouping or layout in a folder saves it for that folder">
        <Toggle checked={settings.rememberPerFolder} onChange={(v) => set("rememberPerFolder", v)} />
      </Field>
      <Field label="Subfolders inherit" hint="Folders without their own view use their nearest parent's">
        <Toggle checked={settings.inheritToSubfolders} onChange={(v) => { set("inheritToSubfolders", v); reresolveAll(); }} />
      </Field>
      <div class="profiles">
        <For each={PROFILES}>
          {(p) => (
            <div class="profile">
              <div><strong>{p.name} profile</strong><small>{p.desc}</small></div>
              <Toggle checked={settings.profiles[p.id].enabled} onChange={(v) => toggleProfile(p.id, v)} />
            </div>
          )}
        </For>
      </div>
      <div class="overrides">
        <div class="overrides-head">
          <span>{overrides()?.length ?? 0} folders with a remembered view</span>
          <Show when={overrides()?.length}>
            <button class="btn danger" onClick={async () => { await api.clearAllFolderViews(); refetch(); reresolveAll(); toast("Cleared all remembered views"); }}>Clear all</button>
          </Show>
        </div>
        <div class="overrides-list">
          <For each={overrides()}>
            {(o) => (
              <div class="override">
                <div class="override-main">
                  <span class="override-path" title={o.path}>{o.path}</span>
                  <small>{describe(o.view)}{o.recursive ? " · applies to subfolders" : ""} · {formatDate(o.updatedAt)}</small>
                </div>
                <button class="icon-btn tiny" title="Forget"
                  onClick={async () => { await api.clearFolderView(o.path); refetch(); reresolveAll(); }}><Trash2 size={13} /></button>
              </div>
            )}
          </For>
        </div>
      </div>
    </Card>
  );
}

function Behavior() {
  return (
    <Card title="Behavior">
      <Field label="Open items with" >
        <Seg value={settings.doubleClickOpen ? "double" : "single"} onChange={(v) => set("doubleClickOpen", v === "double")}
          options={[{ value: "double", label: "Double-click" }, { value: "single", label: "Single click" }]} />
      </Field>
      <Field label="Restore tabs on launch"><Toggle checked={settings.restoreSession} onChange={(v) => set("restoreSession", v)} /></Field>
      <Field label="Folder sizes" hint="Counted in the background at low priority and shown in the Size column (not on network shares)">
        <Toggle checked={settings.folderSizes} onChange={(v) => set("folderSizes", v)} />
      </Field>
    </Card>
  );
}

function SidebarCard() {
  const placeList = () => [...places().known.map((k) => ({ id: k.id, name: k.name })), { id: "recycle", name: "Recycle Bin" }];
  return (
    <Card title="Sidebar" desc="Choose what the sidebar shows. You can also right-click a place or a section title to hide it.">
      <div class="toggle-grid">
        <For each={placeList()}>
          {(p) => (
            <label class="toggle-row">
              <span>{p.name}</span>
              <Toggle checked={!settings.hiddenPlaces.includes(p.id)} onChange={(v) => setPlaceShown(p.id, v)} />
            </label>
          )}
        </For>
      </div>
      <h4 class="card-sub">Sections</h4>
      <div class="toggle-grid">
        <For each={SECTIONS}>
          {(sec) => (
            <label class="toggle-row">
              <span>{sec.name}</span>
              <Toggle checked={!settings.hiddenSections.includes(sec.id)} onChange={(v) => setSectionShown(sec.id, v)} />
            </label>
          )}
        </For>
      </div>
    </Card>
  );
}

function Integration() {
  const [status, { mutate }] = createResource(() => ui.settingsOpen, () => api.defaultManagerStatus());
  const [busy, setBusy] = createSignal(false);
  const change = async (enable: boolean) => {
    setBusy(true);
    try {
      mutate(await api.setDefaultManager(enable));
      toast(enable ? "Ferret now opens folders and Win+E" : "Explorer is the default again");
    } catch (e) {
      toast(friendly(String(e)), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card title="Windows integration">
      <Field label="Use Ferret instead of File Explorer"
        hint="Win+E, folders opened from the desktop and other apps open in Ferret (in a new tab when it's already running). Turning it off puts Explorer back exactly as it was.">
        <Toggle checked={!!status()?.enabled} onChange={(v) => !busy() && change(v)} />
      </Field>
      <Show when={status()?.devBuild && !status()?.enabled}>
        <p class="card-note">This copy is a development or test build, run from the project folder. Turn this on from the installed Ferret.</p>
      </Show>
      <Show when={status()?.enabled && status()?.target}>
        <p class="card-note">Opens with {status()!.target}</p>
      </Show>
      <p class="card-note">Windows still uses Explorer for the taskbar, the desktop, file Open/Save dialogs and some apps' “Show in folder”.</p>
    </Card>
  );
}

const KEYS: [string, string][] = [
  // Finding and moving around
  ["Ctrl+K or /", "Search everywhere"], ["Ctrl+Shift+P", "Command palette"],
  ["Ctrl+L", "Edit path"], ["Ctrl+F", "Filter folder"], ["Type letters", "Jump to file"],
  ["Alt+← → ↑", "Back / forward / up"], ["Backspace", "Up"], ["F5", "Refresh"],
  // Tabs and windows
  ["Ctrl+T / Ctrl+W", "New / close tab"], ["Ctrl+Shift+T", "Reopen closed tab"], ["Ctrl+Tab", "Next tab"],
  ["Ctrl+\\", "Split view (two folders side by side)"], ["Tab", "Other pane (in split view)"],
  ["Ctrl+N", "New window"],
  // Files
  ["Space", "Quick Look"], ["Alt+P", "Preview pane"], ["F2", "Rename"], ["Ctrl+Shift+N", "New folder"],
  ["Del / Shift+Del", "Recycle / delete permanently"], ["Ctrl+Z / Ctrl+Y", "Undo / redo"],
  ["Alt+Enter", "Properties"], ["Ctrl+Shift+C", "Copy path"], ["Ctrl+D", "Pin the selected item (or this folder) to favorites"],
  // View
  ["Ctrl+1 / Ctrl+2", "List / grid"], ["Ctrl+G", "Cycle grouping"], ["Ctrl+H", "Show hidden"],
  ["Ctrl+wheel", "Grid icon size"], ["Ctrl+,", "Settings"],
];

function Shortcuts() {
  return (
    <Card title="Keyboard shortcuts">
      <div class="keys">
        <For each={KEYS}>{([k, d]) => <div class="key-row"><kbd>{k}</kbd><span>{d}</span></div>}</For>
      </div>
    </Card>
  );
}

function About() {
  const status = () => {
    if (updater.phase === "checking") return "Checking…";
    if (updater.phase === "available") return `Version ${updater.version} is available`;
    if (updater.phase === "downloading" || updater.phase === "installing") return `Updating to ${updater.version}…`;
    if (updater.lastChecked) return `Up to date · checked ${formatDate(updater.lastChecked)}`;
    return "";
  };
  return (
    <section class="card about">
      <img src="/ferret-256.png" alt="" width={56} height={56} />
      <div class="about-text">
        <h3>Ferret <span class="muted">{updater.current}</span><Show when={import.meta.env.DEV}><span class="dev-pill">DEV</span></Show></h3>
        <p class="card-desc">A fast, keyboard-first file manager for Windows. Search everything with Ctrl+K.</p>
        <Show when={status()}><p class="card-desc">{status()}</p></Show>
      </div>
      <div class="about-actions">
        <button class="btn" title="Ferret's log files, for tracking down problems"
          onClick={() => api.logFolder().then((dir) => { setUi("settingsOpen", false); newTab(dir); }).catch((e) => toast(String(e), "error"))}>Open logs</button>
        <Show when={updater.phase === "available"} fallback={
          <button class="btn" disabled={updater.phase !== "idle" && updater.phase !== "error"} onClick={() => checkForUpdate(true)}>Check for updates</button>
        }>
          <button class="btn accent" onClick={showWhatsNew}>What's new</button>
        </Show>
      </div>
    </section>
  );
}
