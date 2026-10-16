import { getCurrentWindow } from "@tauri-apps/api/window";
import { ChevronDown, Copy, Minus, Plus, Search, ShieldAlert, Square, Trash2, X } from "../lib/icons";
import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import FileIcon from "./FileIcon";
import { placeMenu } from "./menus";
import { beginReorder } from "../lib/drag";
import { basename, isRecycle, isSearch, parent } from "../lib/path";
import {
  activeId, adminWindow, closedTabs, closeTab, goTo, moveTab, newTab, openMenu, persistSession, reload, reopenClosedTab, searchTitle, setActiveId,
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

  // Keep the active tab visible when it changes or the strip overflows.
  createEffect(on(() => [activeId(), tabs().length], () => {
    requestAnimationFrame(() => strip?.querySelector<HTMLElement>(".tab.active")?.scrollIntoView({ inline: "nearest", block: "nearest" }));
  }));

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
        <div class="tabs-scroll" ref={(el) => {
          strip = el;
          const update = () => {
            el.classList.toggle("more-left", el.scrollLeft > 2);
            el.classList.toggle("more-right", el.scrollLeft + el.clientWidth < el.scrollWidth - 2);
            setOverflowing(el.scrollWidth > el.clientWidth + 2);
          };
          el.addEventListener("scroll", update);
          new ResizeObserver(() => requestAnimationFrame(update)).observe(el);
          new MutationObserver(update).observe(el, { childList: true });
        }} data-tauri-drag-region
          onWheel={(e) => { if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) { strip.scrollLeft += e.deltaY; e.preventDefault(); } }}>
          <For each={tabs()}>
            {(tab, i) => (
              <div
                class="tab"
                classList={{ active: tab.id === activeId() }}
                title={tab.p}
                data-index={i()}
                data-tab-id={tab.id}
                data-drop={isSearch(tab.p) ? undefined : tab.p}
                onMouseDown={(e) => {
                  if (e.button === 1) { e.preventDefault(); closeTab(tab.id); return; }
                  if (e.button !== 0) return;
                  setActiveId(tab.id);
                  persistSession();
                  requestAnimationFrame(() => document.querySelector<HTMLElement>(".files")?.focus({ preventScroll: true }));
                  beginReorder(e, ".tab", i(), (from, to) => moveTab(tabs()[from].id, to));
                }}
                onContextMenu={(e) => tabMenu(e, tab.id, tab.p)}
              >
                <Show when={!isRecycle(tab.p)} fallback={<Trash2 size={14} class="tab-bin" />}>
                  <Show when={!isSearch(tab.p)} fallback={<Search size={14} class="tab-bin" />}>
                    <FileIcon entry={{ isDir: true, ext: "", isSymlink: false }} size={14} />
                  </Show>
                </Show>
                <span class="tab-label">{isSearch(tab.p) ? searchTitle(tab.p) : basename(tab.p) || "…"}</span>
                <button class="tab-close" title="Close tab (Ctrl+W)"
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
        <button title="Minimize" onClick={() => win.minimize()}><Minus size={15} /></button>
        <button title={maximized() ? "Restore down" : "Maximize"} onClick={() => win.toggleMaximize()}>
          <Show when={maximized()} fallback={<Square size={12} />}><Copy size={12} class="flip-x" /></Show>
        </button>
        <button class="close" title="Close" onClick={() => win.close()}><X size={16} /></button>
      </div>
    </div>
  );
}
