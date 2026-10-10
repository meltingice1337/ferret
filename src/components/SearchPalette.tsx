// Ctrl+K: instant whole-disk search. Ctrl+Shift+P (or ">" prefix): command palette.

import { invoke } from "@tauri-apps/api/core";
import { AppWindow, Command, Copy, ExternalLink, FolderOpen, FolderSearch, HardDrive, Hash, Info, LoaderCircle, Search, ShieldCheck, X } from "lucide-solid";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import FileIcon from "./FileIcon";
import "../palette.css";
import { formatDate, formatSize } from "../lib/format";
import { basename, parent } from "../lib/path";
import { activeTab, navigate, newTab, openMenu, recents, setUi, toast, ui } from "../lib/state";
import { api } from "../lib/api";
import { openFile } from "../lib/ops";

export interface Hit {
  path: string;
  name: string;
  isDir: boolean;
  size?: number | null;
  modified?: number | null;
  score: number;
  matches: number[];
}

interface SearchResults {
  results: Hit[];
  total: number;
  tookUs: number;
  source: "service" | "walk" | "none";
  partial: boolean;
}

interface SearchStatus {
  source: string;
  serviceInstalled: boolean;
  serviceRunning: boolean;
  serviceOutdated: boolean;
  volumes: { root: string; entries: number; ready: boolean; progress: number }[];
  building: boolean;
}

export interface PaletteCommand {
  label: string;
  hint?: string;
  keys?: string;
  run: () => void;
}

/** Highlights matched character positions (UTF-16 offsets) in a name. */
function Highlight(props: { text: string; matches: number[] }): JSX.Element {
  const parts = createMemo(() => {
    const set = new Set(props.matches);
    const out: { s: string; hit: boolean }[] = [];
    for (let i = 0; i < props.text.length; i++) {
      const hit = set.has(i);
      const last = out[out.length - 1];
      if (last && last.hit === hit) last.s += props.text[i];
      else out.push({ s: props.text[i], hit });
    }
    return out;
  });
  return <For each={parts()}>{(p) => (p.hit ? <mark>{p.s}</mark> : <>{p.s}</>)}</For>;
}

/** Simple subsequence scorer for the command list. */
function fuzzy(label: string, q: string): { score: number; matches: number[] } | null {
  if (!q) return { score: 0, matches: [] };
  const l = label.toLowerCase();
  const sub = l.indexOf(q);
  if (sub >= 0) return { score: 1000 - sub, matches: [...Array(q.length).keys()].map((k) => k + sub) };
  const matches: number[] = [];
  let i = 0;
  for (const ch of q) {
    i = l.indexOf(ch, i);
    if (i < 0) return null;
    matches.push(i++);
  }
  return { score: 500 - (matches[matches.length - 1] - matches[0]), matches };
}

export default function SearchPalette(props: { mode: "files" | "commands"; commands: PaletteCommand[] }) {
  const [text, setText] = createSignal(props.mode === "commands" ? ">" : "");
  const [scoped, setScoped] = createSignal(false);
  const [res, setRes] = createSignal<SearchResults | null>(null);
  const [status, setStatus] = createSignal<SearchStatus | null>(null);
  const [pick, setPick] = createSignal(0);
  const [busy, setBusy] = createSignal(false);
  let input!: HTMLInputElement;
  let list!: HTMLDivElement;

  const commandMode = () => text().startsWith(">");
  const query = () => (commandMode() ? text().slice(1).trim() : text().trim());
  const close = () => setUi("palette", null);

  // ---- file search: always run the latest query, one request at a time ----
  let inflight = false;
  let queued: string | null = null;
  const frecency = () => {
    const now = Date.now();
    const out: Record<string, number> = {};
    for (const r of recents()) out[r.path] = Math.min(1, r.count / 10) * 0.5 + Math.max(0, 1 - (now - r.lastOpened) / (14 * 86_400_000)) * 0.5;
    return out;
  };
  const run = async (q: string) => {
    if (inflight) { queued = q; return; }
    inflight = true;
    setBusy(true);
    try {
      const r = await invoke<SearchResults>("search_query", { query: q, limit: 60, scope: scoped() ? activeTab().p : null, frecency: frecency() });
      if (q === query() && !commandMode()) { setRes(r); setPick(0); }
    } catch (e) {
      toast(`Search failed: ${e}`, "error");
    } finally {
      inflight = false;
      setBusy(false);
      if (queued !== null) { const next = queued; queued = null; if (next === query()) run(next); }
    }
  };
  createEffect(on([query, scoped, commandMode], ([q, , cmd]) => {
    if (cmd) return;
    if (!q) { setRes(null); return; }
    run(q);
  }));

  // ---- index status (progress while building, service offer) ----
  onMount(() => {
    const poll = () => invoke<SearchStatus>("search_status").then(setStatus).catch(() => {});
    poll();
    const timer = setInterval(() => { if (status()?.building || !status()) poll(); }, 700);
    onCleanup(() => clearInterval(timer));
    queueMicrotask(() => { input.focus(); input.setSelectionRange(text().length, text().length); });
  });
  const indexed = () => status()?.volumes.reduce((a, v) => a + v.entries, 0) ?? 0;
  const progress = () => {
    const v = status()?.volumes ?? [];
    return v.length ? v.reduce((a, x) => a + x.progress, 0) / v.length : 0;
  };

  // ---- commands ----
  const commandHits = createMemo(() => {
    if (!commandMode()) return [];
    const q = query().toLowerCase();
    return props.commands
      .map((c) => ({ c, m: fuzzy(c.label, q) }))
      .filter((x) => x.m)
      .sort((a, b) => b.m!.score - a.m!.score)
      .slice(0, 40);
  });

  const count = () => (commandMode() ? commandHits().length : res()?.results.length ?? 0);

  const reveal = (h: Hit) => {
    const dir = h.isDir ? h.path : parent(h.path);
    if (dir) navigate(activeTab(), dir, h.isDir ? {} : { select: h.path });
  };
  const activate = (i: number, how: "open" | "tab" | "reveal") => {
    if (commandMode()) {
      const c = commandHits()[i]?.c;
      if (c) { close(); c.run(); }
      return;
    }
    const h = res()?.results[i];
    if (!h) return;
    close();
    if (how === "tab") newTab(h.isDir ? h.path : parent(h.path) ?? h.path);
    else if (how === "reveal" || h.isDir) reveal(h);
    else openFile({ path: h.path, inArchive: false });
  };

  createEffect(on(pick, (i) => {
    list?.querySelector<HTMLElement>(`[data-i="${i}"]`)?.scrollIntoView({ block: "nearest" });
  }));

  const onKey = (e: KeyboardEvent) => {
    e.stopPropagation();
    const n = count();
    if (e.key === "Escape") { e.preventDefault(); if (!ui.menu) close(); }
    else if (e.key === "ArrowDown") { e.preventDefault(); if (n) setPick((p) => (p + 1) % n); }
    else if (e.key === "ArrowUp") { e.preventDefault(); if (n) setPick((p) => (p - 1 + n) % n); }
    else if (e.key === "PageDown") { e.preventDefault(); setPick((p) => Math.min(n - 1, p + 8)); }
    else if (e.key === "PageUp") { e.preventDefault(); setPick((p) => Math.max(0, p - 8)); }
    else if (e.key === "Tab" && !commandMode()) { e.preventDefault(); setScoped(!scoped()); }
    else if (e.key === "Enter") {
      e.preventDefault();
      activate(pick(), e.ctrlKey ? "tab" : e.altKey || e.shiftKey ? "reveal" : "open");
    }
  };

  const resultMenu = (e: MouseEvent, i: number, h: Hit) => {
    e.preventDefault();
    setPick(i);
    const copy = (text: string, what: string) => navigator.clipboard.writeText(text).then(() => toast(`${what} copied`));
    openMenu(e.clientX, e.clientY, [
      { label: h.isDir ? "Open folder" : "Open", icon: ExternalLink, shortcut: "Enter", action: () => activate(i, "open") },
      { label: h.isDir ? "Show in parent folder" : "Show in folder", icon: FolderOpen, shortcut: "Alt+Enter", action: () => {
        close();
        const dir = parent(h.path);
        if (dir) navigate(activeTab(), dir, { select: h.path });
      } },
      { label: "Open in new tab", icon: FolderOpen, shortcut: "Ctrl+Enter", action: () => activate(i, "tab") },
      ...(h.isDir ? [] : [{ label: "Open with…", icon: AppWindow, action: () => { close(); api.openWithDialog(h.path).catch((err) => toast(String(err), "error")); } }]),
      { separator: true },
      { label: "Copy path", icon: Hash, action: () => copy(h.path, "Path") },
      { label: "Copy name", icon: Copy, action: () => copy(h.name, "Name") },
      { separator: true },
      { label: "Properties", icon: Info, action: () => { close(); setUi("properties", [h.path]); } },
    ]);
  };

  const installService = async () => {
    try {
      await invoke("search_install_service");
      toast("Installing the Ferret indexer: approve the Windows prompt");
      setTimeout(() => invoke<SearchStatus>("search_status").then(setStatus).catch(() => {}), 4000);
    } catch (e) {
      toast(`Couldn't install the indexer: ${e}`, "error");
    }
  };

  return (
    <Portal>
      <div class="palette-backdrop" onMouseDown={(e) => e.target === e.currentTarget && close()}>
        <div class="palette" role="dialog" aria-label={commandMode() ? "Command palette" : "Search"}>
          <div class="palette-input">
            <Show when={commandMode()} fallback={<Search size={17} />}><Command size={17} /></Show>
            <input
              ref={input}
              value={text()}
              placeholder={commandMode() ? "Type a command…" : scoped() ? `Search in ${basename(activeTab().p)}…` : "Search files and folders everywhere…"}
              spellcheck={false}
              onInput={(e) => setText(e.currentTarget.value)}
              onKeyDown={onKey}
            />
            <Show when={busy()}><LoaderCircle size={15} class="spin muted" /></Show>
            <Show when={!commandMode()}>
              <button class="palette-scope" classList={{ on: scoped() }} title="Toggle scope (Tab)" onClick={() => { setScoped(!scoped()); input.focus(); }}>
                {scoped() ? <><FolderSearch size={13} /> This folder</> : <><HardDrive size={13} /> Everywhere</>}
              </button>
            </Show>
            <button class="icon-btn tiny" title="Close (Esc)" onClick={close}><X size={14} /></button>
          </div>

          <div class="palette-list" ref={list}>
            <Show when={commandMode()} fallback={
              <Show when={res()?.results.length} fallback={
                <div class="palette-empty">
                  <Show when={query()} fallback={<span>Type to search {indexed() ? `${indexed().toLocaleString()} indexed items` : "your drives"}. Filters: <code>ext:pdf</code> <code>type:folder</code> <code>in:C:\dev</code> <code>size:&gt;100mb</code> <code>modified:&lt;7d</code> · <code>&gt;</code> for commands</span>}>
                    <span>{status()?.building ? "Still indexing, so more results will appear…" : `Nothing matches “${query()}”`}</span>
                  </Show>
                </div>
              }>
                <For each={res()!.results}>
                  {(h, i) => (
                    <div class="palette-item file" role="option" aria-selected={i() === pick()} classList={{ active: i() === pick() }} data-i={i()}
                      onMouseMove={() => setPick(i())}
                      onClick={(e) => activate(i(), e.ctrlKey ? "tab" : e.altKey ? "reveal" : "open")}
                      onAuxClick={(e) => e.button === 1 && activate(i(), "tab")}
                      onContextMenu={(e) => resultMenu(e, i(), h)}
                      title={h.path}>
                      <FileIcon entry={{ isDir: h.isDir, ext: h.isDir ? "" : (h.name.split(".").pop() ?? "").toLowerCase(), isSymlink: false }} size={16} />
                      <span class="palette-name"><Highlight text={h.name} matches={h.matches} /></span>
                      <span class="palette-path">{parent(h.path) ?? ""}</span>
                      <span class="palette-meta">{h.isDir ? "" : h.size != null ? formatSize(h.size) : ""}{h.modified ? ` · ${formatDate(h.modified).slice(0, 10)}` : ""}</span>
                      <button class="palette-reveal" title={h.isDir ? "Open folder" : "Show in folder (Alt+Enter)"}
                        onClick={(e) => { e.stopPropagation(); activate(i(), "reveal"); }}>
                        <FolderOpen size={14} /><span>{h.isDir ? "Open" : "Show in folder"}</span>
                      </button>
                    </div>
                  )}
                </For>
              </Show>
            }>
              <For each={commandHits()} fallback={<div class="palette-empty">No matching command</div>}>
                {(x, i) => (
                  <button class="palette-item cmd" classList={{ active: i() === pick() }} data-i={i()}
                    onMouseMove={() => setPick(i())} onClick={() => activate(i(), "open")}>
                    <Command size={14} class="muted" />
                    <span class="palette-name"><Highlight text={x.c.label} matches={x.m!.matches} /></span>
                    <span class="palette-path">{x.c.hint ?? ""}</span>
                    <Show when={x.c.keys}><kbd>{x.c.keys}</kbd></Show>
                  </button>
                )}
              </For>
            </Show>
          </div>

          <div class="palette-foot">
            <Show when={!commandMode()} fallback={<span>↑↓ choose · Enter run · Esc close</span>}>
              <span>
                <Show when={res()} fallback={<>↑↓ choose · Enter open · <b>Alt+Enter show in folder</b> · Ctrl+Enter new tab · right-click for more · Tab scope</>}>
                  {(r) => <>{r().total.toLocaleString()} matches · {(r().tookUs / 1000).toFixed(r().tookUs < 10_000 ? 1 : 0)} ms{r().partial ? " · indexing" : ""}</>}
                </Show>
              </span>
              <span class="palette-status">
                <Show when={status()?.building}>
                  <LoaderCircle size={12} class="spin" /> Indexing {Math.round(progress() * 100)}%
                </Show>
                <Show when={status() && !status()!.serviceInstalled && !status()!.building}>
                  <button class="palette-install" onClick={installService} title="Reads the NTFS file table for instant, always-current results across all drives. Needs one admin approval.">
                    <ShieldCheck size={12} /> Enable instant drive index
                  </button>
                </Show>
                <Show when={status()?.serviceRunning}>
                  <Show when={status()!.serviceOutdated} fallback={<><ShieldCheck size={12} /> Drive index</>}>
                    <button class="palette-install" onClick={installService} title="This version of Ferret ships a newer drive index service. Needs one admin approval.">
                      <ShieldCheck size={12} /> Update drive index
                    </button>
                  </Show>
                </Show>
              </span>
            </Show>
          </div>
        </div>
      </div>
    </Portal>
  );
}

export const paletteOpen = () => !!ui.palette;
