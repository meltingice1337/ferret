import { Check, ChevronRight, LoaderCircle } from "lucide-solid";
import { createEffect, createSignal, For, on, onCleanup, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import { closeMenu, ui, type MenuItem } from "../lib/state";

export default function ContextMenu() {
  createEffect(() => {
    if (!ui.menu) return;
    const onDown = (e: MouseEvent) => !(e.target as HTMLElement).closest(".menu") && closeMenu();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closeMenu(); }
    };
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", closeMenu);
    window.addEventListener("resize", closeMenu);
    onCleanup(() => {
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", closeMenu);
      window.removeEventListener("resize", closeMenu);
    });
  });

  return (
    <Show when={ui.menu} keyed>
      {(m) => <MenuList items={m.items} x={m.x} y={m.y} root />}
    </Show>
  );
}

function MenuList(props: { items: MenuItem[]; x: number; y: number; root?: boolean; parentWidth?: number }) {
  let el!: HTMLDivElement;
  const [open, setOpen] = createSignal<{ index: number; items: MenuItem[] | null; x: number; y: number; w: number } | null>(null);
  const [focus, setFocus] = createSignal(-1);
  let hoverTimer: number | undefined;

  // Position once measured: keep inside the window, flip submenus to the left when needed.
  const place = (node: HTMLDivElement) => {
    el = node;
    position(true);
  };
  // Entries added after opening (other apps' items) can make the menu taller: re-fit it.
  createEffect(on(() => props.items.length, () => el && position(false), { defer: true }));
  const position = (first: boolean) => {
    requestAnimationFrame(() => {
      const r = el.getBoundingClientRect();
      let x = props.x;
      let y = props.y;
      if (x + r.width > window.innerWidth - 6) x = props.root ? window.innerWidth - r.width - 6 : props.x - r.width - (props.parentWidth ?? 0) + 4;
      if (y + r.height > window.innerHeight - 6) y = Math.max(6, (props.root ? props.y - r.height : window.innerHeight - r.height - 6));
      el.style.left = `${Math.max(6, x)}px`;
      el.style.top = `${Math.max(6, y)}px`;
      el.style.visibility = "visible";
      if (props.root && first) el.focus({ preventScroll: true });
    });
  };

  const openSub = async (index: number, it: MenuItem, target: HTMLElement) => {
    const r = target.getBoundingClientRect();
    const w = el.getBoundingClientRect().width;
    if (it.submenu) return setOpen({ index, items: it.submenu, x: r.right - 3, y: r.top - 5, w });
    if (it.loadSubmenu) {
      setOpen({ index, items: null, x: r.right - 3, y: r.top - 5, w });
      const items = await it.loadSubmenu().catch(() => [{ label: "Couldn't load", disabled: true } as MenuItem]);
      setOpen((o) => (o && o.index === index ? { ...o, items: items.length ? items : [{ label: "Nothing here", disabled: true }] } : o));
    }
  };

  const hasSub = (it: MenuItem) => !!(it.submenu || it.loadSubmenu);
  const actionable = () => props.items.map((it, i) => (!it.separator && !it.heading && !it.row && !it.disabled ? i : -1)).filter((i) => i >= 0);

  const onKeyDown = (e: KeyboardEvent) => {
    const idx = actionable();
    if (!idx.length) return;
    const pos = idx.indexOf(focus());
    if (e.key === "ArrowDown") { e.preventDefault(); e.stopPropagation(); setFocus(idx[(pos + 1) % idx.length]); }
    else if (e.key === "ArrowUp") { e.preventDefault(); e.stopPropagation(); setFocus(idx[(pos - 1 + idx.length) % idx.length]); }
    else if ((e.key === "ArrowRight" || e.key === "Enter") && focus() >= 0) {
      e.preventDefault(); e.stopPropagation();
      const it = props.items[focus()];
      const node = el.querySelectorAll<HTMLElement>(":scope > .menu-item")[idx.indexOf(focus())];
      if (hasSub(it)) openSub(focus(), it, node ?? el);
      else if (e.key === "Enter") { closeMenu(); it.action?.(); }
    } else if (e.key === "ArrowLeft" && !props.root) {
      e.preventDefault(); e.stopPropagation();
      el.parentElement?.closest<HTMLElement>(".menu")?.focus();
    }
  };

  return (
    <>
      <div ref={place} class="menu" tabIndex={-1} style={{ left: `${props.x}px`, top: `${props.y}px`, visibility: "hidden" }}
        onContextMenu={(e) => e.preventDefault()} onKeyDown={onKeyDown}>
        <For each={props.items}>
          {(it, i) => (
            <Show when={!it.separator} fallback={<div class="menu-sep" />}>
              <Show when={!it.heading} fallback={<div class="menu-heading">{it.label}</div>}>
                <Show when={!it.row} fallback={
                  <div class="menu-row" onMouseEnter={() => setOpen(null)}>
                    <span class="menu-row-label">{it.label}</span>
                    <For each={it.row}>
                      {(b) => (
                        <button class="menu-chip" classList={{ active: b.active }} title={b.title} style={b.color ? { color: b.color } : undefined}
                          onClick={() => { closeMenu(); b.action(); }}>{b.label}</button>
                      )}
                    </For>
                  </div>
                }>
                  <button
                    class="menu-item"
                    classList={{ danger: it.danger, focus: focus() === i(), "sub-open": open()?.index === i() }}
                    disabled={it.disabled}
                    onMouseEnter={(e) => {
                      setFocus(i());
                      clearTimeout(hoverTimer);
                      const target = e.currentTarget;
                      if (hasSub(it)) hoverTimer = window.setTimeout(() => openSub(i(), it, target), 120);
                      else hoverTimer = window.setTimeout(() => setOpen(null), 150);
                    }}
                    onClick={(e) => {
                      if (hasSub(it)) return openSub(i(), it, e.currentTarget);
                      closeMenu();
                      it.action?.();
                    }}
                  >
                    <span class="menu-icon">
                      <Show when={it.checked !== undefined} fallback={
                        <Show when={it.iconUrl} fallback={it.icon && <Dynamic component={it.icon} size={15} />}>
                          <img src={it.iconUrl!} width={16} height={16} alt="" />
                        </Show>
                      }>
                        <Show when={it.checked}><Check size={15} /></Show>
                      </Show>
                    </span>
                    <span class="menu-label">{it.label}</span>
                    <Show when={it.shortcut}><span class="menu-kbd">{it.shortcut}</span></Show>
                    <Show when={hasSub(it)}><ChevronRight size={14} class="menu-arrow" /></Show>
                  </button>
                </Show>
              </Show>
            </Show>
          )}
        </For>
      </div>
      <Show when={open()} keyed>
        {(o) => (
          <Show when={o.items} fallback={
            <div class="menu" style={{ left: `${o.x}px`, top: `${o.y}px` }}><div class="menu-loading"><LoaderCircle size={14} class="spin" /> Loading…</div></div>
          }>
            {(items) => <MenuList items={items()} x={o.x} y={o.y} parentWidth={o.w} />}
          </Show>
        )}
      </Show>
    </>
  );
}
