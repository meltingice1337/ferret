// Drives the running Ferret dev build over the WebView2 DevTools protocol.
// The app must be running via `npm run tauri dev` with FERRET_DEVTOOLS=1 (debug builds then listen on :9333).
//
//   node scripts/ui.mjs shot out.png [selector]     screenshot (page or element)
//   node scripts/ui.mjs click "<selector>" [right|double|middle] [ctrl|shift|alt]
//   node scripts/ui.mjs clickText "Visible text" [right|double]
//   node scripts/ui.mjs key "Control+C"             keyboard shortcut (Playwright key names)
//   node scripts/ui.mjs type "some text"
//   node scripts/ui.mjs mouse move|down|up|click|right x y
//   node scripts/ui.mjs drag x1 y1 x2 y2            press, move in steps, release
//   node scripts/ui.mjs hover "<selector>"
//   node scripts/ui.mjs text ["<selector>"]          innerText (default: body)
//   node scripts/ui.mjs eval "<js expression>"      result printed as JSON
//   node scripts/ui.mjs wait <ms>
//   node scripts/ui.mjs logs                        console messages collected for 2s
//
// Several steps can be chained with " ;; " in one invocation:
//   node scripts/ui.mjs 'key Control+L ;; type C:\Windows ;; key Enter ;; wait 500 ;; shot a.png'

import { chromium } from "playwright-core";

const PORT = process.env.FERRET_CDP_PORT ?? "9333";

async function page() {
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${PORT}`, { timeout: 5000 });
  for (const ctx of browser.contexts()) {
    for (const p of ctx.pages()) {
      const url = p.url();
      if (url.includes("localhost:1420") || url.includes("tauri.localhost")) return p;
    }
  }
  throw new Error("Ferret page not found. Is `npm run tauri dev` running?");
}

function splitArgs(s) {
  const out = [];
  const re = /"([^"]*)"|'([^']*)'|(\S+)/g;
  let m;
  while ((m = re.exec(s))) out.push(m[1] ?? m[2] ?? m[3]);
  return out;
}

async function run(p, [cmd, ...a]) {
  const mods = (list) => list.filter((x) => ["ctrl", "shift", "alt"].includes(x)).map((x) => ({ ctrl: "Control", shift: "Shift", alt: "Alt" })[x]);
  switch (cmd) {
    case "shot": {
      // Git-Bash style "/c/Users/..." would become "C:\c\Users\..." under Node on Windows.
      const file = (a[0] ?? "shot.png").replace(/^\/([a-zA-Z])\//, "$1:/");
      if (a[1]) await p.locator(a[1]).first().screenshot({ path: file });
      else await p.screenshot({ path: file });
      return `saved ${file}`;
    }
    case "click": {
      const kind = a.slice(1);
      const opts = { button: kind.includes("right") ? "right" : kind.includes("middle") ? "middle" : "left", modifiers: mods(kind), timeout: 4000 };
      if (kind.includes("double")) await p.locator(a[0]).first().dblclick(opts);
      else await p.locator(a[0]).first().click(opts);
      return "clicked";
    }
    case "clickText": {
      const kind = a.slice(1);
      const loc = p.getByText(a[0], { exact: false }).first();
      const opts = { button: kind.includes("right") ? "right" : "left", modifiers: mods(kind), timeout: 4000 };
      if (kind.includes("double")) await loc.dblclick(opts);
      else await loc.click(opts);
      return "clicked";
    }
    case "hover": await p.locator(a[0]).first().hover({ timeout: 4000 }); return "hovered";
    case "key": await p.keyboard.press(a[0]); return `pressed ${a[0]}`;
    case "type": await p.keyboard.type(a[0], { delay: 15 }); return "typed";
    case "mouse": {
      const [action, x, y] = [a[0], Number(a[1]), Number(a[2])];
      if (action === "move") await p.mouse.move(x, y, { steps: 5 });
      else if (action === "down") await p.mouse.down();
      else if (action === "up") await p.mouse.up();
      else if (action === "click") await p.mouse.click(x, y);
      else if (action === "right") await p.mouse.click(x, y, { button: "right" });
      return `mouse ${action}`;
    }
    case "drag": {
      const [x1, y1, x2, y2] = a.map(Number);
      await p.mouse.move(x1, y1);
      await p.mouse.down();
      await p.mouse.move(x2, y2, { steps: 20 });
      await p.mouse.up();
      return "dragged";
    }
    case "text": return (await p.locator(a[0] ?? "body").first().innerText({ timeout: 4000 })).slice(0, 20000);
    case "eval": return JSON.stringify(await p.evaluate(a[0]), null, 1)?.slice(0, 20000);
    case "wait": await p.waitForTimeout(Number(a[0] ?? 300)); return "waited";
    case "logs": {
      const msgs = [];
      p.on("console", (m) => msgs.push(`[${m.type()}] ${m.text()}`));
      p.on("pageerror", (e) => msgs.push(`[pageerror] ${e.message}`));
      await p.waitForTimeout(2000);
      return msgs.join("\n") || "(no console output in 2s)";
    }
    default: throw new Error(`unknown command: ${cmd}`);
  }
}

const p = await page();
const steps = process.argv.slice(2).join(" ").split(" ;; ").map((s) => s.trim()).filter(Boolean);
try {
  for (const s of steps) {
    const args = process.argv.length === 3 || steps.length > 1 ? splitArgs(s) : process.argv.slice(2);
    console.log(await run(p, args));
  }
  process.exit(0);
} catch (e) {
  console.error(`ERROR: ${e.message}`);
  process.exit(1);
}
