import { getCurrentWindow } from "@tauri-apps/api/window";
import { ChevronDown, Columns2, Copy, Minus, Plus, Search, ShieldAlert, Square, Trash2, X } from "../lib/icons";
import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import FileIcon from "./FileIcon";
import { placeMenu } from "./menus";
import { beginReorder } from "../lib/drag";
import { basename, isRecycle, isSearch, parent } from "../lib/path";
import {
  fileList, paneOf, activeId, adminWindow, closedTabs, closeTab, goTo, moveTab, newTab, openMenu, persistSession, reload, reopenClosedTab, searchTitle, setActiveId,
  tabs, type MenuItem,
} from "../lib/state";

export default function TitleBar() {
  const win = getCurrentWindow();
  let strip!: HTMLDivElement;
  const [overflowing, setOverflowing] = createSignal(false);

  // Maximized windows drop the outer margin (see features.css) and show "Restore down".
  const [maximized, setMaximized] = createSignal(false);
  onMount(() => {
    const sync = () => win.isMaximized().then((m) => {
      setMaximized(m);
      document.documentElement.toggleAttribute("data-maximized", m);
    }).catch(() => {});
    sync();
    window.addEventListener("resize", sync);
    onCleanup(() => window.removeEventListener("resize", sync));
  });

  // Keep the active tab visible when it changes, or the strip gets narrower.
  const showActive = () => {
    const el = strip?.querySelector<HTMLElement>(".tab.active");
    if (!el) return;
    const s = strip.getBoundingClientRect(), r = el.getBoundingClientRect();
    if (r.left < s.left) strip.scrollLeft -= s.left - r.left;
    else if (r.right > s.right) strip.scrollLeft += Math.min(r.right - s.right, r.left - s.left);
  };
  createEffect(on(() => [activeId(), tabs().length], () => requestAnimationFrame(showActive)));

  /** Keys on a focused tab: ←/→ (Home/End) open the next tab and keep the focus on the strip,
   *  Enter goes to its files, Delete closes it. */
  const tabKey = (e: KeyboardEvent, i: number) => {
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    const list = tabs();
    const to = e.key === "ArrowRight" ? (i + 1) % list.length : e.key === "ArrowLeft" ? (i - 1 + list.length) % list.length
      : e.key === "Home" ? 0 : e.key === "End" ? list.length - 1 : -1;
    const focusActive = () => requestAnimationFrame(() => strip.querySelector<HTMLElement>(".tab.active")?.focus());
    if (to >= 0) {
      e.preventDefault();
      setActiveId(list[to].id);
      persistSession();
      focusActive();
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      fileList()?.focus({ preventScroll: true });
    } else if (e.key === "Delete" && list.length > 1) {
      e.preventDefault();
      closeTab(list[i].id);
      focusActive();
    }
  };

  const tabMenu = (e: MouseEvent, id: number, path: string) => {
    e.preventDefault();
    const extra: MenuItem[] = [
      { separator: true },
      { label: "Duplicate tab", action: () => newTab(path) },
      { label: "Refresh", action: () => { const t = tabs().find((x) => x.id === id); if (t) reload(t); } },
      { label: "Close tab", shortcut: "Ctrl+W", disabled: tabs().length < 2, action: () => closeTab(id) },
      { label: "Close other tabs", disabled: tabs().length < 2, action: () => tabs().filter((t) => t.id !== id).forEach((t) => closeTab(t.id)) },
      { label: "Reopen closed tab", shortcut: "Ctrl+Shift+T", disabled: !closedTabs().length, action: () => reopenClosedTab() },
    ];
    openMenu(e.clientX, e.clientY, placeMenu(path, goTo, e.clientX, e.clientY, extra));
  };

  return (
    <div class="titlebar" data-tauri-drag-region>
      <div class="tabs" data-tauri-drag-region>
        <div class="tabs-scroll" role="tablist" ref={(el) => {
          strip = el;
          const update = () => {
            el.classList.toggle("more-left", el.scrollLeft > 2);
            el.classList.toggle("more-right", el.scrollLeft + el.clientWidth < el.scrollWidth - 2);
            setOverflowing(el.scrollWidth > el.clientWidth + 2);
          };
          el.addEventListener("scroll", update);
          new ResizeObserver(() => requestAnimationFrame(() => { showActive(); update(); })).observe(el);
          new MutationObserver(update).observe(el, { childList: true });
        }} data-tauri-drag-region
          onWheel={(e) => { if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) { strip.scrollLeft += e.deltaY; e.preventDefault(); } }}>
          <For each={tabs()}>
            {(tab, i) => (
              <div
                class="tab"
                classList={{ active: tab.id === activeId() }}
                // Tab reaches the open tab; the arrow keys go through the others, Delete closes one.
                role="tab"
                aria-selected={tab.id === activeId()}
                tabIndex={tab.id === activeId() ? 0 : -1}
                onKeyDown={(e) => tabKey(e, i())}
                title={paneOf(tab).p}
                data-index={i()}
                data-tab-id={tab.id}
                data-drop={isSearch(paneOf(tab).p) ? undefined : paneOf(tab).p}
                onMouseDown={(e) => {
                  if (e.button === 1) { e.preventDefault(); closeTab(tab.id); return; }
                  if (e.button !== 0) return;
                  setActiveId(tab.id);
                  persistSession();
                  requestAnimationFrame(() => fileList()?.focus({ preventScroll: true }));
                  beginReorder(e, ".tab", i(), (from, to) => moveTab(tabs()[from].id, to));
                }}
                onContextMenu={(e) => tabMenu(e, tab.id, tab.p)}
              >
                {/* A split tab is named after the pane in use. */}
                <Show when={!isRecycle(paneOf(tab).p)} fallback={<Trash2 size={14} class="tab-bin" />}>
                  <Show when={!isSearch(paneOf(tab).p)} fallback={<Search size={14} class="tab-bin" />}>
                    <FileIcon entry={{ isDir: true, ext: "", isSymlink: false }} size={14} />
                  </Show>
                </Show>
                <span class="tab-label">{isSearch(paneOf(tab).p) ? searchTitle(paneOf(tab).p) : basename(paneOf(tab).p) || "…"}</span>
                <Show when={tab.split[0]()}><span class="tab-split" title="Split view"><Columns2 size={12} /></span></Show>
                <button class="tab-close" title="Close tab (Ctrl+W)" tabIndex={-1}
                  onMouseDown={(e) => e.stopPropagation()}
                  onClick={() => closeTab(tab.id)}><X size={13} /></button>
              </div>
            )}
          </For>
        </div>
        <button class="tab-new" title="New tab (Ctrl+T)" onClick={() => newTab()}><Plus size={15} /></button>
        <Show when={overflowing()}>
          <button class="tab-new tab-list" title={`All tabs (${tabs().length})`} onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            openMenu(r.left, r.bottom + 4, [
              { heading: true, label: `${tabs().length} tabs` },
              ...tabs().map((t) => ({ label: `${basename(t.p) || t.p}  ·  ${parent(t.p) ?? ""}`, checked: t.id === activeId(), action: () => { setActiveId(t.id); persistSession(); } })),
            ]);
          }}><ChevronDown size={15} /><span class="tab-count">{tabs().length}</span></button>
        </Show>
      </div>
      {/* Tauri's drag region already maximizes on double-click. */}
      <div class="drag-fill" data-tauri-drag-region />
      <Show when={adminWindow()}>
        <span class="admin-badge" title="This window runs as administrator. Close it when you're done.">
          <ShieldAlert size={13} />Administrator
        </span>
      </Show>
      <div class="winctl">
        <button title="Minimize" tabIndex={-1} onClick={() => win.minimize()}><Minus size={15} /></button>
        <button title={maximized() ? "Restore down" : "Maximize"} tabIndex={-1} onClick={() => win.toggleMaximize()}>
          <Show when={maximized()} fallback={<Square size={12} />}><Copy size={12} class="flip-x" /></Show>
        </button>
        <button class="close" title="Close" tabIndex={-1} onClick={() => win.close()}><X size={16} /></button>
      </div>
    </div>
  );
}
