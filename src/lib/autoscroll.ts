// Autoscroll, like Explorer and browsers: press the mouse wheel over anything that scrolls (the
// file list, the preview, the sidebar) and move away from that point to scroll, faster the
// further you go. Hold and move (it stops when the wheel is released), or click once (it follows
// the mouse until the next click, a wheel turn or Esc). Middle-clicks something already uses
// (a folder: new tab; a tab: close) are left alone: those call preventDefault.

/** Within this distance of the starting point nothing scrolls. */
const DEAD = 12;
/** Fastest speed, pixels per second. */
const MAX = 6000;
/** A press this short, without moving, is a click: autoscroll then follows the mouse. */
const CLICK_MS = 350;

/** Speed for a distance from the starting point: slow near it, faster the further away. */
const speed = (d: number) => {
  const a = Math.abs(d) - DEAD;
  return a <= 0 ? 0 : Math.sign(d) * Math.min(MAX, 0.9 * a ** 1.6);
};

/** The nearest element under `el` that can scroll, and in which directions. */
function scrollerAt(el: Element | null) {
  for (let n = el as HTMLElement | null; n && n !== document.documentElement; n = n.parentElement) {
    const s = getComputedStyle(n);
    const y = /auto|scroll/.test(s.overflowY) && n.scrollHeight > n.clientHeight + 1;
    const x = /auto|scroll/.test(s.overflowX) && n.scrollWidth > n.clientWidth + 1;
    if (x || y) return { el: n, x, y };
  }
  return null;
}

const ARROW = (d: string) => `<path d="${d}" fill="currentColor"/>`;
/** The marker at the starting point: a dot with arrows for the directions that scroll. */
function marker(x: boolean, y: boolean) {
  const m = document.createElement("div");
  m.className = "autoscroll-mark";
  m.innerHTML = `<svg viewBox="0 0 32 32" width="32" height="32" aria-hidden="true"><circle cx="16" cy="16" r="2.4" fill="currentColor"/>${
    y ? ARROW("M16 4 L20.5 10 H11.5 Z") + ARROW("M16 28 L20.5 22 H11.5 Z") : ""}${
    x ? ARROW("M4 16 L10 11.5 V20.5 Z") + ARROW("M28 16 L22 11.5 V20.5 Z") : ""}</svg>`;
  return m;
}

let active: (() => void) | null = null;

function start(down: MouseEvent, target: { el: HTMLElement; x: boolean; y: boolean }) {
  const { el, x: canX, y: canY } = target;
  const ox = down.clientX, oy = down.clientY;
  let mx = ox, my = oy;
  let sticky = false;
  let last = performance.now();
  const began = last;
  const root = document.documentElement;
  const mark = marker(canX, canY);
  mark.style.left = `${ox}px`;
  mark.style.top = `${oy}px`;
  document.body.appendChild(mark);
  // The cursor shows where it's going: still near the point, or which way.
  const cursor = () => {
    const v = canY ? speed(my - oy) : 0, h = canX ? speed(mx - ox) : 0;
    root.dataset.autoscroll = (v < 0 ? "n" : v > 0 ? "s" : "") + (h < 0 ? "w" : h > 0 ? "e" : "") || (canX && canY ? "all" : canY ? "ns" : "ew");
  };
  cursor();
  let raf = requestAnimationFrame(function tick(now) {
    const dt = Math.min(50, now - last) / 1000;
    last = now;
    if (canY) el.scrollTop += speed(my - oy) * dt;
    if (canX) el.scrollLeft += speed(mx - ox) * dt;
    raf = requestAnimationFrame(tick);
  });
  const stop = () => {
    cancelAnimationFrame(raf);
    mark.remove();
    delete root.dataset.autoscroll;
    window.removeEventListener("mousemove", move, true);
    window.removeEventListener("mouseup", up, true);
    window.removeEventListener("mousedown", press, true);
    window.removeEventListener("keydown", key, true);
    window.removeEventListener("wheel", stop, true);
    window.removeEventListener("blur", stop);
    active = null;
  };
  const move = (e: MouseEvent) => { mx = e.clientX; my = e.clientY; cursor(); };
  const up = (e: MouseEvent) => {
    if (e.button !== 1 || sticky) return;
    // A quick click without moving: keep following the mouse until the next click.
    if (performance.now() - began < CLICK_MS && Math.hypot(mx - ox, my - oy) < DEAD) sticky = true;
    else stop();
  };
  // While following the mouse, the next press only stops it (it doesn't click what's below).
  const press = (e: MouseEvent) => { if (sticky) { e.preventDefault(); e.stopPropagation(); stop(); } };
  const key = (e: KeyboardEvent) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); stop(); } };
  window.addEventListener("mousemove", move, true);
  window.addEventListener("mouseup", up, true);
  window.addEventListener("mousedown", press, true);
  window.addEventListener("keydown", key, true);
  window.addEventListener("wheel", stop, { capture: true, passive: true } as AddEventListenerOptions);
  window.addEventListener("blur", stop);
  active = stop;
}

window.addEventListener("mousedown", (e) => {
  if (e.button !== 1 || e.defaultPrevented || active) return;
  const t = e.target as HTMLElement | null;
  // Text fields, links and Ctrl+K's results have middle-clicks of their own.
  if (!t || t.closest("input, textarea, select, a[href], [contenteditable], .palette")) return;
  const target = scrollerAt(t);
  if (!target) return;
  e.preventDefault();
  start(e, target);
});
