import { Check, ChevronRight, LoaderCircle } from "lucide-solid";
import { createEffect, createSignal, For, on, onCleanup, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import { closeMenu, ui, type MenuItem } from "../lib/state";

export default function ContextMenu() {
  createEffect(() => {
    if (!ui.menu) return;
    const onDown = (e: MouseEvent) => !(e.target as HTMLElement).closest(".menu") && closeMenu();
    const onKey = (e: KeyboardEvent) => {
      // A submenu with focus handles Escape itself (closing just that level).
      if (e.key === "Escape" && !(document.activeElement as HTMLElement | null)?.closest(".menu.sub")) {
        e.preventDefault(); e.stopPropagation(); closeMenu();
      }
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
      {(m) => <MenuList items={m.items} x={m.x} y={m.y} root hold={() => !!ui.menu?.hold} />}
    </Show>
  );
}

interface ListProps {
  items: MenuItem[];
  x: number;
  y: number;
  root?: boolean;
  parentWidth?: number;
  /** Root only: stay hidden while other apps' entries load. */
  hold?: () => boolean;
  /** Opened from the keyboard: focus the first entry. */
  focusFirst?: boolean;
  /** Submenus: close this level and give focus back to the parent. */
  onClose?: () => void;
  /** Submenus: the pointer arrived, so the parent stops any pending switch. */
  onEnter?: () => void;
  ref?: (el: HTMLDivElement) => void;
}

/** Is point p inside triangle abc? */
function inTriangle(p: Pt, a: Pt, b: Pt, c: Pt) {
  const s = (p1: Pt, p2: Pt, p3: Pt) => (p1.x - p3.x) * (p2.y - p3.y) - (p2.x - p3.x) * (p1.y - p3.y);
  const d1 = s(p, a, b), d2 = s(p, b, c), d3 = s(p, c, a);
  return !((d1 < 0 || d2 < 0 || d3 < 0) && (d1 > 0 || d2 > 0 || d3 > 0));
}
type Pt = { x: number; y: number };

function MenuList(props: ListProps) {
  let el!: HTMLDivElement;
  let subEl: HTMLDivElement | undefined;
  const [open, setOpen] = createSignal<{ index: number; items: MenuItem[] | null; x: number; y: number; w: number; kb: boolean } | null>(null);
  const [focus, setFocus] = createSignal(-1);
  let hoverTimer: number | undefined;
  // Recent pointer positions, to tell "heading for the open submenu" from "browsing this menu".
  let prev: Pt = { x: 0, y: 0 };
  let last: Pt = { x: 0, y: 0 };

  const hasSub = (it: MenuItem) => !!(it.submenu || it.loadSubmenu);
  const actionable = () => props.items.map((it, i) => (!it.separator && !it.heading && !it.row && !it.actions && !it.disabled ? i : -1)).filter((i) => i >= 0);

  // Position once measured: keep inside the window, flip submenus to the left when needed.
  const place = (node: HTMLDivElement) => {
    el = node;
    props.ref?.(node);
    position(true);
  };
  // Entries added after opening (other apps' items) can make the menu taller: re-fit it.
  createEffect(on(() => props.items.length, () => el && position(false), { defer: true }));
  createEffect(on(() => props.hold?.(), (h) => { if (!h && el) position(true); }, { defer: true }));
  const position = (first: boolean) => {
    requestAnimationFrame(() => {
      if (props.hold?.()) return;
      // Layout size, not getBoundingClientRect: the opening animation scales the menu down a little.
      const r = { width: el.offsetWidth, height: el.offsetHeight };
      let x = props.x;
      let y = props.y;
      if (x + r.width > window.innerWidth - 6) x = props.root ? window.innerWidth - r.width - 6 : props.x - r.width - (props.parentWidth ?? 0) + 4;
      if (y + r.height > window.innerHeight - 6) y = Math.max(6, (props.root ? props.y - r.height : window.innerHeight - r.height - 6));
      el.style.left = `${Math.max(6, x)}px`;
      el.style.top = `${Math.max(6, y)}px`;
      el.style.visibility = "visible";
      if (first && (props.root || props.focusFirst)) {
        el.focus({ preventScroll: true });
        if (props.focusFirst) setFocus(actionable()[0] ?? -1);
      }
    });
  };

  const openSub = async (index: number, it: MenuItem, target: HTMLElement, kb = false) => {
    const r = target.getBoundingClientRect();
    const w = el.getBoundingClientRect().width;
    if (open()?.index === index && !kb) return;
    if (it.submenu) return setOpen({ index, items: it.submenu, x: r.right - 3, y: r.top - 5, w, kb });
    if (it.loadSubmenu) {
      setOpen({ index, items: null, x: r.right - 3, y: r.top - 5, w, kb });
      const items = await it.loadSubmenu().catch(() => [{ label: "Couldn't load", disabled: true } as MenuItem]);
      setOpen((o) => (o && o.index === index ? { ...o, items: items.length ? items : [{ label: "Nothing here", disabled: true }] } : o));
    }
  };
  const closeSub = () => {
    setOpen(null);
    el.focus({ preventScroll: true });
  };

  /** The pointer is travelling from the open submenu's item toward the submenu. */
  const aimingAtSub = () => {
    const sub = subEl?.isConnected ? subEl.getBoundingClientRect() : null;
    if (!open() || !sub) return false;
    const edge = sub.left >= prev.x ? sub.left : sub.right;
    return inTriangle(last, prev, { x: edge, y: sub.top - 8 }, { x: edge, y: sub.bottom + 8 });
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape" && !props.root) { e.preventDefault(); e.stopPropagation(); return props.onClose?.(); }
    const idx = actionable();
    if (!idx.length) return;
    const pos = idx.indexOf(focus());
    if (e.key === "ArrowDown") { e.preventDefault(); e.stopPropagation(); setOpen(null); setFocus(idx[(pos + 1) % idx.length]); }
    else if (e.key === "ArrowUp") { e.preventDefault(); e.stopPropagation(); setOpen(null); setFocus(idx[(pos - 1 + idx.length) % idx.length]); }
    else if (e.key === "Home") { e.preventDefault(); e.stopPropagation(); setFocus(idx[0]); }
    else if (e.key === "End") { e.preventDefault(); e.stopPropagation(); setFocus(idx[idx.length - 1]); }
    else if ((e.key === "ArrowRight" || e.key === "Enter") && focus() >= 0) {
      e.preventDefault(); e.stopPropagation();
      const it = props.items[focus()];
      const node = el.querySelector<HTMLElement>(`:scope > .menu-item[data-i="${focus()}"]`);
      if (hasSub(it)) openSub(focus(), it, node ?? el, true);
      else if (e.key === "Enter") { closeMenu(); it.action?.(); }
    } else if (e.key === "ArrowLeft" && !props.root) {
      e.preventDefault(); e.stopPropagation();
      props.onClose?.();
    }
  };

  return (
    <>
      <div ref={place} class="menu" classList={{ sub: !props.root }} tabIndex={-1} role="menu"
        style={{ left: `${props.x}px`, top: `${props.y}px`, visibility: "hidden" }}
        onContextMenu={(e) => e.preventDefault()} onKeyDown={onKeyDown}
        onMouseEnter={() => props.onEnter?.()}
        onMouseMove={(e) => { prev = last; last = { x: e.clientX, y: e.clientY }; }}>
        <For each={props.items}>
          {(it, i) => (
            <Show when={!it.separator} fallback={<div class="menu-sep" />}>
              <Show when={!it.heading} fallback={<div class="menu-heading">{it.label}</div>}>
                <Show when={!it.actions} fallback={
                  <div class="menu-actions" onMouseEnter={() => { clearTimeout(hoverTimer); setOpen(null); setFocus(-1); }}>
                    <For each={it.actions}>
                      {(b) => (
                        <button class="menu-action" classList={{ danger: b.danger }} title={b.title} aria-label={b.title} disabled={b.disabled}
                          onClick={() => { closeMenu(); b.action(); }}>
                          <Dynamic component={b.icon} size={16} />
                        </button>
                      )}
                    </For>
                  </div>
                }>
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
                    role="menuitem"
                    data-i={i()}
                    classList={{ danger: it.danger, focus: focus() === i(), "sub-open": open()?.index === i() }}
                    disabled={it.disabled}
                    onMouseEnter={(e) => {
                      clearTimeout(hoverTimer);
                      const target = e.currentTarget;
                      const go = () => {
                        setFocus(i());
                        if (hasSub(it)) openSub(i(), it, target);
                        else setOpen(null);
                      };
                      // Crossing other items on the way to the open submenu doesn't switch away from it.
                      if (open() && open()!.index !== i() && aimingAtSub()) { hoverTimer = window.setTimeout(go, 300); return; }
                      setFocus(i());
                      hoverTimer = window.setTimeout(go, hasSub(it) ? 120 : 150);
                    }}
                    onClick={(e) => {
                      clearTimeout(hoverTimer);
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
            </Show>
          )}
        </For>
      </div>
      <Show when={open()} keyed>
        {(o) => (
          <Show when={o.items} fallback={
            <div class="menu sub" ref={(n) => (subEl = n)} style={{ left: `${o.x}px`, top: `${o.y}px` }}><div class="menu-loading"><LoaderCircle size={14} class="spin" /> Loading…</div></div>
          }>
            {(items) => (
              <MenuList items={items()} x={o.x} y={o.y} parentWidth={o.w} focusFirst={o.kb}
                ref={(n) => (subEl = n)}
                onClose={closeSub}
                onEnter={() => { clearTimeout(hoverTimer); setFocus(o.index); }} />
            )}
          </Show>
        )}
      </Show>
    </>
  );
}
