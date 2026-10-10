import { AppWindow, Binary, ChevronDown, ChevronRight, FolderOpen, LoaderCircle, PackageOpen, Play } from "lucide-solid";
import { createEffect, createMemo, createResource, createSignal, For, Index, on, onCleanup, Show, type JSX } from "solid-js";
import DOMPurify from "dompurify";
import { marked } from "marked";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, fileUrl, thumbUrl } from "../../lib/api";
import { formatDate, formatSize, typeLabel } from "../../lib/format";
import { parent } from "../../lib/path";
import { fsTick, isDark } from "../../lib/state";
import type { Entry } from "../../lib/types";
import FileIcon from "../FileIcon";
import { highlight, langFor } from "./highlight";
import { useThumbPath } from "../FileView";
import PdfPages from "./PdfPages";

export interface Meta {
  label: string;
  value: string;
}

/** Renderers report extra metadata (dimensions, duration, lines…) through this. */
export type SetMeta = (items: Meta[]) => void;

const Spinner = () => <div class="pv-center"><LoaderCircle size={20} class="spin" /></div>;

export function ErrorNote(props: { children: JSX.Element }) {
  return <div class="pv-center pv-note">{props.children}</div>;
}

// ---------- code / text ----------

const MAX_HIGHLIGHT = 300 * 1024;

export function CodeView(props: { path: string; name: string; ext: string; setMeta: SetMeta; large?: boolean; text?: string }) {
  const [data] = createResource(() => props.path, async (p) => (props.text !== undefined ? { text: props.text, truncated: false, binary: false, encoding: "UTF-8", size: props.text.length } : api.readText(p, props.large ? 4 * 1024 * 1024 : 1024 * 1024)));
  const [html, setHtml] = createSignal<string | null>(null);
  const [wrap, setWrap] = createSignal(false);

  createEffect(() => {
    const d = data();
    if (!d || d.binary) return;
    const body = d.text.replace(/\r?\n$/, "");
    const lines = body.split("\n").length;
    props.setMeta([
      { label: "Lines", value: `${lines.toLocaleString()}${d.truncated ? "+" : ""}` },
      { label: "Encoding", value: d.encoding },
      { label: "Language", value: langFor(props.name, props.ext) },
    ]);
    const src = body.length > MAX_HIGHLIGHT ? body.slice(0, MAX_HIGHLIGHT) : body;
    setHtml(null);
    highlight(src, langFor(props.name, props.ext), isDark()).then(setHtml).catch(() => setHtml(null));
  });

  return (
    <Show when={data()} fallback={<Spinner />}>
      {(d) => (
        <Show when={d().size > 0 || d().text.length > 0} fallback={<ErrorNote>Empty file</ErrorNote>}>
        <Show when={!d().binary} fallback={<HexView path={props.path} />}>
          <div class="pv-code" classList={{ wrap: wrap() }}>
            <button class="pv-chip pv-wrap" onClick={() => setWrap(!wrap())}>{wrap() ? "No wrap" : "Wrap"}</button>
            <Show when={html()} fallback={<pre class="shiki plain"><code>{d().text.slice(0, MAX_HIGHLIGHT)}</code></pre>}>
              <div innerHTML={html()!} />
            </Show>
            <Show when={d().truncated || d().text.length > MAX_HIGHLIGHT}>
              <div class="pv-trunc">Showing the first {formatSize(Math.min(d().text.length, MAX_HIGHLIGHT))} of {formatSize(d().size)}</div>
            </Show>
          </div>
        </Show>
        </Show>
      )}
    </Show>
  );
}

// ---------- JSON tree ----------

export function JsonView(props: { path: string; name: string; ext: string; setMeta: SetMeta; large?: boolean }) {
  const [data] = createResource(() => props.path, (p) => api.readText(p, 8 * 1024 * 1024));
  const [raw, setRaw] = createSignal(false);
  const parsed = createMemo(() => {
    const d = data();
    if (!d || d.truncated) return { ok: false as const };
    try {
      return { ok: true as const, value: JSON.parse(d.text) };
    } catch (e) {
      return { ok: false as const, error: String(e) };
    }
  });
  createEffect(() => {
    const p = parsed();
    if (p.ok) props.setMeta([{ label: "Top level", value: Array.isArray(p.value) ? `Array (${p.value.length})` : typeof p.value === "object" && p.value ? `Object (${Object.keys(p.value).length} keys)` : typeof p.value }]);
  });
  return (
    <Show when={data()} fallback={<Spinner />}>
      <Show when={parsed().ok && !raw()} fallback={
        <div class="pv-stack">
          <Show when={parsed().ok}><button class="pv-chip pv-wrap" onClick={() => setRaw(false)}>Tree</button></Show>
          <Show when={!parsed().ok && (parsed() as { error?: string }).error}>
            <div class="pv-banner">Not valid JSON, so it's shown as text · {(parsed() as { error?: string }).error!.replace(/^SyntaxError: /, "")}</div>
          </Show>
          <CodeView path={props.path} name={props.name} ext={props.ext} setMeta={() => {}} large={props.large} text={data()!.text} />
        </div>
      }>
        <div class="pv-json">
          <button class="pv-chip pv-wrap" onClick={() => setRaw(true)}>Raw</button>
          <JsonNode k={null} value={(parsed() as { value: unknown }).value} depth={0} />
        </div>
      </Show>
    </Show>
  );
}

function JsonNode(props: { k: string | null; value: unknown; depth: number; last?: boolean }) {
  const [open, setOpen] = createSignal(props.depth < 2);
  const isObj = () => props.value !== null && typeof props.value === "object";
  const entries = createMemo(() => (isObj() ? Object.entries(props.value as object) : []));
  const isArr = () => Array.isArray(props.value);
  const LIMIT = 500;
  const key = () => props.k !== null && <><span class="j-key">{isArr() || /^\d+$/.test(props.k!) ? props.k : `"${props.k}"`}</span><span class="j-punc">: </span></>;
  const scalar = (v: unknown) => {
    if (v === null) return <span class="j-null">null</span>;
    if (typeof v === "string") return <span class="j-str">"{v.length > 2000 ? v.slice(0, 2000) + "…" : v}"</span>;
    if (typeof v === "number") return <span class="j-num">{String(v)}</span>;
    if (typeof v === "boolean") return <span class="j-bool">{String(v)}</span>;
    return <span>{String(v)}</span>;
  };
  return (
    <div class="j-node">
      <Show when={isObj()} fallback={<div class="j-line">{key()}{scalar(props.value)}</div>}>
        <div class="j-line j-toggle" onClick={() => setOpen(!open())}>
          {open() ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
          {key()}
          <span class="j-punc">{isArr() ? "[" : "{"}</span>
          <Show when={!open()}><span class="j-more">{entries().length} {isArr() ? "items" : "keys"}</span><span class="j-punc">{isArr() ? "]" : "}"}</span></Show>
        </div>
        <Show when={open()}>
          <div class="j-children">
            <For each={entries().slice(0, LIMIT)}>{([k, v]) => <JsonNode k={k} value={v} depth={props.depth + 1} />}</For>
            <Show when={entries().length > LIMIT}><div class="j-line j-more">… {entries().length - LIMIT} more</div></Show>
          </div>
          <div class="j-line"><span class="j-punc">{isArr() ? "]" : "}"}</span></div>
        </Show>
      </Show>
    </div>
  );
}

// ---------- markdown ----------

export function MarkdownView(props: { path: string; name: string; ext: string; setMeta: SetMeta; large?: boolean }) {
  const [data] = createResource(() => props.path, (p) => api.readText(p, 2 * 1024 * 1024));
  const [source, setSource] = createSignal(false);
  const empty = () => data()?.size === 0;
  const html = createMemo(() => {
    const d = data();
    if (!d) return "";
    const clean = DOMPurify.sanitize(marked.parse(d.text, { async: false }) as string);
    // Relative images resolve against the markdown file's folder.
    const doc = new DOMParser().parseFromString(clean, "text/html");
    const dir = parent(props.path) ?? "";
    doc.querySelectorAll("img").forEach((img) => {
      const src = img.getAttribute("src") ?? "";
      if (src && !/^(https?:|data:)/i.test(src)) img.src = fileUrl(`${dir}\\${src.replace(/\//g, "\\")}`);
    });
    return doc.body.innerHTML;
  });
  let article!: HTMLElement;
  createEffect(on([html, isDark], () => {
    queueMicrotask(() => {
      article?.querySelectorAll<HTMLElement>("pre > code").forEach((code) => {
        const lang = /language-([\w+#-]+)/.exec(code.className)?.[1] ?? "text";
        highlight(code.textContent ?? "", lang, isDark()).then((h) => {
          const pre = code.parentElement;
          if (pre?.isConnected) pre.outerHTML = h;
        }).catch(() => {});
      });
    });
  }));
  createEffect(() => {
    const d = data();
    if (d) props.setMeta([{ label: "Words", value: d.text.split(/\s+/).filter(Boolean).length.toLocaleString() }]);
  });
  const onClick = (e: MouseEvent) => {
    const a = (e.target as HTMLElement).closest("a");
    if (!a) return;
    e.preventDefault();
    const href = a.getAttribute("href") ?? "";
    if (/^https?:|^mailto:/i.test(href)) openUrl(href);
  };
  return (
    <Show when={data()} fallback={<Spinner />}>
      <Show when={!empty()} fallback={<ErrorNote>Empty file</ErrorNote>}>
      <Show when={!source()} fallback={
        <div class="pv-stack">
          <button class="pv-chip pv-wrap" onClick={() => setSource(false)}>Rendered</button>
          <CodeView path={props.path} name={props.name} ext={props.ext} setMeta={() => {}} large={props.large} text={data()!.text} />
        </div>
      }>
        <div class="pv-md-wrap">
          <button class="pv-chip pv-wrap" onClick={() => setSource(true)}>Source</button>
          <article class="pv-md" ref={article} innerHTML={html()} onClick={onClick} />
        </div>
      </Show>
      </Show>
    </Show>
  );
}

// ---------- images ----------

const NATIVE_IMG = new Set(["jpg", "jpeg", "png", "gif", "webp", "avif", "svg", "bmp", "ico", "apng", "jfif"]);

export function ImageView(props: { path: string; ext: string; setMeta: SetMeta; large?: boolean }) {
  const [failed, setFailed] = createSignal(false);
  const [zoom, setZoom] = createSignal(1);
  const [pan, setPan] = createSignal({ x: 0, y: 0 });
  const src = () => (NATIVE_IMG.has(props.ext) && !failed() ? fileUrl(props.path) : thumbUrl(props.path, 1024));
  createEffect(on(() => props.path, () => { setZoom(1); setPan({ x: 0, y: 0 }); setFailed(false); }));

  const onWheel = (e: WheelEvent) => {
    if (!props.large) return;
    e.preventDefault();
    setZoom((z) => Math.max(0.1, Math.min(16, z * (e.deltaY < 0 ? 1.15 : 1 / 1.15))));
  };
  const onDown = (e: MouseEvent) => {
    if (!props.large || e.button !== 0) return;
    const origin = pan();
    const mv = (m: MouseEvent) => setPan({ x: origin.x + (m.clientX - e.clientX), y: origin.y + (m.clientY - e.clientY) });
    const up = () => { window.removeEventListener("mousemove", mv); window.removeEventListener("mouseup", up); };
    window.addEventListener("mousemove", mv);
    window.addEventListener("mouseup", up);
  };

  return (
    <div class="pv-image" classList={{ large: props.large, zoomed: zoom() !== 1 }} onWheel={onWheel} onMouseDown={onDown}
      onDblClick={() => { setZoom(zoom() === 1 ? 2 : 1); setPan({ x: 0, y: 0 }); }}>
      <img
        src={src()}
        draggable={false}
        style={{ transform: `translate(${pan().x}px, ${pan().y}px) scale(${zoom()})` }}
        onLoad={(e) => props.setMeta([{ label: "Dimensions", value: `${e.currentTarget.naturalWidth} × ${e.currentTarget.naturalHeight}` }])}
        onError={() => { if (!failed()) setFailed(true); }}
        alt=""
      />
      <Show when={props.large && zoom() !== 1}><div class="pv-zoom">{Math.round(zoom() * 100)}%</div></Show>
    </div>
  );
}

// ---------- video / audio ----------

const fmtDuration = (s: number) => {
  if (!Number.isFinite(s)) return "";
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = Math.floor(s % 60);
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}` : `${m}:${String(sec).padStart(2, "0")}`;
};

export function VideoView(props: { path: string; setMeta: SetMeta; large?: boolean }) {
  const [failed, setFailed] = createSignal(false);
  const [started, setStarted] = createSignal(!!props.large);
  createEffect(on(() => props.path, () => { setFailed(false); setStarted(!!props.large); }));
  return (
    <Show when={!failed()} fallback={
      <div class="pv-media-fallback">
        <img src={thumbUrl(props.path, 512)} alt="" onError={(e) => (e.currentTarget.style.display = "none")} />
        <div class="pv-note">This video format can't play here. Open it in your player.</div>
      </div>
    }>
      <Show when={started()} fallback={
        <button class="pv-poster" onClick={() => setStarted(true)} title="Play">
          <img src={thumbUrl(props.path, 512)} alt="" onError={(e) => (e.currentTarget.style.opacity = "0")} />
          <span class="pv-play"><Play size={26} fill="currentColor" /></span>
        </button>
      }>
        <video
          class="pv-video"
          src={fileUrl(props.path)}
          controls
          autoplay
          preload="metadata"
          onLoadedMetadata={(e) => props.setMeta([
            { label: "Dimensions", value: `${e.currentTarget.videoWidth} × ${e.currentTarget.videoHeight}` },
            { label: "Duration", value: fmtDuration(e.currentTarget.duration) },
          ])}
          onError={() => setFailed(true)}
        />
      </Show>
    </Show>
  );
}

export function AudioView(props: { path: string; setMeta: SetMeta; large?: boolean }) {
  const [cover, setCover] = createSignal(true);
  createEffect(on(() => props.path, () => setCover(true)));
  return (
    <div class="pv-audio">
      <Show when={cover()} fallback={<div class="pv-audio-icon"><FileIcon entry={{ isDir: false, ext: "mp3", isSymlink: false }} size={72} /></div>}>
        <img class="pv-cover" src={thumbUrl(props.path, 512)} alt="" onError={() => setCover(false)}
          onLoad={(e) => e.currentTarget.naturalWidth <= 1 && setCover(false)} />
      </Show>
      <audio src={fileUrl(props.path)} controls autoplay={props.large}
        onLoadedMetadata={(e) => props.setMeta([{ label: "Duration", value: fmtDuration(e.currentTarget.duration) }])} />
    </div>
  );
}

// ---------- CSV / TSV ----------

function detectDelimiter(sample: string): string {
  const lines = sample.split(/\r?\n/).slice(0, 10).filter(Boolean);
  let best = ",", score = -1;
  for (const d of [",", ";", "\t", "|"]) {
    const counts = lines.map((l) => l.split(d).length - 1);
    const min = Math.min(...counts);
    const consistent = counts.every((c) => c === counts[0]);
    const s = min * (consistent ? 2 : 1);
    if (s > score) { score = s; best = d; }
  }
  return best;
}

export function parseCsv(text: string, delim: string, maxRows = 100_000): string[][] {
  const rows: string[][] = [];
  let row: string[] = [], field = "", i = 0, q = false;
  const n = text.length;
  while (i < n && rows.length < maxRows) {
    const c = text[i];
    if (q) {
      if (c === '"') {
        if (text[i + 1] === '"') { field += '"'; i += 2; continue; }
        q = false; i++; continue;
      }
      field += c; i++; continue;
    }
    if (c === '"' && field === "") { q = true; i++; continue; }
    if (c === delim) { row.push(field); field = ""; i++; continue; }
    if (c === "\n" || c === "\r") {
      row.push(field); rows.push(row); row = []; field = "";
      if (c === "\r" && text[i + 1] === "\n") i++;
      i++; continue;
    }
    field += c; i++;
  }
  if (field !== "" || row.length) { row.push(field); rows.push(row); }
  while (rows.length && rows[rows.length - 1].every((c) => c === "")) rows.pop();
  return rows;
}

export function CsvView(props: { path: string; ext: string; setMeta: SetMeta; large?: boolean }) {
  const [data] = createResource(() => props.path, (p) => api.readText(p, 8 * 1024 * 1024));
  const [sort, setSort] = createSignal<{ col: number; dir: 1 | -1 } | null>(null);
  const [scroll, setScroll] = createSignal(0);
  const [viewH, setViewH] = createSignal(400);
  const ROW_H = 26;

  const table = createMemo(() => {
    const d = data();
    if (!d) return null;
    const delim = props.ext === "tsv" ? "\t" : detectDelimiter(d.text.slice(0, 20000));
    const rows = parseCsv(d.text, delim);
    const header = rows.shift() ?? [];
    const cols = Math.max(header.length, ...rows.slice(0, 200).map((r) => r.length));
    // Column widths from header + a sample of rows.
    const widths = Array.from({ length: cols }, (_, c) => {
      const lens = [header[c] ?? "", ...rows.slice(0, 200).map((r) => r[c] ?? "")].map((s) => s.length);
      return Math.min(320, Math.max(60, Math.max(...lens) * 7.4 + 24));
    });
    return { header, rows, cols, widths, delim, truncated: d.truncated };
  });

  const sorted = createMemo(() => {
    const t = table();
    const s = sort();
    if (!t || !s) return t?.rows ?? [];
    const col = s.col;
    const num = t.rows.slice(0, 50).every((r) => r[col] === undefined || r[col] === "" || !Number.isNaN(Number(r[col])));
    return [...t.rows].sort((a, b) => {
      const x = a[col] ?? "", y = b[col] ?? "";
      return (num ? Number(x) - Number(y) : x.localeCompare(y, undefined, { numeric: true })) * s.dir;
    });
  });

  createEffect(() => {
    const t = table();
    if (t) props.setMeta([
      { label: "Rows", value: `${t.rows.length.toLocaleString()}${t.truncated ? "+" : ""}` },
      { label: "Columns", value: String(t.cols) },
      { label: "Delimiter", value: t.delim === "\t" ? "Tab" : t.delim },
    ]);
  });

  const visibleRows = createMemo(() => {
    const start = Math.max(0, Math.floor(scroll() / ROW_H) - 10);
    const end = Math.min(sorted().length, start + Math.ceil(viewH() / ROW_H) + 20);
    return { start, rows: sorted().slice(start, end) };
  });

  return (
    <Show when={table()} fallback={<Spinner />}>
      {(t) => {
        const tmpl = () => `48px ${t().widths.map((w) => `${w}px`).join(" ")}`;
        const total = () => 48 + t().widths.reduce((a, b) => a + b, 0);
        return (
          <div class="pv-csv" ref={(el) => { const ro = new ResizeObserver(() => requestAnimationFrame(() => setViewH(el.clientHeight))); ro.observe(el); onCleanup(() => ro.disconnect()); }}
            onScroll={(e) => setScroll(e.currentTarget.scrollTop)}>
            <div class="csv-head" style={{ "grid-template-columns": tmpl(), width: `${total()}px` }}>
              <span class="csv-rn">#</span>
              <Index each={Array.from({ length: t().cols }, (_, i) => t().header[i] ?? `Column ${i + 1}`)}>
                {(h, i) => (
                  <button class="csv-h" classList={{ on: sort()?.col === i }} title={h()}
                    onClick={() => setSort((s) => (s?.col === i ? (s.dir === 1 ? { col: i, dir: -1 } : null) : { col: i, dir: 1 }))}>
                    {h()} {sort()?.col === i ? (sort()!.dir === 1 ? "↑" : "↓") : ""}
                  </button>
                )}
              </Index>
            </div>
            <div class="csv-body" style={{ height: `${sorted().length * ROW_H}px`, width: `${total()}px` }}>
              <For each={visibleRows().rows}>
                {(r, i) => (
                  <div class="csv-row" style={{ "grid-template-columns": tmpl(), transform: `translateY(${(visibleRows().start + i()) * ROW_H}px)` }}>
                    <span class="csv-rn">{visibleRows().start + i() + 1}</span>
                    <Index each={Array.from({ length: t().cols }, (_, c) => r[c] ?? "")}>{(v) => <span class="csv-c" title={v()}>{v()}</span>}</Index>
                  </div>
                )}
              </For>
            </div>
          </div>
        );
      }}
    </Show>
  );
}

// ---------- PDF ----------

export function PdfView(props: { path: string; large?: boolean; setMeta: SetMeta }) {
  // pdf.js keeps keyboard focus in Ferret (the WebView2 viewer iframe traps Esc/arrows).
  return <PdfPages path={props.path} setMeta={props.setMeta} large={props.large} />;
}

// ---------- archives ----------

export function ArchiveView(props: { entry: Entry; setMeta: SetMeta; onBrowse: () => void; onExtract: () => void }) {
  const [info] = createResource(() => props.entry.path, (p) => api.archiveInfo(p));
  createEffect(() => {
    const i = info();
    if (i) props.setMeta([
      { label: "Format", value: i.format },
      { label: "Contents", value: `${i.files.toLocaleString()} files, ${i.dirs.toLocaleString()} folders` },
      { label: "Unpacked", value: formatSize(i.totalSize) },
      { label: "Saved", value: !i.totalSize ? "–" : i.compressedSize < i.totalSize
          ? `${Math.round((1 - i.compressedSize / i.totalSize) * 100)}% smaller than the contents`
          : "None: the archive is larger than its contents" },
      ...(i.encrypted ? [{ label: "Encrypted", value: "Yes" }] : []),
    ]);
  });
  return (
    <div class="pv-archive">
      <Show when={!info.error} fallback={<ErrorNote>{String(info.error)}</ErrorNote>}>
        <Show when={info()} fallback={<Spinner />}>
          {(i) => (
            <>
              <div class="pv-actions">
                <button class="btn accent" onClick={props.onBrowse}><FolderOpen size={14} /> Browse</button>
                <button class="btn" onClick={props.onExtract}><PackageOpen size={14} /> Extract here</button>
              </div>
              <div class="pv-archive-list">
                <For each={i().topLevel}>
                  {(n) => (
                    <div class="pv-archive-item">
                      <FileIcon entry={{ isDir: n.endsWith("/"), ext: n.split(".").pop()?.toLowerCase() ?? "", isSymlink: false }} size={14} />
                      <span>{n.replace(/\/$/, "")}</span>
                    </div>
                  )}
                </For>
                <Show when={i().entries > i().topLevel.length}><div class="pv-note small">…and more inside</div></Show>
              </div>
              <Show when={i().comment}><pre class="pv-comment">{i().comment}</pre></Show>
            </>
          )}
        </Show>
      </Show>
    </div>
  );
}

// ---------- fonts ----------

let fontSeq = 0;
export function FontView(props: { path: string; name: string }) {
  const family = `ferret-font-${++fontSeq}`;
  const [ok, setOk] = createSignal<boolean | null>(null);
  createEffect(() => {
    const face = new FontFace(family, `url("${fileUrl(props.path)}")`);
    face.load().then((f) => { document.fonts.add(f); setOk(true); }).catch(() => setOk(false));
    onCleanup(() => document.fonts.delete(face));
  });
  return (
    <Show when={ok() !== false} fallback={<ErrorNote>Couldn't load this font.</ErrorNote>}>
      <div class="pv-font" style={{ "font-family": `"${family}", sans-serif`, opacity: ok() ? 1 : 0.2 }}>
        <div style={{ "font-size": "40px" }}>Aa Bb Cc</div>
        <div style={{ "font-size": "22px" }}>The quick brown fox jumps over the lazy dog</div>
        <div style={{ "font-size": "16px" }}>ABCDEFGHIJKLMNOPQRSTUVWXYZ<br />abcdefghijklmnopqrstuvwxyz<br />0123456789 !@#$%^&*()[]{"{}"}</div>
        <div style={{ "font-size": "12px" }}>Sphinx of black quartz, judge my vow.</div>
      </div>
    </Show>
  );
}

// ---------- folders ----------

export function FolderView(props: { path: string; setMeta: SetMeta; onOpen: (p: string) => void }) {
  const [listing] = createResource(() => [props.path, fsTick()] as const, ([p]) => api.listDir(p));
  const summary = createMemo(() => {
    const l = listing();
    if (!l) return null;
    const visible = l.entries.filter((e) => !e.hidden);
    const files = visible.filter((e) => !e.isDir);
    const dirs = visible.length - files.length;
    const bytes = files.reduce((a, e) => a + e.size, 0);
    const recent = [...visible].sort((a, b) => (b.modified ?? 0) - (a.modified ?? 0)).slice(0, 8);
    const images = files.filter((e) => /^(jpe?g|png|gif|webp|avif|heic|bmp|mp4|mov|mkv|webm)$/.test(e.ext)).slice(0, 4);
    return { total: visible.length, files: files.length, dirs, bytes, recent, images, hidden: l.entries.length - visible.length };
  });
  createEffect(() => {
    const s = summary();
    if (s) props.setMeta([
      { label: "Contains", value: `${s.files.toLocaleString()} files, ${s.dirs.toLocaleString()} folders` },
      ...(s.files ? [{ label: "Size of files", value: formatSize(s.bytes) }] : []),
      ...(s.hidden ? [{ label: "Hidden", value: String(s.hidden) }] : []),
    ]);
  });
  return (
    <Show when={!listing.error} fallback={<ErrorNote>{String(listing.error)}</ErrorNote>}>
      <Show when={summary()} fallback={<Spinner />}>
        {(s) => (
          <div class="pv-folder">
            <Show when={s().images.length}>
              <div class="pv-mosaic" classList={{ one: s().images.length === 1 }}>
                <For each={s().images}>{(e) => <MosaicTile entry={e} />}</For>
              </div>
            </Show>
            <div class="pv-folder-head">{s().total ? "Recently changed" : "This folder is empty"}</div>
            <For each={s().recent}>
              {(e) => (
                <button class="pv-folder-item" onDblClick={() => props.onOpen(e.path)} title={e.path}>
                  <FileIcon entry={e} size={14} />
                  <span class="pv-folder-name">{e.name}</span>
                  <span class="muted">{formatDate(e.modified).slice(0, 10)}</span>
                </button>
              )}
            </For>
          </div>
        )}
      </Show>
    </Show>
  );
}

function MosaicTile(props: { entry: Entry }) {
  const path = useThumbPath(() => props.entry);
  return (
    <Show when={path()} fallback={<div class="pv-mosaic-blank"><FileIcon entry={props.entry} size={40} /></div>}>
      <img src={/\.(svg|png|jpe?g|gif|webp|avif|bmp)$/i.test(path()!) && props.entry.inArchive ? fileUrl(path()!) : thumbUrl(path()!, 256)} alt=""
        onError={(ev) => (ev.currentTarget.style.visibility = "hidden")} />
    </Show>
  );
}

// ---------- fallbacks ----------

export function HexView(props: { path: string; large?: boolean; noThumb?: boolean }) {
  const [bytes] = createResource(() => props.path, (p) => api.readBytes(p, 512));
  const lines = createMemo(() => {
    const b = bytes() ?? [];
    const out: { off: string; hex: string; ascii: string }[] = [];
    const per = props.large ? 16 : 8;
    for (let i = 0; i < b.length; i += per) {
      const chunk = b.slice(i, i + per);
      out.push({
        off: i.toString(16).padStart(8, "0"),
        hex: chunk.map((x) => x.toString(16).padStart(2, "0")).join(" "),
        ascii: chunk.map((x) => (x >= 32 && x < 127 ? String.fromCharCode(x) : "·")).join(""),
      });
    }
    return out;
  });
  return (
    <div class="pv-hex">
      <Show when={!props.noThumb}>
        <img class="pv-hex-thumb" src={thumbUrl(props.path, 256)} alt="" onError={(e) => (e.currentTarget.style.display = "none")} />
      </Show>
      <div class="pv-hex-head">First {(bytes() ?? []).length} bytes</div>
      <For each={lines()}>{(l) => <div class="hex-line"><span class="hex-off">{l.off}</span><span class="hex-bytes">{l.hex}</span><span class="hex-ascii">{l.ascii}</span></div>}</For>
    </div>
  );
}

/** Windows' own thumbnail (Office documents, executables…); a placeholder when there is none. */
export function ShellThumbView(props: { path: string; entry: Entry; large?: boolean }) {
  const [failed, setFailed] = createSignal(false);
  createEffect(on(() => props.path, () => setFailed(false)));
  return (
    <Show when={!failed()} fallback={<NoPreview path={props.path} entry={props.entry} large={props.large} />}>
      <div class="pv-center">
        {/* "No thumbnail" comes back as a 1×1 transparent image. */}
        <img class="pv-shell-thumb" src={thumbUrl(props.path, 512)} alt="" onError={() => setFailed(true)}
          onLoad={(e) => e.currentTarget.naturalWidth <= 1 && setFailed(true)} />
      </div>
    </Show>
  );
}

const PE = /^(exe|dll|sys|ocx|cpl|scr|mui|efi|winmd)$/;

/** Placeholder for files without a visual preview: big icon, what the file is, version
 *  details for programs and libraries, and the raw bytes on request. */
export function NoPreview(props: { path: string; entry: Entry; large?: boolean }) {
  const [iconFailed, setIconFailed] = createSignal(false);
  const [showBytes, setShowBytes] = createSignal(false);
  const [details] = createResource(() => (PE.test(props.entry.ext) ? props.path : null), (p) => api.fileDetails(p).catch(() => null));
  createEffect(on(() => props.path, () => { setIconFailed(false); setShowBytes(false); }));
  const facts = () => [
    ...(details()?.version ?? []),
    ...(details()?.arch ? [["Architecture", details()!.arch!] as [string, string]] : []),
  ];
  return (
    <div class="pv-none" classList={{ large: props.large }}>
      <div class="pv-none-icon">
        <Show when={!iconFailed()} fallback={<FileIcon entry={props.entry} size={88} />}>
          <img src={thumbUrl(props.path, 256, true)} alt="" onError={() => setIconFailed(true)}
            onLoad={(e) => e.currentTarget.naturalWidth <= 1 && setIconFailed(true)} />
        </Show>
      </div>
      <div class="pv-none-type">{typeLabel(props.entry)}</div>
      <div class="pv-none-sub">{props.entry.size ? `${formatSize(props.entry.size)} · ` : ""}No preview for this kind of file</div>
      <Show when={facts().length}>
        <dl class="pv-none-facts">
          <For each={facts()}>{([k, v]) => <><dt>{k}</dt><dd title={v}>{v}</dd></>}</For>
        </dl>
      </Show>
      <div class="pv-none-actions">
        <button class="btn" onClick={() => api.openWithDialog(props.path)}><AppWindow size={14} />Open with…</button>
        <Show when={props.entry.size > 0}>
          <button class="btn" classList={{ on: showBytes() }} onClick={() => setShowBytes((v) => !v)}><Binary size={14} />{showBytes() ? "Hide bytes" : "Show bytes"}</button>
        </Show>
      </div>
      <Show when={showBytes()}><HexView path={props.path} large={props.large} noThumb /></Show>
    </div>
  );
}

// ---------- Word documents ----------

/** .docx rendered as pages (docx-preview, loaded on first use), scaled to fit the pane. */
export function DocxView(props: { path: string; entry: Entry; setMeta: SetMeta; large?: boolean }) {
  let box!: HTMLDivElement;
  let host!: HTMLDivElement;
  const [state, setState] = createSignal<"loading" | "ready" | "error">("loading");
  const [zoom, setZoom] = createSignal(1);
  const fit = () => {
    const page = host?.querySelector<HTMLElement>("section.docx");
    if (!page || !box) return;
    setZoom(Math.min(props.large ? 1.25 : 1, (box.clientWidth - 24) / page.offsetWidth));
  };
  createEffect(on(() => props.path, async (p) => {
    setState("loading");
    host.replaceChildren();
    try {
      const [{ renderAsync }, data] = await Promise.all([
        import("docx-preview"),
        fetch(fileUrl(p)).then((r) => (r.ok ? r.arrayBuffer() : Promise.reject(new Error(r.statusText)))),
      ]);
      if (p !== props.path) return;
      await renderAsync(data, host, undefined, {
        className: "docx", inWrapper: true, breakPages: true, ignoreLastRenderedPageBreak: true,
        renderHeaders: true, renderFooters: true, renderFootnotes: true, useBase64URL: true,
      });
      if (p !== props.path) return;
      // Page text only: the rendered document also carries its own <style> text.
      const text = [...host.querySelectorAll("section.docx")].map((s) => s.textContent ?? "").join(" ");
      const words = text.split(/\s+/).filter(Boolean).length;
      props.setMeta([{ label: "Pages", value: String(host.querySelectorAll("section.docx").length) }, { label: "Words", value: words.toLocaleString() }]);
      setState("ready");
      requestAnimationFrame(fit);
    } catch {
      if (p === props.path) setState("error");
    }
  }));
  const ro = new ResizeObserver(() => fit());
  onCleanup(() => ro.disconnect());
  return (
    <Show when={state() !== "error"} fallback={<ShellThumbView path={props.path} entry={props.entry} large={props.large} />}>
      <div class="pv-docx" ref={(el) => { box = el; ro.observe(el); }}
        onClick={(e) => {
          // Links must never navigate the app itself.
          const a = (e.target as HTMLElement).closest("a");
          if (!a) return;
          e.preventDefault();
          const href = a.getAttribute("href") ?? "";
          if (/^https?:\/\//i.test(href)) openUrl(href).catch(() => {});
        }}>
        <Show when={state() === "loading"}><Spinner /></Show>
        <div class="pv-docx-host" ref={host} style={{ zoom: zoom() }} />
      </div>
    </Show>
  );
}
