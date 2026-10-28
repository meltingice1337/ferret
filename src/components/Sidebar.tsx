import {
  ChevronDown, Clock, Download, EyeOff, FileText, FolderOpen, HardDrive, House, Image, Monitor, Music, Pencil, Plug, Plus, Server,
  Inbox, Search, Settings as SettingsIcon, SlidersHorizontal, Star, Terminal, Trash2, Unplug, Usb, Video, X,
} from "../lib/icons";
import { disconnect, editConnection, openConnection, removeConnection } from "../lib/remote";
import type { Connection } from "../lib/types";
import { openSearchPalette } from "./SearchPalette";
import { createEffect, createSignal, For, type JSX, onCleanup, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import { api } from "../lib/api";
import { formatSize } from "../lib/format";
import { basename, isRecycle, isSearch, isWithin, parent, RECYCLE, remoteParts, samePath, searchPath, searchQuery } from "../lib/path";

/** Folder names that say little on their own (build output, sources, assets…). */
const GENERIC_NAMES = new Set([
  "src", "lib", "bin", "obj", "build", "dist", "out", "target", "debug", "release", "deps", "docs", "doc", "test", "tests",
  "assets", "public", "static", "data", "temp", "tmp", "config", "scripts", "images", "img", "node_modules", "app", "packages",
]);
import {
  connections, remoteState,
  activeTab, favorites, goTo, navigate, newTab, openMenu, persistSession, persistSettings, places, promptSaveSearch, recents,
  removeFromShelf, removeSavedSearch, setFavorites, setRecents, setSettings, settings, setUi, SHELF, ui, type MenuItem,
} from "../lib/state";
import FileIcon from "./FileIcon";
import { binPlaceMenu, placeMenu } from "./menus";
import { beginDrag, beginReorder } from "../lib/drag";
import { pathKey } from "../lib/path";
import { showWhatsNew, updater } from "../lib/updater";

const KNOWN_ICONS: Record<string, any> = {
  home: House, desktop: Monitor, documents: FileText, downloads: Download, pictures: Image, music: Music, videos: Video,
};

/** Sidebar sections that can be hidden (Settings → Sidebar). */
export const SECTIONS = [
  { id: "favorites", name: "Favorites" }, { id: "searches", name: "Saved searches" }, { id: "places", name: "Places" }, { id: "linux", name: "Linux (WSL)" },
  { id: "servers", name: "Servers" }, { id: "drives", name: "Drives" }, { id: "recents", name: "Recent" }, { id: "dropzone", name: "Dropzone" },
];

const placeShown = (id: string) => !settings.hiddenPlaces.includes(id);
const sectionShown = (id: string) => !settings.hiddenSections.includes(id);

export function setPlaceShown(id: string, show: boolean) {
  setSettings("hiddenPlaces", (h) => (show ? h.filter((x) => x !== id) : [...new Set([...h, id])]));
  persistSettings();
}

export function setSectionShown(id: string, show: boolean) {
  setSettings("hiddenSections", (h) => (show ? h.filter((x) => x !== id) : [...new Set([...h, id])]));
  persistSettings();
}

const customize: MenuItem = { label: "Customize sidebar…", icon: SlidersHorizontal, action: () => setUi("settingsOpen", true) };

export const FAVORITE_COLORS = ["#ef4444", "#f59e0b", "#22c55e", "#06b6d4", "#3b82f6", "#a855f7", "#ec4899"];

/** ↑ ↓ Home End move between the sidebar's items (as in Explorer's navigation pane) instead of
 *  Tab through each one; Enter opens the one with the focus. */
function arrowKeys(e: KeyboardEvent) {
  if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key) || e.altKey || e.ctrlKey || e.shiftKey) return;
  const at = e.target as HTMLElement;
  if (at.closest("input, textarea")) return;
  const items = [...(e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>(".side-item")].filter((el) => el.offsetParent);
  if (!items.length) return;
  const i = items.indexOf(at.closest<HTMLElement>(".side-item")!);
  const step = e.key === "ArrowDown" ? 1 : -1;
  const next = e.key === "Home" ? 0 : e.key === "End" ? items.length - 1 : i < 0 ? 0 : Math.min(items.length - 1, Math.max(0, i + step));
  e.preventDefault();
  items[next].focus();
  items[next].scrollIntoView({ block: "nearest" });
}

export default function Sidebar(props: { overlay?: boolean }) {
  const current = () => activeTab()?.p ?? "";
  const shownRecents = () => recents().filter((r) => !samePath(r.path, current())).slice(0, 8);
  /** Generic or repeated names ("debug", two "src") get their parent folder alongside. */
  const needsParent = (path: string) => {
    const name = basename(path).toLowerCase();
    return GENERIC_NAMES.has(name) || shownRecents().filter((r) => basename(r.path).toLowerCase() === name).length > 1;
  };
  // As an overlay, clicking outside closes it.
  createEffect(() => {
    if (!props.overlay) return;
    const away = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest(".sidebar, .menu, .sidebar-toggle")) setUi("sidebarOverlay", false);
    };
    window.addEventListener("mousedown", away, true);
    onCleanup(() => window.removeEventListener("mousedown", away, true));
  });

  const startResize = (e: MouseEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = ui.sidebarWidth;
    const move = (ev: MouseEvent) => setUi("sidebarWidth", Math.max(190, Math.min(420, startW + ev.clientX - startX)));
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      persistSession();
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  return (
    <aside class="sidebar panel" classList={{ overlay: props.overlay }} style={{ width: `${ui.sidebarWidth}px` }}>
      <div class="sidebar-scroll" onKeyDown={arrowKeys}>

        <Section id="favorites" title="Favorites">
          <Show when={favorites().length} fallback={<div class="side-empty">Ctrl+D pins the selected item (or the folder you're in) here</div>}>
            <FavoriteList current={current()} />
          </Show>
        </Section>

        <Show when={settings.savedSearches.length}>
          <Section id="searches" title="Searches">
            <For each={settings.savedSearches}>
              {(s) => {
                const path = () => searchPath(s.query);
                const open = (inNewTab = false) => (inNewTab ? newTab(path(), false) : navigate(activeTab(), path()));
                return (
                  <button class="side-item" classList={{ active: isSearch(current()) && searchQuery(current()).toLowerCase() === s.query.toLowerCase() }}
                    title={`${s.name}\n${s.query}\nAlways lists the current matches`}
                    onClick={() => open()}
                    onMouseDown={(e) => e.button === 1 && (e.preventDefault(), open(true))}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      openMenu(e.clientX, e.clientY, [
                        { label: "Open", icon: Search, action: () => open() },
                        { label: "Open in new tab", icon: FolderOpen, action: () => open(true) },
                        { separator: true },
                        { label: "Edit search…", icon: Pencil, action: () => openSearchPalette(s.query) },
                        { label: "Rename…", icon: Pencil, action: () => promptSaveSearch(s.query) },
                        { label: "Remove from sidebar", icon: X, action: () => removeSavedSearch(s.query) },
                      ]);
                    }}>
                    <Search size={15} />
                    <span class="side-label">{s.name}</span>
                    <span class="side-x" title="Remove from sidebar" onClick={(e) => { e.stopPropagation(); removeSavedSearch(s.query); }}><X size={12} /></span>
                  </button>
                );
              }}
            </For>
          </Section>
        </Show>

        <Section id="places" title="Places">
          <For each={places().known.filter((k) => placeShown(k.id))}>
            {(k) => (
              <SideItem icon={KNOWN_ICONS[k.id] ?? FolderOpen} label={k.name} path={k.path}
                active={samePath(k.path, current())}
                menuExtra={[{ separator: true }, { label: "Hide from sidebar", icon: EyeOff, action: () => setPlaceShown(k.id, false) }, customize]} />
            )}
          </For>
          <Show when={placeShown("recycle")}>
            <button class="side-item" classList={{ active: isRecycle(current()) }} data-drop={RECYCLE}
              title={places().recycle.items ? `Recycle Bin: ${places().recycle.items} items, ${formatSize(places().recycle.bytes)}\nDrop files here to delete them` : "Recycle Bin (empty)\nDrop files here to delete them"}
              onClick={() => goTo(RECYCLE)}
              onMouseDown={(e) => e.button === 1 && (e.preventDefault(), goTo(RECYCLE, true))}
              onContextMenu={(e) => { e.preventDefault(); openMenu(e.clientX, e.clientY, [...binPlaceMenu(() => setPlaceShown("recycle", false)), customize]); }}>
              <Trash2 size={15} />
              <span class="side-label">Recycle Bin</span>
              <Show when={places().recycle.items}><span class="side-badge">{places().recycle.items}</span></Show>
            </button>
          </Show>
        </Section>

        <Show when={places().wsl.length}>
          <Section id="linux" title="Linux">
            <For each={places().wsl}>
              {(d) => (
                <SideItem icon={Terminal} label={d.name} path={d.path} active={isWithin(current(), d.path)}
                  extra={d.isDefault ? <span class="side-badge">default</span> : undefined} />
              )}
            </For>
          </Section>
        </Show>

        <Section id="servers" title="Servers" action={
          <button class="icon-btn tiny" title="Add a server (SSH)" onClick={(e) => { e.stopPropagation(); editConnection(null); }}><Plus size={13} /></button>
        }>
          <Show when={connections().length} fallback={
            <button class="side-empty side-add" onClick={() => editConnection(null)}>Add a server to browse its files over SSH</button>
          }>
            <For each={connections()}>{(c) => <ServerItem connection={c} current={current()} />}</For>
          </Show>
        </Section>

        <Section id="drives" title="Drives">
          <For each={places().drives}>
            {(d) => {
              const used = () => (d.total ? 1 - d.free / d.total : 0);
              return (
                <button class="drive" classList={{ active: isWithin(current(), d.path) }}
                  data-drop={d.path}
                  onContextMenu={(e) => { e.preventDefault(); openMenu(e.clientX, e.clientY, placeMenu(d.path, goTo, e.clientX, e.clientY)); }}
                  onClick={() => goTo(d.path)}
                  onMouseDown={(e) => e.button === 1 && (e.preventDefault(), goTo(d.path, true))}
                  title={`${d.label || "Local Disk"} (${d.path}) · ${d.fs}\n${formatSize(d.free)} free of ${formatSize(d.total)}`}>
                  <div class="drive-top">
                    <Dynamic component={d.removable ? Usb : HardDrive} size={15} />
                    <Show when={/^[a-z]:/i.test(d.path)}><span class="drive-letter">{d.path[0].toUpperCase()}</span></Show>
                    <span class="drive-name">{d.label || "Local Disk"}</span>
                    <span class="drive-free">{formatSize(d.free)} free</span>
                  </div>
                  <div class="drive-bar"><div classList={{ full: used() > 0.9 }} style={{ width: `${used() * 100}%` }} /></div>
                </button>
              );
            }}
          </For>
        </Section>

        <Section id="recents" title="Recent" action={
          <button class="icon-btn tiny" title="Clear recents"
            onClick={async (e) => { e.stopPropagation(); setRecents(await api.clearRecents()); }}><Trash2 size={12} /></button>
        }>
          <For each={shownRecents()}>
            {(r) => {
              // A drive root reads like the Drives section: letter badge + the drive's name.
              const letter = /^([a-z]):\\?$/i.exec(r.path)?.[1].toUpperCase();
              const label = () => letter
                ? places().drives.find((d) => d.path[0].toUpperCase() === letter)?.label || "Local Disk"
                : basename(r.path);
              return (
                <SideItem icon={Clock} label={label()} badge={letter} path={r.path} title={r.path}
                  extra={<Show when={!letter && needsParent(r.path)}><span class="side-hint">{basename(parent(r.path) ?? "")}</span></Show>}
                  onRemove={async () => setRecents(await api.removeRecent(r.path))} />
              );
            }}
          </For>
        </Section>
      </div>

      <Show when={sectionShown("dropzone")}><Shelf /></Show>
      <div class="sidebar-foot">
        <button class="side-item" onClick={() => setUi("settingsOpen", true)} title="Settings (Ctrl+,)">
          <SettingsIcon size={15} /><span>Settings</span>
        </button>
        <AppVersion />
      </div>
      <div class="resizer" onMouseDown={startResize} onDblClick={() => { setUi("sidebarWidth", 248); persistSession(); }} />
    </aside>
  );
}

/** "v0.3.0" in the corner (plus DEV in dev builds); a dot when an update is waiting. */
function AppVersion() {
  const fresh = () => updater.phase === "available" && !!updater.version;
  const open = () => {
    if (fresh()) return showWhatsNew();
    setUi("settingsOpen", true);
    requestAnimationFrame(() => document.querySelector(".card.about")?.scrollIntoView({ block: "end", behavior: "smooth" }));
  };
  return (
    <Show when={updater.current}>
      <button class="app-version" classList={{ fresh: fresh() }} onClick={open}
        title={fresh() ? `Ferret ${updater.version} is available: see what's new` : `Ferret ${updater.current}${import.meta.env.DEV ? " (development build)" : ""}`}>
        <Show when={import.meta.env.DEV}><span class="dev-pill">DEV</span></Show>
        <span>v{updater.current}</span>
        <Show when={fresh()}><span class="update-dot" /></Show>
      </button>
    </Show>
  );
}

/** A server connection: opens (connecting if need be) on click; a dot while connected. */
function ServerItem(props: { connection: Connection; current: string }) {
  const c = () => props.connection;
  const state = () => remoteState[c().id] ?? "disconnected";
  const here = () => remoteParts(props.current)?.id === c().id;
  const where = () => `${c().user ? `${c().user}@` : ""}${c().host || c().sshAlias}${c().port !== 22 ? `:${c().port}` : ""}`;
  const menu = (e: MouseEvent) => {
    e.preventDefault();
    openMenu(e.clientX, e.clientY, [
      { label: state() === "connected" ? "Open" : "Connect", icon: Plug, action: () => openConnection(c().id) },
      { label: "Open in new tab", icon: FolderOpen, action: () => openConnection(c().id, true) },
      { separator: true },
      { label: "Edit…", icon: Pencil, action: () => editConnection(c()) },
      ...(state() === "connected" ? [{ label: "Disconnect", icon: Unplug, action: () => disconnect(c().id) }] : []),
      { label: "Remove…", icon: X, action: () => removeConnection(c()) },
    ]);
  };
  return (
    <button class="side-item server-item" classList={{ active: here() }}
      title={`${c().name}\n${where()}${state() === "connected" ? "\nConnected" : ""}`}
      onClick={() => openConnection(c().id)}
      onMouseDown={(e) => e.button === 1 && (e.preventDefault(), openConnection(c().id, true))}
      onContextMenu={menu}>
      <Server size={15} />
      <span class="side-label">{c().name}</span>
      <Show when={state() !== "disconnected"}>
        <span class="server-dot" classList={{ connecting: state() === "connecting" }} title={state() === "connecting" ? "Connecting…" : "Connected"} />
      </Show>
    </button>
  );
}

function Section(props: { id: string; title: string; children: JSX.Element; action?: JSX.Element }) {
  const collapsed = () => ui.collapsedSections.includes(props.id);
  const toggle = () => {
    setUi("collapsedSections", (s) => (s.includes(props.id) ? s.filter((x) => x !== props.id) : [...s, props.id]));
    persistSession();
  };
  return (
    <Show when={sectionShown(props.id)}>
    <section class="side-section">
      <div class="side-title" onClick={toggle}
        onContextMenu={(e) => {
          e.preventDefault();
          openMenu(e.clientX, e.clientY, [
            { label: collapsed() ? "Expand" : "Collapse", action: toggle },
            { label: `Hide ${props.title}`, icon: EyeOff, action: () => setSectionShown(props.id, false) },
            customize,
          ]);
        }}>
        <span>{props.title}</span>
        <span class="side-title-actions">
          {props.action}
          <ChevronDown size={13} classList={{ rot: collapsed() }} />
        </span>
      </div>
      <Show when={!collapsed()}>{props.children}</Show>
    </section>
    </Show>
  );
}

function SideItem(props: {
  icon: any; label: string; path: string; active?: boolean; title?: string; extra?: JSX.Element; onRemove?: () => void;
  menuExtra?: MenuItem[];
  /** Drive letter shown as a badge before the label. */
  badge?: string;
}) {
  return (
    <button class="side-item" classList={{ active: props.active }} title={props.title ?? props.path}
      data-drop={props.path}
      onClick={() => goTo(props.path)}
      onMouseDown={(e) => e.button === 1 && (e.preventDefault(), goTo(props.path, true))}
      onContextMenu={(e) => {
        e.preventDefault();
        openMenu(e.clientX, e.clientY, placeMenu(props.path, goTo, e.clientX, e.clientY, [
          ...(props.onRemove ? [{ label: "Remove from recents", icon: X, action: props.onRemove }] : []),
          ...(props.menuExtra ?? []),
        ]));
      }}>
      {/* A drive's letter badge takes the icon's place, so names stay aligned. */}
      <Show when={props.badge} fallback={<Dynamic component={props.icon} size={15} />}>
        <span class="drive-letter side-badge">{props.badge}</span>
      </Show>
      <span class="side-label">{props.label}</span>
      {props.extra}
      <Show when={props.onRemove}>
        <span class="side-x" title="Remove" onClick={(e) => { e.stopPropagation(); props.onRemove!(); }}><X size={12} /></span>
      </Show>
    </button>
  );
}

function FavoriteList(props: { current: string }) {
  const [renaming, setRenaming] = createSignal<string | null>(null);

  const reorder = async (from: number, to: number) => {
    const list = favorites().map((f) => f.path);
    const [moved] = list.splice(from, 1);
    list.splice(to, 0, moved);
    setFavorites(await api.reorderFavorites(list));
  };

  return (
    <For each={favorites()}>
      {(f, i) => (
        <Show when={renaming() !== f.path} fallback={
          <input class="side-rename" value={f.name} ref={(el) => queueMicrotask(() => { el.focus(); el.select(); })}
            onKeyDown={async (e) => {
              e.stopPropagation();
              if (e.key === "Enter") { setFavorites(await api.updateFavorite(f.path, e.currentTarget.value || f.name, f.color)); setRenaming(null); }
              if (e.key === "Escape") setRenaming(null);
            }}
            onBlur={() => setRenaming(null)} />
        }>
          <button
            class="side-item fav"
            classList={{ active: samePath(f.path, props.current) }}
            title={f.path}
            data-index={i()}
            data-drop={f.isDir ? f.path : undefined}
            onClick={() => goTo(f.path)}
            onMouseDown={(e) => {
              if (e.button === 1) { e.preventDefault(); goTo(f.path, true); }
              else beginReorder(e, ".side-item.fav", i(), reorder);
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              openMenu(e.clientX, e.clientY, [
                { label: "Open", icon: FolderOpen, action: () => goTo(f.path) },
                { label: "Open in new tab", icon: FolderOpen, disabled: !f.isDir, action: () => goTo(f.path, true) },
                { label: "Rename", icon: Pencil, action: () => setRenaming(f.path) },
                { label: "Color", row: [
                  { label: "–", title: "No color", active: !f.color, action: async () => setFavorites(await api.updateFavorite(f.path, f.name, null)) },
                  ...FAVORITE_COLORS.map((c) => ({
                    label: "●", title: c, color: c, active: f.color === c,
                    action: async () => setFavorites(await api.updateFavorite(f.path, f.name, c)),
                  })),
                ] },
                { separator: true },
                { label: "Remove from favorites", icon: Trash2, danger: true, action: async () => setFavorites(await api.removeFavorite(f.path)) },
              ]);
            }}
          >
            <Show when={f.color} fallback={
              f.isDir ? <Star size={15} class="fav-star" /> : <FileIcon entry={{ isDir: false, ext: f.path.split(".").pop()?.toLowerCase() ?? "", isSymlink: false }} size={15} />
            }>
              <span class="fav-dot" style={{ background: f.color! }} />
            </Show>
            <span class="side-label">{f.name}</span>
          </button>
        </Show>
      )}
    </For>
  );
}

/** Dropzone: park files here from anywhere, navigate freely, then drag them out. */
function Shelf() {
  const items = () => ui.shelf;
  const name = (p: string) => basename(p);
  const ext = (p: string) => (name(p).includes(".") ? name(p).split(".").pop()!.toLowerCase() : "");
  return (
    <div class="shelf" data-drop={SHELF} classList={{ filled: items().length > 0 }}>
      <div class="shelf-head">
        <Inbox size={14} />
        <span>Dropzone</span>
        <Show when={items().length}>
          <span class="shelf-count">{items().length}</span>
          <span class="shelf-spacer" />
          <button class="shelf-all" title="Drag all items out"
            onMouseDown={(e) => beginDrag(e, () => [...items()])}>Drag all</button>
          <button class="icon-btn tiny" title="Clear shelf" onClick={() => removeFromShelf([...items()])}><X size={12} /></button>
        </Show>
      </div>
      <Show when={items().length} fallback={<div class="shelf-hint">Drop files here and browse freely</div>}>
        <div class="shelf-list">
          <For each={items()}>
            {(p) => (
              <div class="shelf-item" title={`${p}\nDrag out to copy/move · double-click to show`}
                onMouseDown={(e) => { if (!(e.target as HTMLElement).closest("button")) beginDrag(e, () => [p]); }}
                onDblClick={() => goTo(p)}>
                <FileIcon entry={{ isDir: !ext(p), ext: ext(p), isSymlink: false }} size={14} />
                <span class="shelf-name">{name(p)}</span>
                <button class="side-x" title="Remove from shelf" onClick={() => removeFromShelf([p])}><X size={12} /></button>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

export const shelfHas = (p: string) => ui.shelf.some((x) => pathKey(x) === pathKey(p));
