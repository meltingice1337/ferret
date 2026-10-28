import { AppWindow, Binary, ChevronDown, ChevronRight, FolderOpen, LoaderCircle, PackageOpen, Play } from "../../lib/icons";
import { createEffect, createMemo, createResource, createSignal, For, Index, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, fileUrl, thumbUrl } from "../../lib/api";
import { formatDate, formatSize, languageName, plural, typeLabel } from "../../lib/format";
import { isRecycle, isRemote, isWsl, parent, remoteParts } from "../../lib/path";
import { activeTab, fsTick, isDark, remoteState } from "../../lib/state";
import { enter } from "../../lib/motion";
import type { Entry, VideoInfo } from "../../lib/types";
import FileIcon from "../FileIcon";
import { highlight, langFor, prepare } from "./highlight";
import { useThumbPath } from "../FileView";
import PdfPages from "./PdfPages";

export interface Meta {
  label: string;
  value: string;
  /** Kept from the previous item while the next one loads (shown blank). */
  pending?: boolean;
}

/** Renderers report extra metadata (dimensions, duration, lines…) through this. */
export type SetMeta = (items: Meta[]) => void;

const Spinner = () => <div class="pv-center"><LoaderCircle size={20} class="spin" /></div>;

export function ErrorNote(props: { children: JSX.Element }) {
  return <div class="pv-center pv-note">{props.children}</div>;
}

// ---------- code / text ----------

const MAX_HIGHLIGHT = 300 * 1024;
/** Highlighter ids ("log", "yaml", "text") shown as words: "Log", "YAML", "Plain text". */
const prettyLang = (id: string) =>
  ({ text: "Plain text", txt: "Plain text", yaml: "YAML", yml: "YAML", json: "JSON", toml: "TOML", xml: "XML", ini: "INI", md: "Markdown" } as Record<string, string>)[id]
  ?? id.charAt(0).toUpperCase() + id.slice(1);

/** Shiki puts a newline between its line spans. The code view shows each line as a row of its
 *  own (number in a gutter, wrapped text indented past it), where those newlines would be
 *  selectable on their own and draw stray boxes. Copying still gives one line per row; an empty
 *  row gets a <br>, or copying would leave the blank line out. */
const asRows = (html: string) =>
  html.replace(/<\/span>\n<span class="line">/g, '</span><span class="line">').replace(/<span class="line"><\/span>/g, '<span class="line"><br></span>');

/** How long the colors may take before the text shows plain meanwhile (a big file). */
const PLAIN_AFTER = 200;

export function CodeView(props: { path: string; name: string; ext: string; setMeta: SetMeta; large?: boolean; text?: string }) {
  const lang = () => langFor(props.name, props.ext);
  // The language loads while the file is read, not after.
  prepare(lang());
  const [data] = createResource(() => props.path, async (p) => (props.text !== undefined ? { text: props.text, truncated: false, binary: false, encoding: "UTF-8", size: props.text.length, lines: null } : api.readText(p, props.large ? 4 * 1024 * 1024 : 1024 * 1024)));
  const [html, setHtml] = createSignal<string | null>(null);
  // The text shows plain only when the colors are slow, so it doesn't flash plain first.
  const [late, setLate] = createSignal(false);
  const [wrap, setWrap] = createSignal(false);
  let lateTimer: number | undefined;
  let runs = 0;
  onCleanup(() => clearTimeout(lateTimer));

  createEffect(() => {
    const d = data();
    if (!d || d.binary) return;
    const body = d.text.replace(/\r?\n$/, "");
    const lines = body ? body.split("\n").length : 0;
    props.setMeta([
      { label: "Lines", value: d.lines ? d.lines.toLocaleString() : `${lines.toLocaleString()}${d.truncated ? "+" : ""}` },
      { label: "Encoding", value: d.encoding },
      { label: "Language", value: languageName(props.ext) || prettyLang(lang()) },
    ]);
    const src = body.length > MAX_HIGHLIGHT ? body.slice(0, MAX_HIGHLIGHT) : body;
    const run = ++runs;
    setHtml(null);
    setLate(false);
    clearTimeout(lateTimer);
    lateTimer = window.setTimeout(() => setLate(true), PLAIN_AFTER);
    highlight(src, lang(), isDark()).then(
      (h) => { if (run === runs) { clearTimeout(lateTimer); setHtml(asRows(h)); } },
      () => { if (run === runs) setLate(true); },
    );
  });

  return (
    <Show when={data()} fallback={<Spinner />}>
      {(d) => (
        <Show when={d().size > 0 || d().text.length > 0} fallback={<ErrorNote>Empty file</ErrorNote>}>
        <Show when={!d().binary} fallback={<HexView path={props.path} />}>
          <div class="pv-code" classList={{ wrap: wrap() }}>
            <button class="pv-chip pv-wrap" onClick={() => setWrap(!wrap())}>{wrap() ? "No wrap" : "Wrap"}</button>
            <Show when={html()} fallback={<Show when={late()}><pre class="shiki plain"><code>{d().text.slice(0, MAX_HIGHLIGHT)}</code></pre></Show>}>
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

/** The Markdown parser and HTML sanitizer, loaded with the first Markdown file shown (not at startup). */
const markdownLibs = () => Promise.all([import("marked"), import("dompurify")]);

export function MarkdownView(props: { path: string; name: string; ext: string; setMeta: SetMeta; large?: boolean }) {
  const [data] = createResource(() => props.path, async (p) => {
    const [d, [{ marked }, { default: DOMPurify }]] = await Promise.all([api.readText(p, 2 * 1024 * 1024), markdownLibs()]);
    return { ...d, html: DOMPurify.sanitize(marked.parse(d.text, { async: false }) as string) };
  });
  const [source, setSource] = createSignal(false);
  const empty = () => data()?.size === 0;
  const html = createMemo(() => {
    const d = data();
    if (!d) return "";
    // Relative images resolve against the markdown file's folder.
    const doc = new DOMParser().parseFromString(d.html, "text/html");
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

/** What the image viewer loads for a file: browser-native formats directly, others via the shell. */
export const imageUrl = (path: string, ext: string) => (NATIVE_IMG.has(ext) ? fileUrl(path) : thumbUrl(path, 1024));

const MAX_ZOOM = 16;
const clamp01 = (n: number) => Math.min(1, Math.max(0, n));

/**
 * Click zooms in on the clicked spot, then the picture follows the mouse so moving across it
 * explores the whole image. Click again, or pick another file, to zoom back out. The wheel
 * zooms in and out from wherever the mouse is.
 */
export function ImageView(props: { path: string; ext: string; setMeta: SetMeta; large?: boolean }) {
  const [failed, setFailed] = createSignal(false);
  const want = () => (failed() ? thumbUrl(props.path, 1024) : imageUrl(props.path, props.ext));
  // The next picture is decoded before it replaces the current one, so stepping through a
  // folder never flashes an empty frame.
  const [src, setSrc] = createSignal(want());
  // Plain variable: the decode callback runs outside Solid's tracking scope.
  let latest = src();
  let shownPath = props.path;
  createEffect(on(want, (url) => {
    latest = url;
    const path = props.path;
    const next = new Image();
    next.src = url;
    next.decode().catch(() => {}).then(() => {
      if (latest !== url) return;
      setSrc(url);
      // A different picture fades in (not the same one reloaded after an edit).
      if (path !== shownPath) enter(pic, "fade", 170, "preview");
      shownPath = path;
    });
  }, { defer: true }));

  let box!: HTMLDivElement;
  let pic!: HTMLImageElement;
  // The first picture after another kind of preview fades in too.
  onMount(() => enter(pic, "fade", 170, "preview"));
  const [zoomed, setZoomed] = createSignal(false);
  const [factor, setFactor] = createSignal(1);
  // `goal` follows the mouse and the clicks; `cur` eases toward it every frame. x/y are where
  // the mouse is across the viewer (0–1), which picks the part of the picture shown.
  const cur = { z: 1, x: 0.5, y: 0.5 };
  const goal = { z: 1, x: 0.5, y: 0.5 };
  let raf = 0;
  let last = 0;

  const apply = () => {
    if (cur.z === 1) {
      pic.style.transform = "";
      return;
    }
    // A zoomed picture larger than the viewer always fills it: the mouse at the left edge
    // shows the picture's left edge, and so on. A smaller one stays centered.
    const bw = box.clientWidth, bh = box.clientHeight;
    const w = pic.offsetWidth * cur.z, h = pic.offsetHeight * cur.z;
    const left = w > bw ? (bw - w) * cur.x : (bw - w) / 2;
    const top = h > bh ? (bh - h) * cur.y : (bh - h) / 2;
    pic.style.transform = `translate(${left - pic.offsetLeft}px, ${top - pic.offsetTop}px) scale(${cur.z})`;
  };
  const frame = (now: number) => {
    const dt = Math.min(64, now - (last || now - 16));
    last = now;
    const ease = (from: number, to: number, ms: number) => {
      const v = from + (to - from) * (1 - Math.exp(-dt / ms));
      return Math.abs(to - v) < 0.0005 ? to : v;
    };
    cur.z = ease(cur.z, goal.z, 70);
    cur.x = ease(cur.x, goal.x, 55);
    cur.y = ease(cur.y, goal.y, 55);
    apply();
    const settled = cur.z === goal.z && cur.x === goal.x && cur.y === goal.y;
    raf = settled ? 0 : requestAnimationFrame(frame);
    if (settled) last = 0;
  };
  const animate = () => { if (!raf) raf = requestAnimationFrame(frame); };
  onCleanup(() => cancelAnimationFrame(raf));

  /** Where the mouse is across the viewer. The outer 8% counts as the edge, so the picture's
   *  edges come into view before the mouse leaves the viewer. */
  const pointAt = (e: MouseEvent) => {
    const r = box.getBoundingClientRect();
    return {
      x: clamp01(((e.clientX - r.left) / r.width - 0.08) / 0.84),
      y: clamp01(((e.clientY - r.top) / r.height - 0.08) / 0.84),
    };
  };
  const setZoom = (z: number, e: MouseEvent) => {
    if (goal.z === 1) {
      // Zoom in toward the spot under the mouse.
      const p = pointAt(e);
      cur.x = goal.x = p.x;
      cur.y = goal.y = p.y;
    }
    goal.z = z <= 1.02 ? 1 : Math.min(MAX_ZOOM, z);
    setZoomed(goal.z > 1);
    setFactor(Math.round(goal.z * 10) / 10);
    animate();
  };
  const reset = () => {
    cancelAnimationFrame(raf);
    raf = 0;
    Object.assign(cur, { z: 1, x: 0.5, y: 0.5 });
    Object.assign(goal, cur);
    setZoomed(false);
    if (pic) apply();
  };
  createEffect(on(() => props.path, () => { setFailed(false); reset(); }, { defer: true }));

  const onClick = (e: MouseEvent) => {
    if (goal.z > 1) return setZoom(1, e);
    // Big pictures open at about 1:1; small ones at least double.
    const scale = (pic.naturalWidth || pic.offsetWidth) / pic.offsetWidth;
    setZoom(Math.min(4, Math.max(2, scale)), e);
  };
  const onMove = (e: MouseEvent) => {
    if (goal.z === 1) return;
    Object.assign(goal, pointAt(e));
    animate();
  };
  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    setZoom(goal.z * (e.deltaY < 0 ? 1.25 : 0.8), e);
  };

  return (
    <div class="pv-image" classList={{ large: props.large, zoomed: zoomed(), opaque: /^(jpe?g|jfif|bmp)$/i.test(props.ext) }} ref={box}
      onClick={onClick} onMouseMove={onMove} onWheel={onWheel}>
      <img
        ref={pic}
        src={src()}
        draggable={false}
        onLoad={(e) => props.setMeta([{ label: "Dimensions", value: `${e.currentTarget.naturalWidth} × ${e.currentTarget.naturalHeight}` }])}
        onError={() => { if (!failed()) setFailed(true); }}
        alt=""
      />
      <Show when={zoomed()}><div class="pv-zoom">{factor()}×</div></Show>
    </div>
  );
}

// ---------- video / audio ----------

const fmtDuration = (s: number) => {
  if (!Number.isFinite(s)) return "";
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = Math.floor(s % 60);
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}` : `${m}:${String(sec).padStart(2, "0")}`;
};

const bitrate = (bps: number) => (bps >= 1e6 ? `${(bps / 1e6).toFixed(1)} Mbps` : `${Math.round(bps / 1e3)} kbps`);

/** A video's details as Windows reads them (no playing needed): size as shown, length, frame
 *  rate, codec, bitrate, sound. */
function videoMeta(v: VideoInfo): Meta[] {
  const turned = v.rotation === 90 || v.rotation === 270;
  const rows: Meta[] = [];
  if (v.width && v.height) rows.push({ label: "Dimensions", value: turned ? `${v.height} × ${v.width}` : `${v.width} × ${v.height}` });
  if (v.durationMs) rows.push({ label: "Duration", value: fmtDuration(v.durationMs / 1000) });
  if (v.fps) rows.push({ label: "Frame rate", value: `${Math.round(v.fps * 100) / 100} fps` });
  if (v.codec && v.codec !== "Unknown") rows.push({ label: "Codec", value: v.codec });
  if (v.videoBitrate) rows.push({ label: "Bitrate", value: bitrate(v.videoBitrate + v.audioBitrate) });
  rows.push({ label: "Audio", value: !v.hasAudio ? "None" : v.audioBitrate ? bitrate(v.audioBitrate) : "Yes" });
  return rows;
}

export function VideoView(props: { path: string; setMeta: SetMeta; large?: boolean }) {
  const [failed, setFailed] = createSignal(false);
  const [started, setStarted] = createSignal(!!props.large);
  // Quick Look plays a video right away, without sound until it's turned on; one started with
  // its Play button has its sound.
  let clicked = false;
  createEffect(on(() => props.path, () => { setFailed(false); setStarted(!!props.large); }));
  // The details come from Windows, so they show before the video plays. A video Windows can't
  // read gets the player's (size and length) once it plays, if the player can.
  const [info] = createResource(() => props.path, (p) => api.videoInfo(p).catch(() => null));
  createEffect(() => { const v = info(); if (v && v.path === props.path) props.setMeta(videoMeta(v)); });
  return (
    <Show when={!failed()} fallback={
      <div class="pv-media-fallback">
        <img src={thumbUrl(props.path, 512)} alt="" onError={(e) => (e.currentTarget.style.display = "none")} />
        <div class="pv-note">This video format can't play here. Open it in your player.</div>
      </div>
    }>
      <Show when={started()} fallback={
        <button class="pv-poster" onClick={() => { clicked = true; setStarted(true); }} title="Play">
          <img src={thumbUrl(props.path, 512)} alt="" onError={(e) => (e.currentTarget.style.opacity = "0")} />
          <span class="pv-play"><Play size={26} fill="currentColor" /></span>
        </button>
      }>
        <video
          class="pv-video"
          ref={(el) => { if (!clicked) el.muted = true; }}
          src={fileUrl(props.path)}
          controls
          autoplay
          preload="metadata"
          onLoadedMetadata={(e) => !info() && props.setMeta([
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
  // Tags (title, artist, album…) as Windows reads them; the player's length until they come.
  const [tags] = createResource(() => props.path, (p) => api.audioInfo(p).catch(() => null));
  createEffect(() => {
    const t = tags();
    if (!t) return;
    const rows: Meta[] = [];
    const add = (label: string, value: string | number) => { if (value) rows.push({ label, value: String(value) }); };
    add("Title", t.title);
    add("Artist", t.artist);
    add("Album", t.album);
    if (t.albumArtist !== t.artist) add("Album artist", t.albumArtist);
    add("Year", t.year);
    add("Track", t.track);
    if (t.durationMs) add("Duration", fmtDuration(t.durationMs / 1000));
    if (t.bitrate) add("Bitrate", bitrate(t.bitrate));
    if (rows.length) props.setMeta(rows);
  });
  return (
    <div class="pv-audio">
      <Show when={cover()} fallback={<div class="pv-audio-icon"><FileIcon entry={{ isDir: false, ext: "mp3", isSymlink: false }} size={72} /></div>}>
        <img class="pv-cover" src={thumbUrl(props.path, 512)} alt="" onError={() => setCover(false)}
          onLoad={(e) => e.currentTarget.naturalWidth <= 1 && setCover(false)} />
      </Show>
      <audio src={fileUrl(props.path)} controls
        onLoadedMetadata={(e) => !tags()?.durationMs && props.setMeta([{ label: "Duration", value: fmtDuration(e.currentTarget.duration) }])} />
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
      { label: "Contents", value: `${plural(i.files, "file")}, ${plural(i.dirs, "folder")}` },
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
                <Show when={i().entries > i().topLevel.length}><div class="pv-note small">{plural(i().entries, "item")} in all</div></Show>
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
  // A distro picked in \\wsl.localhost may not be running, and reading it would start it: only
  // opening it does that (as in Explorer).
  const distroFromOutside = () => isWsl(props.path) && !parent(props.path) && !isWsl(activeTab().p);
  // Likewise a server's folder is only read while it's connected (a click connects it).
  const offline = () => isRemote(props.path) && remoteState[remoteParts(props.path)?.id ?? ""] !== "connected";
  const [listing] = createResource(() => !distroFromOutside() && !offline() && ([props.path, fsTick()] as const), ([p]) => api.listDir(p));
  const summary = createMemo(() => {
    const l = listing();
    if (!l) return null;
    const visible = l.entries.filter((e) => !e.hidden);
    const files = visible.filter((e) => !e.isDir);
    const dirs = visible.length - files.length;
    const bytes = files.reduce((a, e) => a + e.size, 0);
    const recent = [...visible].sort((a, b) => (b.modified ?? 0) - (a.modified ?? 0)).slice(0, 8);
    // Recycled files have no thumbnails of their own (only blurry icons), so no mosaic there.
    const images = isRecycle(props.path) ? [] : files.filter((e) => /^(jpe?g|png|gif|webp|avif|heic|bmp|mp4|mov|mkv|webm)$/.test(e.ext)).slice(0, 4);
    return { total: visible.length, files: files.length, dirs, bytes, recent, images, hidden: l.entries.length - visible.length };
  });
  createEffect(() => {
    const s = summary();
    if (s) props.setMeta([
      { label: "Contains", value: `${plural(s.files, "file")}, ${plural(s.dirs, "folder")}` },
      ...(s.files ? [{ label: "Size of files", value: formatSize(s.bytes) }] : []),
      ...(s.hidden ? [{ label: "Hidden", value: String(s.hidden) }] : []),
    ]);
  });
  return (
    <Show when={!listing.error} fallback={<ErrorNote>{String(listing.error)}</ErrorNote>}>
      <Show when={summary()} fallback={distroFromOutside() ? <div class="pv-folder-head">Open the distro to see what's inside</div> : <Spinner />}>
        {(s) => (
          <div class="pv-folder">
            <Show when={s().images.length}>
              <div class="pv-mosaic" classList={{ one: s().images.length === 1 }}>
                <For each={s().images}>{(e) => <MosaicTile entry={e} />}</For>
              </div>
            </Show>
            {/* An empty folder already says so in the file list. */}
            <Show when={s().total}>
              <div class="pv-folder-head">{isRecycle(props.path) ? "Recently deleted" : "Recently changed"}</div>
            </Show>
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

/** Page margin left at each side when a narrow pane trims them (px at 100%). */
const TRIMMED_MARGIN = 24;

/** .docx rendered as pages (docx-preview, loaded on first use), scaled to fit the pane.
 *  A narrow pane trims the pages' side margins (the lines break where they did) so the text is
 *  bigger, and Ctrl+wheel zooms. */
export function DocxView(props: { path: string; entry: Entry; setMeta: SetMeta; large?: boolean }) {
  let box!: HTMLDivElement;
  let host!: HTMLDivElement;
  const [state, setState] = createSignal<"loading" | "ready" | "error">("loading");
  const [zoom, setZoom] = createSignal(1);
  /** Ctrl+wheel's factor on top of the fitted size. */
  const [scale, setScale] = createSignal(1);
  /** Each page's own width and side margins, as docx-preview laid it out. */
  const layout = new WeakMap<HTMLElement, { w: number; l: number; r: number }>();
  const fit = () => {
    const pages = [...(host?.querySelectorAll<HTMLElement>("section.docx") ?? [])];
    if (!pages.length || !box) return;
    for (const s of pages) {
      if (layout.has(s)) continue;
      const cs = getComputedStyle(s);
      layout.set(s, { w: s.offsetWidth, l: parseFloat(cs.paddingLeft) || 0, r: parseFloat(cs.paddingRight) || 0 });
    }
    const room = box.clientWidth - 24;
    const first = layout.get(pages[0])!;
    const trim = !props.large && room / first.w < 0.6;
    for (const s of pages) {
      const o = layout.get(s)!;
      const l = trim ? Math.min(o.l, TRIMMED_MARGIN) : o.l, r = trim ? Math.min(o.r, TRIMMED_MARGIN) : o.r;
      s.style.paddingLeft = `${l}px`;
      s.style.paddingRight = `${r}px`;
      s.style.width = `${o.w - o.l - o.r + l + r}px`;
    }
    setZoom(Math.min(props.large ? 1.25 : 1, room / pages[0].offsetWidth) * scale());
  };
  const wheel = (e: WheelEvent) => {
    if (!e.ctrlKey || state() !== "ready") return;
    e.preventDefault();
    setScale((s) => Math.max(0.5, Math.min(4, s * (e.deltaY < 0 ? 1.15 : 1 / 1.15))));
    fit();
  };
  createEffect(on(() => props.path, async (p) => {
    setState("loading");
    setScale(1);
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
      <div class="pv-docx-frame">
        <div class="pv-docx" ref={(el) => { box = el; ro.observe(el); }} onWheel={wheel}
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
        <Show when={scale() !== 1}><div class="pv-zoom">{Math.round(zoom() * 100)}%</div></Show>
      </div>
    </Show>
  );
}
