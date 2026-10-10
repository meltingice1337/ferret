import {
  ChevronDown, Clock, Download, EyeOff, FileText, FolderOpen, HardDrive, House, Image, Monitor, Music, Pencil,
  Inbox, Settings as SettingsIcon, SlidersHorizontal, Star, Terminal, Trash2, Usb, Video, X,
} from "lucide-solid";
import { createEffect, createSignal, For, type JSX, onCleanup, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import { api } from "../lib/api";
import { formatSize } from "../lib/format";
import { basename, isRecycle, isWithin, RECYCLE, samePath } from "../lib/path";
import {
  activeTab, favorites, goTo, openMenu, persistSession, persistSettings, places, recents,
  removeFromShelf, setFavorites, setRecents, setSettings, settings, setUi, SHELF, ui, type MenuItem,
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
  { id: "favorites", name: "Favorites" }, { id: "places", name: "Places" }, { id: "linux", name: "Linux (WSL)" },
  { id: "drives", name: "Drives" }, { id: "recents", name: "Recent" }, { id: "dropzone", name: "Dropzone" },
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

export default function Sidebar(props: { overlay?: boolean }) {
  const current = () => activeTab()?.p ?? "";
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
      <div class="sidebar-scroll">

        <Section id="favorites" title="Favorites">
          <Show when={favorites().length} fallback={<div class="side-empty">Star a folder (Ctrl+D) to pin it here</div>}>
            <FavoriteList current={current()} />
          </Show>
        </Section>

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
                    <span class="drive-name">{d.label || "Local Disk"} <span class="muted">{d.path.replace(/\\$/, "")}</span></span>
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
          <For each={recents().filter((r) => !samePath(r.path, current())).slice(0, 8)}>
            {(r) => (
              <SideItem icon={Clock} label={basename(r.path)} path={r.path} title={r.path}
                onRemove={async () => setRecents(await api.removeRecent(r.path))} />
            )}
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
      <Dynamic component={props.icon} size={15} />
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
