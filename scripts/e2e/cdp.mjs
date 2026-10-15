// A small DevTools-protocol client for the end-to-end tests: evaluate in the page, send real
// key presses and clicks, take screenshots, and collect every uncaught error the page throws.

const KEYS = {
  Enter: [13, "Enter"], Backspace: [8, "Backspace"], Delete: [46, "Delete"], Escape: [27, "Escape"], Tab: [9, "Tab"],
  Home: [36, "Home"], End: [35, "End"], ArrowUp: [38, "ArrowUp"], ArrowDown: [40, "ArrowDown"], ArrowLeft: [37, "ArrowLeft"],
  ArrowRight: [39, "ArrowRight"], F2: [113, "F2"], F5: [116, "F5"], ",": [188, "Comma"],
};
const MODS = { Alt: 1, Control: 2, Meta: 4, Shift: 8 };

export async function connect(port, { timeoutMs = 60_000 } = {}) {
  const start = Date.now();
  let page;
  while (!page) {
    try {
      const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      page = targets.find((t) => t.type === "page" && /tauri\.localhost|localhost:1420/.test(t.url));
    } catch {}
    if (!page) {
      if (Date.now() - start > timeoutMs) throw new Error(`Ferret's page didn't show up on port ${port}`);
      await sleep(300);
    }
  }
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((r, j) => { ws.addEventListener("open", r); ws.addEventListener("error", j); });
  let id = 0;
  const pending = new Map();
  const errors = [];
  ws.addEventListener("message", (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
    if (m.method === "Runtime.exceptionThrown") {
      const d = m.params.exceptionDetails;
      errors.push((d.exception?.description ?? d.text ?? "error").split("\n").slice(0, 3).join(" | "));
    }
    if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") {
      errors.push("console.error: " + m.params.args.map((a) => a.value ?? a.description).join(" ").slice(0, 300));
    }
  });
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const i = ++id;
    const timer = setTimeout(() => { pending.delete(i); reject(new Error(`${method} timed out`)); }, 30_000);
    pending.set(i, (m) => { clearTimeout(timer); m.error ? reject(new Error(m.error.message)) : resolve(m.result); });
    ws.send(JSON.stringify({ id: i, method, params }));
  });
  await send("Runtime.enable");
  await send("Page.enable");

  const c = {
    errors,
    send,
    /** Evaluates `expr` (async allowed) in the page and returns its JSON value; throws on page errors. */
    async eval(expr) {
      const r = await send("Runtime.evaluate", { expression: `(async () => (${expr}))()`, awaitPromise: true, returnByValue: true });
      if (r.exceptionDetails) throw new Error("in page: " + (r.exceptionDetails.exception?.description ?? r.exceptionDetails.text).split("\n")[0]);
      return r.result.value;
    },
    /** Polls `expr` until it's truthy; returns its value. */
    async waitFor(expr, { timeoutMs = 8000, what = expr } = {}) {
      const start = Date.now();
      let last;
      while (Date.now() - start < timeoutMs) {
        try { last = await c.eval(expr); } catch (e) { last = String(e); }
        if (last) return last;
        await sleep(100);
      }
      throw new Error(`timed out waiting for: ${what} (last: ${JSON.stringify(last)})`);
    },
    /** A real key press: `key("Delete")`, `key("c", ["Control"])`, `key("F5")`. */
    async key(k, mods = []) {
      const m = mods.reduce((a, x) => a | MODS[x], 0);
      const known = KEYS[k];
      const letter = !known && /^[a-z0-9]$/i.test(k);
      const vk = known ? known[0] : k.toUpperCase().charCodeAt(0);
      const code = known ? known[1] : letter ? (/\d/.test(k) ? `Digit${k}` : `Key${k.toUpperCase()}`) : k;
      const text = letter && !(m & (MODS.Control | MODS.Alt | MODS.Meta)) ? k : undefined;
      await send("Input.dispatchKeyEvent", { type: text ? "keyDown" : "rawKeyDown", key: k, code, windowsVirtualKeyCode: vk, modifiers: m, text });
      await send("Input.dispatchKeyEvent", { type: "keyUp", key: k, code, windowsVirtualKeyCode: vk, modifiers: m });
      await sleep(60);
    },
    /** Types text into whatever has focus. */
    async type(text) {
      await send("Input.insertText", { text });
      await sleep(60);
    },
    /** A real click in the middle of the first element matching `selector` (optionally with text). */
    async click(selector, { text, button = "left", count = 1, mods = [] } = {}) {
      const at = await c.eval(`(() => {
        const els = [...document.querySelectorAll(${JSON.stringify(selector)})].filter((e) => ${text ? `e.textContent.includes(${JSON.stringify(text)})` : "true"});
        const el = els[0]; if (!el) return null;
        el.scrollIntoView({ block: "nearest" });
        const b = el.getBoundingClientRect(); return { x: b.left + b.width / 2, y: b.top + b.height / 2 };
      })()`);
      if (!at) throw new Error(`nothing to click: ${selector}${text ? ` containing “${text}”` : ""}`);
      const modifiers = mods.reduce((a, x) => a | MODS[x], 0);
      await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: at.x, y: at.y });
      for (let i = 1; i <= count; i++) {
        await send("Input.dispatchMouseEvent", { type: "mousePressed", x: at.x, y: at.y, button, clickCount: i, modifiers });
        await send("Input.dispatchMouseEvent", { type: "mouseReleased", x: at.x, y: at.y, button, clickCount: i, modifiers });
      }
      await sleep(80);
    },
    async screenshot(file) {
      const r = await send("Page.captureScreenshot", { format: "png" });
      const { writeFileSync } = await import("node:fs");
      writeFileSync(file, Buffer.from(r.data, "base64"));
    },
    close: () => ws.close(),
  };
  return c;
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
