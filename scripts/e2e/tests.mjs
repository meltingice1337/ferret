// The end-to-end checks, in order. Each gets `t`: the page connection (`t.c`), the fixture
// folder (`t.root`, `t.p(...)`), and helpers. They use real key presses and clicks where a user
// would, and check results on disk where it matters. `soft` tests warn instead of failing
// (they depend on the machine, like the drive index).
import { existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { execSync } from "node:child_process";
import { crc32, deflateSync } from "node:zlib";
import { sleep } from "./cdp.mjs";

const T = "window.__ferretTest";
const tab = `${T}.state.activeTab()`;
const binCount = () => Number(execSync(`powershell -NoProfile -Command "@((New-Object -ComObject Shell.Application).Namespace(10).Items()).Count"`).toString().trim());
const assert = (ok, msg) => { if (!ok) throw new Error(msg); };

/** Clicks the list row named exactly `name` (selects it, like a user would). */
async function clickRow(t, name, mods = []) {
  const at = await t.c.eval(`(() => {
    const el = [...document.querySelectorAll(".files .row .name, .files .cell .cell-name")].find((e) => e.textContent === ${JSON.stringify(name)});
    if (!el) return null; el.scrollIntoView({ block: "nearest" });
    const b = el.getBoundingClientRect(); return { x: b.left + Math.min(40, b.width / 2), y: b.top + b.height / 2 };
  })()`);
  assert(at, `no row named “${name}”`);
  const modifiers = mods.reduce((a, m) => a | { Control: 2, Shift: 8 }[m], 0);
  await t.c.send("Input.dispatchMouseEvent", { type: "mousePressed", x: at.x, y: at.y, button: "left", clickCount: 1, modifiers });
  await t.c.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: at.x, y: at.y, button: "left", clickCount: 1, modifiers });
  await sleep(120);
}
const go = (t, path) => t.c.eval(`${T}.state.navigate(${tab}, ${JSON.stringify(path)})`);
const rows = (t) => t.c.eval(`[...document.querySelectorAll(".files .row .name")].map((e) => e.textContent)`);
/** Whether the preview shows the item named `name` (its path ends the details). */
const previewShows = (name) => `(document.querySelector(".pv-meta .pv-path")?.textContent ?? "").includes(${JSON.stringify("\\" + name)})`;
const modalText = (t) => t.c.eval(`document.querySelector(".modal")?.innerText ?? ""`);

export const tests = [
  {
    name: "opens the folder it was started with",
    async run(t) {
      await t.c.waitFor(`${T} && ${tab}?.p === ${JSON.stringify(t.root)}`, { what: "the fixture folder as the open tab" });
      await t.c.waitFor(`document.querySelectorAll(".files .row").length >= 5`, { what: "the list" });
      await t.c.waitFor(`!document.getElementById("splash")`, { what: "the splash to fade out" });
      const names = await rows(t);
      for (const n of ["docs", "sub", "bulk", "rename-me.txt"]) assert(names.includes(n), `“${n}” isn't listed (got ${names.join(", ")})`);
    },
  },
  {
    name: "the window appears once the app is drawn",
    async run(t) {
      // It starts hidden; the page shows it with the folder already in it (showApp in App.tsx).
      await t.c.waitFor(`!!window.__ferretStartup?.shown`, { what: "the page to show the window" });
      const s = await t.c.eval(`window.__ferretStartup`);
      assert(s.shown >= s.ready, `shown ${s.shown} ms after the page began loading, before the app was ready (${s.ready} ms)`);
      const visible = await t.c.eval(`window.__TAURI_INTERNALS__.invoke("plugin:window|is_visible", { label: "main" })`);
      assert(visible === true, `the window isn't visible (${visible})`);
    },
  },
  {
    name: "names the window after the folder",
    async run(t) {
      const title = await t.c.eval(`window.__TAURI_INTERNALS__.invoke("plugin:window|title", { label: "main" })`);
      assert(/^e2e-\S+ - Ferret/.test(title), `window title is “${title}”`);
    },
  },
  {
    name: "Enter opens a folder, Alt+← goes back",
    async run(t) {
      await clickRow(t, "sub");
      await t.c.key("Enter");
      await t.c.waitFor(`${tab}.p.endsWith("\\\\sub")`, { what: "inside sub" });
      await t.c.waitFor(`[...document.querySelectorAll(".files .row .name")].some((e) => e.textContent === "inner.txt")`, { what: "inner.txt listed" });
      await t.c.key("ArrowLeft", ["Alt"]);
      await t.c.waitFor(`${tab}.p === ${JSON.stringify(t.root)}`, { what: "back in the fixture folder" });
    },
  },
  {
    name: "previews text, Markdown and pictures",
    async run(t) {
      await t.c.eval(`${T}.state.setUi("previewOpen", true)`);
      await go(t, t.p("docs"));
      await clickRow(t, "notes.txt");
      await t.c.waitFor(`document.querySelector(".pv-body")?.innerText.includes("hello from e2e")`, { what: "the text in the preview" });
      await clickRow(t, "readme.md");
      await t.c.waitFor(`document.querySelector(".pv-body h1")?.textContent.includes("E2E title")`, { what: "the rendered Markdown heading" });
      await clickRow(t, "photo.png");
      await t.c.waitFor(`(() => { const i = document.querySelector(".pv-body img"); return i && i.complete && i.naturalWidth > 0; })()`, { what: "the picture decoded" });
    },
  },
  {
    name: "a video's details show before it plays",
    async run(t) {
      await t.c.eval(`${T}.state.setUi("previewOpen", true)`);
      await go(t, t.p("media"));
      await clickRow(t, "clip.mp4");
      await t.c.waitFor(`document.querySelector(".pv-poster")`, { what: "the video's poster" });
      const details = `Object.fromEntries([...document.querySelectorAll(".pv-meta dt")].map((dt) => [dt.textContent, dt.nextElementSibling?.textContent ?? ""]))`;
      await t.c.waitFor(`!!${details}["Duration"]?.trim()`, { what: "the video's length, without playing it" });
      const d = await t.c.eval(details);
      assert(d.Duration === "0:01", `Duration ${d.Duration}`);
      assert(d.Dimensions === "160 × 90", `Dimensions ${d.Dimensions}`);
      assert(d["Frame rate"] === "25 fps", `Frame rate ${d["Frame rate"]}`);
      assert(d.Codec === "H.264", `Codec ${d.Codec}`);
      assert(d.Audio && d.Audio !== "None", `Audio ${d.Audio}`);
      assert(!(await t.c.eval(`!!document.querySelector(".pv-body video")`)), "the video started playing to get them");
    },
  },
  {
    name: "code is highlighted in the preview and in Quick Look",
    async run(t) {
      await t.c.eval(`${T}.state.setUi("previewOpen", true)`);
      await go(t, t.p("code"));
      // A language not loaded yet: the code must appear colored, never plain first.
      await t.c.eval(`(() => {
        const seen = window.__e2ePlain = { plain: false };
        window.__e2ePlainObs = new MutationObserver(() => { if (document.querySelector(".pv-body pre.plain")) seen.plain = true; });
        window.__e2ePlainObs.observe(document.body, { subtree: true, childList: true });
      })()`);
      await clickRow(t, "lines.rs");
      // Colored tokens: Shiki's spans with their own colors (plain text has none).
      const colored = (sel) => `(() => { const s = [...document.querySelectorAll(${JSON.stringify(sel)} + " pre.shiki:not(.plain) span[style*='color']")]; return new Set(s.map((e) => e.style.color)).size; })()`;
      await t.c.waitFor(`${colored(".pv-body")} >= 3`, { timeoutMs: 10000, what: "colored code in the preview" });
      const flashed = await t.c.eval(`(() => { window.__e2ePlainObs.disconnect(); return window.__e2ePlain.plain; })()`);
      assert(!flashed, "the code showed plain for a moment before its colors");
      // Selecting (and so copying) all of it gives the file's lines, without the line numbers.
      const selected = await t.c.eval(`(() => {
        const r = document.createRange(); r.selectNodeContents(document.querySelector(".pv-body pre.shiki code"));
        const s = getSelection(); s.removeAllRanges(); s.addRange(r);
        const text = s.toString(); s.removeAllRanges(); return text;
      })()`);
      assert(selected === "fn main() {\n\n    let x = 1; // one\n}", `selected text: ${JSON.stringify(selected)}`);
      await t.c.key(" ");
      await t.c.waitFor(`${colored(".ql")} >= 3`, { timeoutMs: 10000, what: "colored code in Quick Look" });
      await t.c.key("Escape");
      await t.c.waitFor(`!document.querySelector(".ql")`, { what: "Quick Look to close" });
    },
  },
  {
    name: "details show only the shown item's rows (zip, zip, then an exe)",
    async run(t) {
      await t.c.eval(`${T}.state.setUi("previewOpen", true)`);
      await go(t, t.p("kinds"));
      await t.c.waitFor(`[...document.querySelectorAll(".files .row .name")].some((e) => e.textContent === "one.zip")`, { what: "kinds listed" });
      const details = `[...document.querySelectorAll(".pv-meta dt")].map((dt) => [dt.textContent, dt.nextElementSibling.textContent.trim(), dt.nextElementSibling.classList.contains("pv-wait")])`;
      const contents = (want) => `${details}.some(([l, v, wait]) => l === "Contents" && v === ${JSON.stringify(want)} && !wait)`;
      await clickRow(t, "one.zip");
      await t.c.waitFor(contents("1 file, 0 folders"), { what: "the first zip's contents" });
      // Another zip: the same rows, filled in with its own values.
      await clickRow(t, "two.zip");
      await t.c.waitFor(contents("2 files, 0 folders"), { what: "the second zip's contents" });
      // An exe reports none of those rows: they go as soon as it's shown, not after a delay.
      await clickRow(t, "app.exe");
      await t.c.waitFor(previewShows("app.exe"), { what: "the exe in the preview" });
      const rowsNow = await t.c.eval(details);
      const stale = rowsNow.filter(([l]) => ["Format", "Contents", "Unpacked", "Saved"].includes(l));
      assert(!stale.length, `the zip's rows are still shown for the exe: ${JSON.stringify(rowsNow)}`);
    },
  },
  {
    name: "clicking the path in the details copies it and says so",
    async run(t) {
      await t.c.eval(`${T}.state.setUi("previewOpen", true)`);
      await go(t, t.p("docs"));
      await clickRow(t, "notes.txt");
      await t.c.waitFor(previewShows("notes.txt"), { what: "notes.txt in the preview" });
      const at = await t.c.eval(`(() => { const b = document.querySelector(".pv-meta .pv-path").getBoundingClientRect(); return { x: b.left + 30, y: b.top + 8 }; })()`);
      await t.c.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: at.x, y: at.y });
      await sleep(250);
      const icon = await t.c.eval(`getComputedStyle(document.querySelector(".pv-copy-ico")).opacity`);
      assert(icon === "1", `no copy icon on hover (opacity ${icon})`);
      await t.c.send("Input.dispatchMouseEvent", { type: "mousePressed", x: at.x, y: at.y, button: "left", clickCount: 1 });
      await t.c.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: at.x, y: at.y, button: "left", clickCount: 1 });
      await t.c.waitFor(`document.querySelector(".pv-copied")?.textContent === "Copied"`, { what: "“Copied” by the pointer" });
      const copied = execSync(`powershell -NoProfile -Command "Get-Clipboard"`).toString().trim();
      assert(copied === t.p("docs", "notes.txt"), `the clipboard has “${copied}”`);
      await t.c.waitFor(`!document.querySelector(".pv-copied")`, { timeoutMs: 3000, what: "“Copied” to go away" });
    },
  },
  {
    name: "F5 and Ctrl+R refresh the folder (and never reload the window)",
    async run(t) {
      await go(t, t.p("docs"));
      await t.c.waitFor(`[...document.querySelectorAll(".files .row .name")].some((e) => e.textContent === "notes.txt")`, { what: "docs listed" });
      // Count folder listings, and see whether each key reached the browser (a reload) or was
      // taken by Ferret: synthetic key presses can't trigger the browser's own reload here, so
      // that's checked through the event instead.
      await t.c.eval(`(() => {
        const api = ${T}.api; const m = window.__e2eF5 = { lists: 0, prevented: [] };
        const real = api.listDir; window.__e2eRealList = real;
        api.listDir = (...a) => { m.lists++; return real(...a); };
        window.addEventListener("keydown", (e) => { if (e.key === "F5" || (e.ctrlKey && e.key === "r")) m.prevented.push(e.defaultPrevented); }, true);
        window.__e2eMark = 1;
      })()`);
      await t.c.key("F5");
      await t.c.waitFor(`window.__e2eF5.lists >= 1`, { what: "F5 to list the folder again" });
      await t.c.key("r", ["Control"]);
      await t.c.waitFor(`window.__e2eF5.lists >= 2`, { what: "Ctrl+R to list the folder again" });
      const m = await t.c.eval(`(() => { ${T}.api.listDir = window.__e2eRealList; return { ...window.__e2eF5, mark: window.__e2eMark }; })()`);
      assert(m.prevented.length === 2 && m.prevented.every(Boolean), `a key reached the browser (it would reload the window): ${JSON.stringify(m.prevented)}`);
      assert(m.mark === 1, "the window reloaded");
    },
  },
  {
    name: "a folder rewritten by a build stays open and keeps up",
    async run(t) {
      // A cargo build replaces its outputs and churns its subfolders while you look at the
      // folder. Ferret once took that for the folder being deleted, moved up, and its list
      // and preview stopped updating.
      const dir = t.p("build-out");
      const errorsBefore = t.c.errors.length;
      const previewWasOpen = await t.c.eval(`${T}.state.ui.previewOpen`);
      await t.c.eval(`${T}.state.setUi("previewOpen", true)`);
      await go(t, dir);
      await t.c.waitFor(`[...document.querySelectorAll(".files .row .name")].some((e) => e.textContent === "tool.exe")`, { what: "build-out listed" });
      await clickRow(t, "tool.exe");
      await t.c.waitFor(previewShows("tool.exe"), { what: "tool.exe in the preview" });
      const end = Date.now() + 4000;
      for (let i = 0; Date.now() < end; i++) {
        writeFileSync(join(dir, "deps", `lib${i % 40}.rlib`), "x".repeat(i % 400));
        if (i % 3 === 0) rmSync(join(dir, "deps", `lib${(i + 20) % 40}.rlib`), { force: true });
        // Outputs replaced in one step (written beside, renamed over), or deleted and written again.
        writeFileSync(join(dir, "tool.pdb.tmp"), `pdb ${i}`);
        renameSync(join(dir, "tool.pdb.tmp"), join(dir, "tool.pdb"));
        if (i % 5 === 0) { rmSync(join(dir, "tool.d"), { force: true }); writeFileSync(join(dir, "tool.d"), `tool.d ${i}`); }
        if (i % 7 === 0) { rmSync(join(dir, "helper.exe"), { force: true }); writeFileSync(join(dir, "helper.exe"), Buffer.concat([Buffer.from("MZ"), Buffer.alloc(510 + i)])); }
        // The file in the preview is relinked too: for a moment the list has the new one
        // while the preview still shows the old one (they used to overwrite each other's
        // version forever, until Solid gave up and the UI stopped updating).
        if (i % 40 === 20) { rmSync(join(dir, "tool.exe"), { force: true }); writeFileSync(join(dir, "tool.exe"), Buffer.concat([Buffer.from("MZ"), Buffer.alloc(510 + i)])); }
        mkdirSync(join(dir, ".fingerprint", `tool-${i % 6}`), { recursive: true });
        await sleep(15);
      }
      writeFileSync(join(dir, "done.txt"), "built");
      await t.c.waitFor(`[...document.querySelectorAll(".files .row .name")].some((e) => e.textContent === "done.txt")`, { what: "the list to keep up with the build" });
      const s = await t.c.eval(`({ path: ${tab}.p, toasts: ${T}.state.ui.toasts.map((x) => x.text) })`);
      assert(s.path.toLowerCase() === dir.toLowerCase(), `the tab moved to ${s.path}`);
      assert(!s.toasts.some((x) => /no longer available/.test(x)), `said the folder was gone: ${s.toasts.join(" | ")}`);
      // Still in step: another folder replaces the list and the preview.
      await go(t, t.root);
      await t.c.waitFor(`[...document.querySelectorAll(".files .row .name")].some((e) => e.textContent === "build-out")`, { what: "the list to follow the next folder" });
      await t.c.waitFor(`!(${previewShows("tool.exe")})`, { what: "the preview to let go of tool.exe" });
      await t.c.eval(`${T}.state.setUi("previewOpen", ${!!previewWasOpen})`);
      const errs = t.c.errors.slice(errorsBefore);
      assert(!errs.length, `${errs.length} UI error(s) during the build: ${errs.slice(0, 3).join(" || ")}`);
    },
  },
  {
    name: "creates a folder (Ctrl+Shift+N) and names it",
    async run(t) {
      await go(t, t.p("docs"));
      await t.c.eval(`document.querySelector(".files").focus()`);
      await t.c.key("N", ["Control", "Shift"]);
      await t.c.waitFor(`document.querySelector("textarea.rename")`, { what: "the name box" });
      await t.c.eval(`(() => { const r = document.querySelector("textarea.rename"); r.focus(); r.select(); })()`);
      await t.c.type("made-by-e2e");
      await t.c.key("Enter");
      await t.c.waitFor(`!document.querySelector("textarea.rename")`, { what: "the name box to close" });
      await sleep(400);
      assert(existsSync(t.p("docs", "made-by-e2e")), "the folder wasn't created on disk");
    },
  },
  {
    name: "asks before a rename changes the extension, then renames",
    async run(t) {
      await go(t, t.root);
      await clickRow(t, "rename-me.txt");
      await t.c.key("F2");
      await t.c.waitFor(`document.querySelector("textarea.rename")`, { what: "the name box" });
      await t.c.eval(`document.querySelector("textarea.rename").select()`);
      await t.c.type("rename-me.md");
      await t.c.key("Enter");
      await t.c.waitFor(`document.querySelector(".modal")?.innerText.includes("Change the file extension?")`, { what: "the extension question" });
      await t.c.click(".modal button", { text: "Use .md" });
      await t.c.waitFor(`!document.querySelector(".modal")`, { what: "the dialog to close" });
      await sleep(500);
      assert(existsSync(t.p("rename-me.md")) && !existsSync(t.p("rename-me.txt")), "not renamed on disk");
    },
  },
  {
    name: "copies and pastes (Ctrl+C, Ctrl+V)",
    async run(t) {
      await go(t, t.p("docs"));
      const before = readdirSync(t.p("docs")).length;
      await clickRow(t, "notes.txt");
      await t.c.key("c", ["Control"]);
      await t.c.key("v", ["Control"]);
      const start = Date.now();
      while (readdirSync(t.p("docs")).length === before && Date.now() - start < 8000) await sleep(150);
      const added = readdirSync(t.p("docs")).filter((n) => /^notes.+\.txt$/.test(n) && n !== "notes.txt");
      assert(added.length === 1, `expected one copy of notes.txt, found: ${readdirSync(t.p("docs")).join(", ")}`);
    },
  },
  {
    name: "split view: two folders, Tab switches, copy to the other pane",
    async run(t) {
      const paneNames = `[...document.querySelectorAll(".pane-name")].map((e) => e.textContent)`;
      const inUse = `[...document.querySelectorAll(".pane")].findIndex((e) => e.classList.contains("in-use"))`;
      const tabCount = await t.c.eval(`${T}.state.tabs().length`);
      await go(t, t.root);
      await t.c.waitFor(`[...document.querySelectorAll(".files .row .name")].some((e) => e.textContent === "split-me.txt")`, { what: "the fixture folder" });
      await t.c.eval(`${T}.state.fileList()?.focus()`);
      // Ctrl+\ opens a second pane on the same folder, and it gets the keyboard.
      await t.c.key("\\", ["Control"]);
      await t.c.waitFor(`document.querySelectorAll(".pane").length === 2 && ${inUse} === 1`, { what: "a second pane, in use" });
      // The second pane goes elsewhere; the first stays.
      await go(t, t.p("docs"));
      await t.c.waitFor(`${paneNames}[1] === "docs"`, { what: "the second pane in docs" });
      assert((await t.c.eval(paneNames))[0] !== "docs", "the first pane moved too");
      // Tab gives the keyboard (and the toolbar) to the first pane.
      await t.c.key("Tab");
      await t.c.waitFor(`${inUse} === 0 && ${tab}.p === ${JSON.stringify(t.root)}`, { what: "the first pane in use" });
      // Copy a file across with the command palette's "Copy to the other pane".
      await clickRow(t, "split-me.txt");
      await t.c.key("p", ["Control", "Shift"]);
      await t.c.waitFor(`document.querySelector(".palette input")`, { what: "the command palette" });
      await t.c.type("copy to the other pane");
      await t.c.waitFor(`document.querySelector(".palette-item.cmd.active")?.textContent.includes("Copy to the other pane")`, { what: "the command" });
      await t.c.key("Enter");
      const start = Date.now();
      while (!existsSync(t.p("docs", "split-me.txt")) && Date.now() - start < 8000) await sleep(150);
      assert(existsSync(t.p("docs", "split-me.txt")), "the file wasn't copied into the other pane's folder");
      assert(existsSync(t.p("split-me.txt")), "the original is gone (it should have been copied)");
      await t.c.waitFor(`[...document.querySelectorAll(".pane:nth-child(2) .files .row .name")].some((e) => e.textContent === "split-me.txt")`, { what: "the copy listed in the other pane" });
      // Ctrl+W in the second pane closes that pane, not the tab.
      await t.c.key("Tab");
      await t.c.waitFor(`${inUse} === 1`, { what: "the second pane in use again" });
      await t.c.key("w", ["Control"]);
      await t.c.waitFor(`document.querySelectorAll(".pane").length === 1`, { what: "split view closed" });
      assert((await t.c.eval(`${T}.state.tabs().length`)) === tabCount, "Ctrl+W closed the tab instead of the pane");
    },
  },
  {
    name: "middle-click autoscroll: hold and move, or click and follow the mouse",
    async run(t) {
      const mouse = (type, x, y, extra = {}) => t.c.send("Input.dispatchMouseEvent", { type, x, y, ...extra });
      const top = () => t.c.eval(`${T}.state.fileList().scrollTop`);
      const marker = () => t.c.eval(`!!document.querySelector(".autoscroll-mark")`);
      await go(t, t.p("bulk"));
      await t.c.waitFor(`document.querySelectorAll(".files .row").length > 10`, { what: "the bulk folder" });
      const at = await t.c.eval(`(() => { const b = ${T}.state.fileList().getBoundingClientRect(); return { x: b.left + 120, y: b.top + 80 }; })()`);
      // Hold the wheel and move down: it scrolls, and stops once the wheel is released.
      await mouse("mousePressed", at.x, at.y, { button: "middle", buttons: 4, clickCount: 1 });
      assert(await marker(), "no autoscroll marker after pressing the wheel");
      await mouse("mouseMoved", at.x, at.y + 160, { buttons: 4 });
      await sleep(500);
      await mouse("mouseReleased", at.x, at.y + 160, { button: "middle", buttons: 0, clickCount: 1 });
      const held = await top();
      assert(held > 150, `holding and moving scrolled only ${held}px`);
      assert(!(await marker()), "the marker stayed after releasing the wheel");
      await sleep(250);
      assert((await top()) === held, "it kept scrolling after the wheel was released");
      // A click of the wheel: it follows the mouse until the next click, which does nothing else.
      await t.c.eval(`${T}.state.fileList().scrollTop = 0`);
      const selected = await t.c.eval(`${tab}.selection[0]().join()`);
      await mouse("mousePressed", at.x, at.y, { button: "middle", buttons: 4, clickCount: 1 });
      await mouse("mouseReleased", at.x, at.y, { button: "middle", buttons: 0, clickCount: 1 });
      await mouse("mouseMoved", at.x, at.y + 160, { buttons: 0 });
      await sleep(500);
      const followed = await top();
      assert(followed > 150, `after a click of the wheel it scrolled only ${followed}px`);
      await mouse("mousePressed", at.x, at.y + 160, { button: "left", buttons: 1, clickCount: 1 });
      await mouse("mouseReleased", at.x, at.y + 160, { button: "left", buttons: 0, clickCount: 1 });
      assert(!(await marker()), "a click didn't stop autoscroll");
      assert((await t.c.eval(`${tab}.selection[0]().join()`)) === selected, "the click that stopped autoscroll also selected a file");
      // A folder still opens in a new tab on a middle-click.
      await go(t, t.root);
      await t.c.waitFor(`[...document.querySelectorAll(".files .row .name")].some((e) => e.textContent === "docs")`, { what: "the fixture folder" });
      const tabs = await t.c.eval(`${T}.state.tabs().length`);
      const docs = await t.c.eval(`(() => { const el = [...document.querySelectorAll(".files .row .name")].find((e) => e.textContent === "docs"); const b = el.getBoundingClientRect(); return { x: b.left + 20, y: b.top + b.height / 2 }; })()`);
      await mouse("mousePressed", docs.x, docs.y, { button: "middle", buttons: 4, clickCount: 1 });
      await mouse("mouseReleased", docs.x, docs.y, { button: "middle", buttons: 0, clickCount: 1 });
      await t.c.waitFor(`${T}.state.tabs().length === ${tabs + 1}`, { what: "docs in a new tab" });
      assert(!(await marker()), "a middle-click on a folder started autoscroll");
      await t.c.eval(`${T}.state.closeTab(${T}.state.tabs()[${tabs}].id)`);
    },
  },
  {
    name: "Shift+Delete deletes for good and leaves the Recycle Bin alone",
    async run(t) {
      await go(t, t.root);
      const bin = binCount();
      await clickRow(t, "delete-me.txt");
      await t.c.key("Delete", ["Shift"]);
      await t.c.waitFor(`document.querySelector(".modal")?.innerText.includes("Permanently delete")`, { what: "the confirmation" });
      await t.c.click(".modal button", { text: "Delete permanently" });
      const start = Date.now();
      while (existsSync(t.p("delete-me.txt")) && Date.now() - start < 8000) await sleep(150);
      assert(!existsSync(t.p("delete-me.txt")), "still on disk");
      assert(binCount() === bin, "the Recycle Bin changed");
    },
  },
  {
    name: "Delete recycles and Ctrl+Z brings it back",
    async run(t) {
      const bin = binCount();
      await clickRow(t, "recycle-me.txt");
      await t.c.key("Delete");
      let start = Date.now();
      while (existsSync(t.p("recycle-me.txt")) && Date.now() - start < 8000) await sleep(150);
      assert(!existsSync(t.p("recycle-me.txt")), "not recycled");
      assert(binCount() === bin + 1, "not in the Recycle Bin");
      await t.c.eval(`document.querySelector(".files").focus()`);
      await t.c.key("z", ["Control"]);
      start = Date.now();
      while (!existsSync(t.p("recycle-me.txt")) && Date.now() - start < 10000) await sleep(150);
      assert(existsSync(t.p("recycle-me.txt")), "undo didn't restore it");
      assert(binCount() === bin, "the Recycle Bin still holds it");
    },
  },
  {
    name: "stays responsive while deleting 300 files",
    async run(t) {
      await go(t, t.p("bulk"));
      await t.c.waitFor(`${tab}.entries[0]().length === 300`, { what: "300 files listed" });
      await clickRow(t, "file1.txt");
      await t.c.key("a", ["Control"]);
      await t.c.eval(`(() => { const m = window.__e2ePing = { worst: 0, run: true }; (async () => { while (m.run) { const s = performance.now(); await ${T}.api.windowInfo(); m.worst = Math.max(m.worst, performance.now() - s); await new Promise((r) => setTimeout(r, 50)); } })(); })()`);
      await t.c.key("Delete", ["Shift"]);
      await t.c.waitFor(`document.querySelector(".modal")?.innerText.includes("Permanently delete")`, { what: "the confirmation" });
      await t.c.click(".modal button", { text: "Delete permanently" });
      const start = Date.now();
      while (readdirSync(t.p("bulk")).length && Date.now() - start < 20000) await sleep(200);
      const worst = await t.c.eval(`(window.__e2ePing.run = false, Math.round(window.__e2ePing.worst))`);
      assert(!readdirSync(t.p("bulk")).length, `${readdirSync(t.p("bulk")).length} files left`);
      assert(worst < 1500, `the window didn't answer for ${worst} ms`);
      t.note(`slowest answer ${worst} ms`);
    },
  },
  {
    name: "finds a new file with Ctrl+K",
    soft: true,
    async run(t) {
      await t.c.key("k", ["Control"]);
      await t.c.waitFor(`document.querySelector(".palette input")`, { what: "the search box" });
      await t.c.type(t.needle);
      try {
        await t.c.waitFor(`[...document.querySelectorAll(".palette-item.file")].some((e) => e.innerText.includes(${JSON.stringify(t.needle)}))`, { timeoutMs: 20000, what: "the new file in the results" });
      } finally {
        await t.c.key("Escape");
      }
    },
  },
  {
    name: "a search tab lists its matches",
    soft: true,
    async run(t) {
      await t.c.eval(`${T}.state.newTab("search:" + ${JSON.stringify(t.needle)})`);
      await t.c.waitFor(`${tab}.entries[0]().some((e) => e.name.startsWith(${JSON.stringify(t.needle)}))`, { timeoutMs: 15000, what: "the match in the search tab" });
      await t.c.eval(`${T}.state.closeTab(${tab}.id)`);
    },
  },
  {
    name: "Ctrl+K content: finds text inside the open folder's files",
    async run(t) {
      await go(t, t.p("grep"));
      await t.c.waitFor(`[...document.querySelectorAll(".files .row .name")].some((e) => e.textContent === "notes.md")`, { what: "grep listed" });
      await t.c.eval(`document.querySelector(".files")?.focus()`);
      await t.c.key("k", ["Control"]);
      await t.c.waitFor(`document.querySelector(".palette input")`, { what: "the search box" });
      await t.c.type('content:"zebra-quokka"');
      const found = `[...document.querySelectorAll(".palette-item.has-line")].map((e) => [e.querySelector(".palette-name").textContent, e.querySelector(".palette-line-no").textContent, e.querySelector(".palette-line-text mark").textContent, e.querySelector(".palette-line-more")?.textContent ?? ""])`;
      try {
        await t.c.waitFor(`${found}.length === 2`, { what: "the two files with the text" });
        const rows = await t.c.eval(found);
        // Folder order; the match keeps the file's own case; binary files and node_modules are skipped.
        assert(JSON.stringify(rows) === JSON.stringify([["code.ts", "2", "ZEBRA-QUOKKA", "+1"], ["notes.md", "2", "zebra-quokka", ""]]), `results: ${JSON.stringify(rows)}`);
        const foot = await t.c.eval(`document.querySelector(".palette-foot").innerText`);
        assert(/2 files have it/.test(foot), `footer: ${foot}`);
        // Every match as a tab: the search keeps the folder it read.
        await t.c.key("Enter", ["Control", "Shift"]);
        await t.c.waitFor(`${tab}.p.startsWith("search:") && ${tab}.entries[0]().length === 2`, { what: "the matches in a search tab" });
        const q = await t.c.eval(`${tab}.p`);
        assert(q.includes("in:") && q.includes("grep"), `the tab's search is “${q}”`);
        await t.c.eval(`${T}.state.closeTab(${tab}.id)`);
      } finally {
        if (await t.c.eval(`!!document.querySelector(".palette")`)) await t.c.key("Escape");
      }
    },
  },
  {
    name: "Settings open (Ctrl+,) and close (Esc)",
    async run(t) {
      await t.c.eval(`document.querySelector(".files")?.focus()`);
      await t.c.key(",", ["Control"]);
      await t.c.waitFor(`document.querySelector(".modal")`, { what: "the settings dialog" });
      await t.c.key("Escape");
      await t.c.waitFor(`!document.querySelector(".modal")`, { what: "the dialog to close" });
    },
  },
  {
    name: "no errors in the UI during the run",
    async run(t) {
      const errs = t.c.errors.filter((e) => !/favicon/.test(e));
      assert(!errs.length, `${errs.length} error(s): ${errs.slice(0, 3).join(" || ")}`);
      const csp = await t.c.eval(`${T}.csp`);
      assert(!csp.length, `the security policy (CSP) refused ${csp.length} thing(s): ${csp.slice(0, 3).join(" || ")}`);
    },
  },
];

/** The files the tests expect. */
export function fixture(root, needle) {
  return {
    "docs/notes.txt": "hello from e2e\nsecond line",
    "docs/readme.md": "# E2E title\n\nSome *Markdown*.",
    "docs/photo.png": png(24, 16, [220, 40, 40]),
    // 1 s, 160 × 90, 25 fps, H.264 with an AAC tone (made with ffmpeg).
    "media/clip.mp4": readFileSync(new URL("./fixtures/clip.mp4", import.meta.url)),
    "kinds/one.zip": zip({ "a.txt": "a" }),
    "kinds/two.zip": zip({ "a.txt": "a", "b.txt": "b" }),
    // Never run: only its extension matters (an application, which has no detail rows).
    "kinds/app.exe": Buffer.concat([Buffer.from("MZ"), Buffer.alloc(510)]),
    // Searching inside files: two have the text, a binary file and node_modules are skipped.
    "grep/notes.md": "first line\nThe phrase zebra-quokka lives here\n",
    "grep/deep/code.ts": "const a = 1;\n// ZEBRA-QUOKKA, then zebra-quokka again\n",
    "grep/blob.bin": Buffer.concat([Buffer.from("zebra-quokka"), Buffer.alloc(16)]),
    "grep/node_modules/pkg/x.js": "zebra-quokka",
    "grep/other.txt": "nothing here",
    // Highlighting, and selecting code with an empty line in it.
    "code/lines.rs": "fn main() {\n\n    let x = 1; // one\n}\n",
    // Rewritten the way a build rewrites its output folder (cargo's target\release).
    "build-out/tool.exe": Buffer.concat([Buffer.from("MZ"), Buffer.alloc(510)]),
    "build-out/tool.pdb": "pdb",
    "build-out/tool.d": "tool.d",
    ...Object.fromEntries(Array.from({ length: 40 }, (_, i) => [`build-out/deps/lib${i}.rlib`, "x"])),
    "build-out/.fingerprint/tool-1/output": "x",
    "sub/inner.txt": "inside",
    "rename-me.txt": "rename me",
    "split-me.txt": "copied across in split view",
    "delete-me.txt": "delete me",
    "recycle-me.txt": "recycle me",
    [`${needle}.txt`]: "find me",
    ...Object.fromEntries(Array.from({ length: 300 }, (_, i) => [`bulk/file${i + 1}.txt`, "x"])),
  };
}

/** A zip of the given files, stored uncompressed. */
function zip(files) {
  const locals = [], central = [];
  let offset = 0;
  for (const [name, text] of Object.entries(files)) {
    const n = Buffer.from(name), data = Buffer.from(text), crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(0x21, 12);
    local.writeUInt32LE(crc, 14); local.writeUInt32LE(data.length, 18); local.writeUInt32LE(data.length, 22); local.writeUInt16LE(n.length, 26);
    const dir = Buffer.alloc(46);
    dir.writeUInt32LE(0x02014b50, 0); dir.writeUInt16LE(20, 4); dir.writeUInt16LE(20, 6); dir.writeUInt16LE(0x21, 14);
    dir.writeUInt32LE(crc, 16); dir.writeUInt32LE(data.length, 20); dir.writeUInt32LE(data.length, 24); dir.writeUInt16LE(n.length, 28); dir.writeUInt32LE(offset, 42);
    locals.push(local, n, data); central.push(dir, n);
    offset += local.length + n.length + data.length;
  }
  const cd = Buffer.concat(central), end = Buffer.alloc(22);
  const count = Object.keys(files).length;
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(count, 8); end.writeUInt16LE(count, 10); end.writeUInt32LE(cd.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, cd, end]);
}

/** A solid-color PNG (for the picture preview). */
function png(w, h, [r, g, b]) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td));
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2;
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array.from({ length: w }, () => [r, g, b]).flat())]);
  const raw = Buffer.concat(Array.from({ length: h }, () => row));
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}
