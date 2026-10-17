import { Check, ChevronLeft, ChevronRight, Copy, ExternalLink, Maximize2, X } from "../lib/icons";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { formatDate, formatSize, kindOf, typeLabel } from "../lib/format";
import { openFile } from "../lib/ops";
import { basename, isRecycle, pathKey } from "../lib/path";
import { fileList, activeTab, openEntry, persistSession, previewSpace, setUi, ui } from "../lib/state";
import type { Entry } from "../lib/types";
import { Thumb } from "./FileView";
import Preview from "./preview/Preview";
import { enter } from "../lib/motion";
import { imageUrl, type Meta } from "./preview/renderers";

/** The entry the user is looking at: the cursor if selected, or the single selection. */
function useFocusedEntry(delay: number) {
  const [entry, setEntry] = createSignal<Entry | null>(null);
  const raw = createMemo(() => {
    const t = activeTab();
    if (!t) return null;
    const sel = t.selection[0]();
    const cur = t.cursor[0]();
    const pick = cur && sel.some((p) => pathKey(p) === pathKey(cur)) ? cur : sel.length === 1 ? sel[0] : null;
    return pick ? t.entries[0]().find((e) => pathKey(e.path) === pathKey(pick)) ?? null : null;
  });
  let timer: number | undefined;
  createEffect(on(raw, (e) => {
    clearTimeout(timer);
    // Debounce so holding an arrow key doesn't load every file on the way.
    timer = window.setTimeout(() => setEntry(e), entry() ? delay : 0);
  }));
  onCleanup(() => clearTimeout(timer));
  return entry;
}

/** "66 lines", "1920 × 1080", "Duration 3:47" — readable on its own. */
function metaText(m: Meta) {
  if (m.label === "Dimensions" || m.label === "Duration") return m.value;
  if (/^[\d,.+]+$/.test(m.value)) return `${m.value} ${m.value === "1" ? m.label.toLowerCase().replace(/s$/, "") : m.label.toLowerCase()}`;
  return `${m.label}: ${m.value}`;
}

function folderEntry(path: string): Entry {
  return { name: isRecycle(path) ? "Recycle Bin" : basename(path), path, isDir: true, isSymlink: false, hidden: false, size: 0, modified: null, created: null, ext: "", inArchive: false };
}

/** The details' path: copied on click, with "Copied" shown by the pointer for a moment. */
function CopyPath(props: { path: string }) {
  const [copied, setCopied] = createSignal<{ x: number; y: number; ok: boolean } | null>(null);
  let timer: number | undefined;
  onCleanup(() => clearTimeout(timer));
  // Another item: nothing left over from copying the last one.
  createEffect(on(() => props.path, () => { clearTimeout(timer); setCopied(null); }, { defer: true }));
  const copy = (e: MouseEvent) => {
    // Kept inside the window (the pane sits at its right edge).
    const at = { x: Math.min(Math.max(e.clientX, 50), window.innerWidth - 50), y: e.clientY - 10 };
    navigator.clipboard.writeText(props.path).then(() => true, () => false).then((ok) => {
      setCopied({ ...at, ok });
      clearTimeout(timer);
      timer = window.setTimeout(() => setCopied(null), 1200);
    });
  };
  return (
    <dd class="pv-path" classList={{ copied: !!copied()?.ok }} title="Click to copy" onClick={copy}>
      {props.path.split("\\").map((seg, i) => <>{i > 0 && "\\"}{i > 0 && <wbr />}{seg}</>)}
      <span class="pv-copy-ico" aria-hidden="true">{copied()?.ok ? <Check size={12} /> : <Copy size={12} />}</span>
      {/* A new bubble per click, so its animation starts over. */}
      <Show when={copied()} keyed>
        {(c) => <Portal><div class="pv-copied" role="status" style={{ left: `${c.x}px`, top: `${c.y}px` }}>{c.ok ? "Copied" : "Couldn't copy"}</div></Portal>}
      </Show>
    </dd>
  );
}

function MetaList(props: { entry: Entry; extra: Meta[] }) {
  // Recycle Bin items: where they came from and when they were deleted.
  const original = () => props.entry.origin && `${props.entry.origin.replace(/\\$/, "")}\\${props.entry.name}`;
  const rows = (): Meta[] => props.entry.origin ? [
    { label: "Type", value: typeLabel(props.entry) },
    { label: "Size", value: `${formatSize(props.entry.size)}${props.entry.size >= 1024 ? ` (${props.entry.size.toLocaleString()} bytes)` : ""}` },
    ...props.extra,
    ...(props.entry.modified ? [{ label: "Deleted", value: formatDate(props.entry.modified) }] : []),
    ...(props.entry.created ? [{ label: "Modified", value: formatDate(props.entry.created) }] : []),
  ] : [
    { label: "Type", value: typeLabel(props.entry) },
    ...(props.entry.isDir ? [] : [{ label: "Size", value: `${formatSize(props.entry.size)}${props.entry.size >= 1024 ? ` (${props.entry.size.toLocaleString()} bytes)` : ""}` }]),
    ...props.extra,
    ...(props.entry.modified ? [{ label: "Modified", value: formatDate(props.entry.modified) }] : []),
    ...(props.entry.created ? [{ label: "Created", value: formatDate(props.entry.created) }] : []),
  ];
  return (
    <dl class="pv-meta">
      {/* A row kept from the previous item shows a placeholder until its value arrives. */}
      <For each={rows()}>{(r) => <><dt>{r.label}</dt><dd classList={{ "pv-wait": !!r.pending }}>{r.pending ? " " : r.value || " "}</dd></>}</For>
      {/* The Recycle Bin itself has no real path worth showing. */}
      <Show when={!isRecycle(props.entry.path)}>
        <dt>{original() ? "From" : "Path"}</dt>
        <CopyPath path={original() || props.entry.path} />
      </Show>
    </dl>
  );
}

export default function PreviewPane() {
  const focused = useFocusedEntry(140);
  const shown = () => focused() ?? (activeTab() ? folderEntry(activeTab().p) : null);
  const [meta, setMeta] = createSignal<Meta[]>([]);
  // A different item eases in once it's on screen. Pictures fade by themselves once decoded
  // (ImageView), since the previous one stays up until then.
  let body: HTMLDivElement | undefined;
  const swapped = (image: boolean) => !image && enter(body, "lift", 180, "preview");

  const startResize = (e: MouseEvent) => {
    e.preventDefault();
    const sx = e.clientX, sw = ui.previewWidth;
    const move = (m: MouseEvent) => setUi("previewWidth", Math.max(260, Math.min(window.innerWidth * 0.6, sw - (m.clientX - sx))));
    const up = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); persistSession(); };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  return (
    <aside class="preview" style={{ width: `${previewSpace()}px` }}>
      <div class="preview-resizer" onMouseDown={startResize} />
      <Show when={shown()}>
        {(e) => (
          <>
            <header class="pv-head">
              <div class="pv-head-icon"><Thumb entry={e()} size={32} fallbackSize={28} /></div>
              <div class="pv-head-text">
                <div class="pv-name" title={e().name}>{e().name || e().path}</div>
                <div class="pv-sub">{e().isDir && !focused() ? "Current folder" : typeLabel(e())}</div>
              </div>
              <Show when={!e().isDir}>
                <button class="icon-btn" title="Open" onClick={() => openFile(e())}><ExternalLink size={15} /></button>
                <button class="icon-btn" title="Quick Look (Space)" onClick={() => setUi("quickLook", true)}><Maximize2 size={15} /></button>
              </Show>
            </header>
            <div class="pv-body" ref={body}><Preview entry={e()} onMeta={setMeta} onSwap={swapped} /></div>
            <MetaList entry={e()} extra={meta()} />
          </>
        )}
      </Show>
    </aside>
  );
}

export function QuickLook() {
  const t = () => activeTab();
  const focused = useFocusedEntry(0);
  const [meta, setMeta] = createSignal<Meta[]>([]);
  const list = () => t()?.ordered[0]() ?? [];
  const index = () => {
    const e = focused();
    return e ? list().findIndex((x) => pathKey(x.path) === pathKey(e.path)) : -1;
  };

  const go = (delta: number) => {
    const l = list();
    if (!l.length) return;
    const cur = t().cursor[0]();
    const i = cur ? l.findIndex((x) => pathKey(x.path) === pathKey(cur)) : index();
    const next = l[(i + delta + l.length) % l.length];
    const tab = t();
    tab.selection[1]([next.path]);
    tab.cursor[1](next.path);
    tab.anchor[1](next.path);
    tab.reveal[1]((n) => n + 1);
  };
  // Decode the neighbouring pictures ahead of time (and hold on to them) so stepping is instant.
  const warm: HTMLImageElement[] = [];
  createEffect(on(index, (i) => {
    const l = list();
    if (i < 0 || !l.length) return;
    const next = [l[(i + 1) % l.length], l[(i - 1 + l.length) % l.length]]
      .filter((n) => !n.isDir && !n.inArchive && kindOf(n) === "image")
      .map((n) => {
        const img = new Image();
        img.src = imageUrl(n.path, n.ext);
        img.decode().catch(() => {});
        return img;
      });
    warm.splice(0, warm.length, ...next);
  }));
  onCleanup(() => warm.splice(0));

  const close = () => {
    setUi("quickLook", false);
    requestAnimationFrame(() => fileList()?.focus());
  };

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      const media = (e.target as HTMLElement).closest("video, audio, input, textarea");
      if (e.key === "Escape" || (e.key === " " && !media)) { e.preventDefault(); e.stopPropagation(); close(); }
      else if (e.key === "ArrowRight" || e.key === "ArrowDown") { if (media && e.key === "ArrowRight") return; e.preventDefault(); e.stopPropagation(); go(1); }
      else if (e.key === "ArrowLeft" || e.key === "ArrowUp") { if (media && e.key === "ArrowLeft") return; e.preventDefault(); e.stopPropagation(); go(-1); }
      else if (e.key === "Enter") {
        e.preventDefault(); e.stopPropagation();
        const f = focused();
        if (f) { close(); openEntry(t(), f); }
      }
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });

  return (
    <Portal>
      <div class="ql-backdrop" onMouseDown={(e) => e.target === e.currentTarget && close()}>
        <Show when={focused()} fallback={<div class="ql pv-note">Select a file to preview</div>}>
          {(e) => (
            <div class="ql">
              <header class="ql-head">
                <div class="pv-head-icon"><Thumb entry={e()} size={24} fallbackSize={20} /></div>
                <div class="pv-head-text">
                  <div class="pv-name">{e().name}</div>
                  <div class="pv-sub">{[typeLabel(e()), e().isDir ? "" : formatSize(e().size), ...meta().filter((m) => !m.pending).slice(0, 3).map(metaText)].filter(Boolean).join(" · ")}</div>
                </div>
                <span class="ql-count">{index() + 1} / {list().length}</span>
                <button class="icon-btn" title="Previous (←)" onClick={() => go(-1)}><ChevronLeft size={16} /></button>
                <button class="icon-btn" title="Next (→)" onClick={() => go(1)}><ChevronRight size={16} /></button>
                <button class="btn" onClick={() => { close(); openEntry(t(), e()); }}><ExternalLink size={14} /> Open</button>
                <button class="icon-btn" title="Close (Esc / Space)" onClick={close}><X size={16} /></button>
              </header>
              <div class="ql-body"><Preview entry={e()} large onMeta={setMeta} /></div>
            </div>
          )}
        </Show>
      </div>
    </Portal>
  );
}
