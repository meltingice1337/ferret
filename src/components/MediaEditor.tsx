// Compress editor for videos and pictures (right-click → "Compress video…" / "Compress picture…").
// Encoding runs in media.rs with Windows' own codecs; this is the window around it: a live
// preview (trim timeline and encoded samples for video, a before/after slider for pictures),
// the settings, and the size it will come out at.

import {
  AlertTriangle, ChevronLeft, ChevronRight, CircleCheck, Clapperboard, ImageDown, Loader2, Maximize2, Minimize2, Pause, Play, RotateCcw,
  Sparkles, Volume2, VolumeX, X,
} from "../lib/icons";
import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { createStore } from "solid-js/store";
import { listen } from "@tauri-apps/api/event";
import { Dynamic } from "solid-js/web";
import { api, fileUrl, thumbUrl } from "../lib/api";
import { formatSize, plural } from "../lib/format";
import { compressMedia } from "../lib/ops";
import { basename, parent } from "../lib/path";
import { luma, ssim, verdict } from "../lib/quality";
import { setUi, toast, ui } from "../lib/state";
import type { VideoInfo, VideoJob, VideoSample } from "../lib/types";

export default function MediaEditor() {
  return (
    <Show when={ui.media} keyed>
      {(m) => (m.kind === "video" ? <VideoEditor paths={m.paths} /> : <ImageEditor paths={m.paths} />)}
    </Show>
  );
}

export function closeMediaEditor() {
  setUi("media", null);
  api.mediaRelease().catch(() => {});
}

// ---------- shared pieces ----------

const MiB = 1024 * 1024;
const stemOf = (p: string) => basename(p).replace(/\.[^.\\]+$/, "");
const even = (n: number) => Math.max(2, Math.round(n / 2) * 2);
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
const BAD_NAME = /[<>:"/\\|?*\u0000-\u001f]|[. ]$/;

function fmtTime(ms: number, tenths = true) {
  const s = Math.max(0, ms) / 1000;
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
  const ss = tenths ? sec.toFixed(1).padStart(4, "0") : String(Math.floor(sec)).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** `src()` once it has stopped changing for `ms`. */
function settled<T>(src: () => T, ms: number): () => T | undefined {
  const [v, setV] = createSignal<T>();
  createEffect(() => {
    const x = src();
    const id = setTimeout(() => setV(() => x), ms);
    onCleanup(() => clearTimeout(id));
  });
  return v;
}

function Seg<T extends string | number | boolean>(props: { value: T; options: { value: T; label: string; title?: string }[]; onChange: (v: T) => void }) {
  return (
    <div class="seg mx-seg" role="radiogroup">
      <For each={props.options}>
        {(o) => (
          <button role="radio" aria-checked={props.value === o.value} classList={{ on: props.value === o.value }} title={o.title}
            onClick={() => props.onChange(o.value)}>{o.label}</button>
        )}
      </For>
    </div>
  );
}

function Section(props: { title: string; aside?: JSX.Element; children: JSX.Element }) {
  return (
    <section class="mx-sec">
      <div class="mx-sec-head"><h3>{props.title}</h3>{props.aside}</div>
      {props.children}
    </section>
  );
}

function Presets<T extends { id: string; label: string; hint: string }>(props: { items: T[]; current: string | null; onPick: (p: T) => void; hintOf?: (p: T) => string }) {
  return (
    <div class="mx-presets">
      <For each={props.items}>
        {(p) => (
          <button class="mx-preset" classList={{ on: props.current === p.id }} onClick={() => props.onPick(p)}>
            <b>{p.label}</b><small>{props.hintOf?.(p) ?? p.hint}</small>
          </button>
        )}
      </For>
    </div>
  );
}

function NameField(props: { single: boolean; value: string; ext: string; onInput: (v: string) => void; example: string; error: string | null }) {
  return (
    <Section title={props.single ? "Save as" : "Name ending"}>
      <div class="mx-name" classList={{ invalid: !!props.error }}>
        <input class="text-input" value={props.value} spellcheck={false} onInput={(e) => props.onInput(e.currentTarget.value)} />
        <span class="mx-ext">{props.ext}</span>
      </div>
      <p class="mx-hint" classList={{ err: !!props.error }}>{props.error ?? (props.single ? "Saved next to the original, which stays as it is." : `e.g. ${props.example}`)}</p>
    </Section>
  );
}

function Summary(props: { before: number; after: number | null; approx: boolean; note: JSX.Element }) {
  const ratio = () => (props.after == null || !props.before ? null : props.after / props.before);
  const pct = () => (ratio() == null ? null : Math.round((1 - ratio()!) * 100));
  return (
    <div class="mx-sum">
      <div class="mx-sum-sizes">
        <span class="mx-before">{formatSize(props.before)}</span>
        <span class="mx-arrow">→</span>
        <span class="mx-after">{props.after == null ? "…" : `${props.approx ? "≈ " : ""}${formatSize(props.after)}`}</span>
        <Show when={pct() != null}>
          <span class="mx-badge" classList={{ bad: pct()! <= 0, warn: pct()! > 0 && pct()! < 10 }}>{pct()! <= 0 ? "not smaller" : pct()! < 10 ? `only ${pct()}% smaller` : `${pct()}% smaller`}</span>
        </Show>
      </div>
      <div class="mx-bar"><div style={{ width: `${clamp((ratio() ?? 1) * 100, 1.5, 100)}%` }} classList={{ bad: (ratio() ?? 0) >= 1 }} /></div>
      <div class="mx-sum-note">{props.note}</div>
    </div>
  );
}

let leftFullscreenAt = -Infinity;
document.addEventListener("fullscreenchange", () => { if (!document.fullscreenElement) leftFullscreenAt = performance.now(); });
/** Esc in full screen only leaves full screen (the webview does that itself, and may pass the
 *  key on too, even after leaving): true when that is what this Esc was for. */
export function escLeavesFullscreen() {
  if (!document.fullscreenElement && performance.now() - leftFullscreenAt > 300) return false;
  if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
  return true;
}

function Shell(props: {
  icon: any; title: string; sub: string; stage: JSX.Element; side: JSX.Element; summary: JSX.Element;
  canSubmit: boolean; onSubmit: () => void; onKey?: (e: KeyboardEvent) => void;
}) {
  return (
    <div class="modal-backdrop">
      <div class="modal mx" role="dialog" aria-label={props.title} tabIndex={-1}
        ref={(el) => queueMicrotask(() => el.focus())}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Escape") {
            e.preventDefault();
            if (escLeavesFullscreen()) return;
            return closeMediaEditor();
          }
          const inField = !!(e.target as HTMLElement).closest("input, select, textarea");
          if (e.key === "Enter" && (e.ctrlKey || (inField && (e.target as HTMLInputElement).type === "text"))) {
            e.preventDefault();
            if (props.canSubmit) props.onSubmit();
            return;
          }
          if (!inField) props.onKey?.(e);
        }}>
        <header class="modal-head mx-head">
          <span class="mx-head-icon"><Dynamic component={props.icon} size={18} /></span>
          <div class="mx-head-text"><h2>{props.title}</h2><span class="muted">{props.sub}</span></div>
          <button class="icon-btn" onClick={closeMediaEditor} title="Close (Esc)"><X size={16} /></button>
        </header>
        <div class="mx-main">
          <div class="mx-stage">{props.stage}</div>
          <aside class="mx-side">{props.side}</aside>
        </div>
        <footer class="mx-foot">
          {props.summary}
          <div class="mx-actions">
            <button class="btn" onClick={closeMediaEditor}>Cancel</button>
            <button class="btn accent" disabled={!props.canSubmit} onClick={props.onSubmit} title="Ctrl+Enter">Compress</button>
          </div>
        </footer>
      </div>
    </div>
  );
}

/** "‹ 2 of 5 · name ›" when several files share the settings. */
function Pager(props: { paths: string[]; idx: number; onIdx: (i: number) => void }) {
  return (
    <Show when={props.paths.length > 1}>
      <div class="mx-pager">
        <button class="icon-btn tiny" disabled={props.idx === 0} onClick={() => props.onIdx(props.idx - 1)} title="Previous"><ChevronLeft size={14} /></button>
        <span><b>{props.idx + 1}</b> of {props.paths.length} · {basename(props.paths[props.idx])}</span>
        <button class="icon-btn tiny" disabled={props.idx >= props.paths.length - 1} onClick={() => props.onIdx(props.idx + 1)} title="Next"><ChevronRight size={14} /></button>
      </div>
    </Show>
  );
}

// ---------- video ----------

interface VSettings { res: number; fps: number; mode: "quality" | "size"; quality: number; targetMb: number; hevc: boolean; audio: number }

const VPRESETS: { id: string; label: string; hint: string; s: VSettings }[] = [
  { id: "small", label: "Small", hint: "720p · 30 fps", s: { res: 720, fps: 30, mode: "quality", quality: 28, targetMb: 10, hevc: false, audio: 96 } },
  { id: "balanced", label: "Balanced", hint: "1080p · 30 fps", s: { res: 1080, fps: 30, mode: "quality", quality: 50, targetMb: 10, hevc: false, audio: 128 } },
  { id: "high", label: "High quality", hint: "Full size", s: { res: 0, fps: 0, mode: "quality", quality: 75, targetMb: 10, hevc: false, audio: 160 } },
  { id: "share", label: "Fit 10 MB", hint: "Discord, chats", s: { res: 720, fps: 30, mode: "size", quality: 50, targetMb: 10, hevc: false, audio: 96 } },
];
const RESOLUTIONS = [2160, 1440, 1080, 720, 540, 480, 360];
const SAMPLE_MS = 4000;

/** Bits per pixel per frame for the quality slider: 0.01 … 0.12, evenly on a log scale. */
const bpp = (q: number) => 0.01 * Math.pow(12, q / 100);
const qualityLabel = (q: number) => (q < 20 ? "Smallest" : q < 40 ? "Small" : q < 62 ? "Balanced" : q < 82 ? "High" : "Best");

/** What the encoder is asked for, and the size that should come out. */
function plan(info: VideoInfo, s: VSettings, trim: [number, number] | null) {
  const turned = info.rotation === 90 || info.rotation === 270;
  const dw = turned ? info.height : info.width, dh = turned ? info.width : info.height;
  const short = Math.min(dw, dh);
  const k = s.res && s.res < short ? s.res / short : 1;
  const ow = even(dw * k), oh = even(dh * k);
  const fps = s.fps && info.fps && s.fps < info.fps - 0.5 ? s.fps : 0;
  const durMs = Math.max(1, trim ? trim[1] - trim[0] : info.durationMs);
  const audio = info.hasAudio ? s.audio * 1000 : 0;
  let vb: number;
  let tooSmall = false;
  if (s.mode === "size") {
    // Room for the container and the encoder overshooting a little.
    const bits = s.targetMb * MiB * 8 * 0.93 - 64_000 * 8;
    vb = bits / (durMs / 1000) - audio;
    if (vb < 200_000) { tooSmall = true; vb = 200_000; }
  } else {
    vb = bpp(s.quality) * ow * oh * (fps || info.fps || 30) * (s.hevc ? 0.62 : 1);
  }
  // Never ask for more than the original had (a short clip "fit to 10 MB" would grow).
  if (info.videoBitrate > 0) vb = Math.min(vb, info.videoBitrate * (s.hevc ? 0.8 : 1) * Math.min(1, ((ow * oh) / (dw * dh)) * 1.15));
  vb = Math.round(Math.max(100_000, vb));
  const bytes = ((vb + audio) * durMs) / 8000 * 1.01 + 24_000;
  return { ow, oh, sw: turned ? oh : ow, sh: turned ? ow : oh, dw, dh, fps, vb, audio, durMs, bytes, tooSmall };
}

const sameSettings = (a: VSettings, b: VSettings) =>
  a.res === b.res && a.fps === b.fps && a.mode === b.mode && a.hevc === b.hevc && a.audio === b.audio &&
  (a.mode === "size" ? a.targetMb === b.targetMb : a.quality === b.quality);

/** Draws frames along the timeline (a hidden second <video> seeks through the file). */
async function drawFilmstrip(src: string, canvas: HTMLCanvasElement, durMs: number, stop: () => boolean) {
  const v = document.createElement("video");
  v.muted = true;
  v.preload = "auto";
  v.src = src;
  try {
    await new Promise((res, rej) => { v.onloadeddata = res; v.onerror = rej; });
    const dpr = window.devicePixelRatio || 1;
    const W = Math.round(canvas.clientWidth * dpr), H = Math.round(canvas.clientHeight * dpr);
    if (!W || !H || !v.videoWidth) return;
    canvas.width = W;
    canvas.height = H;
    const ctx = canvas.getContext("2d")!;
    const tw = H * (v.videoWidth / v.videoHeight);
    const n = Math.ceil(W / tw);
    for (let i = 0; i < n && !stop(); i++) {
      v.currentTime = Math.min(durMs / 1000 - 0.05, (((i + 0.5) * tw) / W) * (durMs / 1000));
      await new Promise((r) => v.addEventListener("seeked", r, { once: true }));
      if (stop()) break;
      ctx.drawImage(v, i * tw, 0, tw, H);
    }
  } catch {
    /* no strip for formats the webview can't play */
  } finally {
    v.removeAttribute("src");
    v.load();
  }
}

/** A hidden <video> for reading frames. Anonymous CORS: the file protocol allows it, and a frame
 *  from it can be read back from a canvas. */
async function frameReader(src: string) {
  const v = document.createElement("video");
  v.muted = true;
  v.preload = "auto";
  v.crossOrigin = "anonymous";
  v.src = src;
  await new Promise((res, rej) => { v.onloadeddata = res; v.onerror = rej; });
  return v;
}
/** Seeks and waits for the frame. Already there: nothing to wait for (no "seeked" may come). */
const seekTo = (v: HTMLVideoElement, s: number) => new Promise<void>((resolve, reject) => {
  if (Math.abs(v.currentTime - s) < 1e-6 && !v.seeking) return resolve();
  const timer = setTimeout(() => reject(new Error("seek timed out")), 3000);
  v.addEventListener("seeked", () => { clearTimeout(timer); resolve(); }, { once: true });
  v.currentTime = s;
});
const SAMPLE_FRAMES = 8;

/** The quality check: frames of a preview sample against the same moments of the original (see
 *  lib/quality.ts). Both are drawn at the original's size (at most 1920 wide) and averaged down
 *  to about 960 wide. Returns the worst frame's score and when it is (in the sample's time), and
 *  how far the sample's pictures are from the original's (`shift`, seconds), or null when the
 *  webview can't play one of them (or `stop` says the sample was replaced). */
async function checkSample(original: string, sample: string, atMs: number, durMs: number, fps: number, origFps: number, stop: () => boolean) {
  let o: HTMLVideoElement | undefined, s: HTMLVideoElement | undefined;
  try {
    [o, s] = await Promise.all([frameReader(original), frameReader(sample)]);
    const W = Math.min(1920, o.videoWidth), H = Math.round((o.videoHeight * W) / o.videoWidth);
    const f = Math.max(2, Math.round(W / 960));
    const ca = new OffscreenCanvas(W, H).getContext("2d", { willReadFrequently: true })!;
    const cb = new OffscreenCanvas(W, H).getContext("2d", { willReadFrequently: true })!;
    const picture = (ctx: OffscreenCanvasRenderingContext2D, v: HTMLVideoElement, by: number) => {
      ctx.drawImage(v, 0, 0, W, H);
      return luma(ctx.getImageData(0, 0, W, H).data, W, H, by);
    };
    // The middle of one of the sample's frames, so both show the same picture.
    const frameAt = (k: number) => (Math.floor(((k + 0.5) / SAMPLE_FRAMES) * (durMs / 1000) * fps) + 0.5) / fps;
    const origAt = (t: number) => clamp(atMs / 1000 + t, 0, Math.max(0, o!.duration - 0.05));
    // A sample can show the original's picture from a frame or two earlier or later (the encoder
    // shifts the video, e.g. to make room for the sound): line them up first, on one frame.
    let shift = 0;
    {
      const t = frameAt(0), step = 1 / (origFps || fps);
      await seekTo(s, t);
      const b = picture(cb, s, f * 2);
      let best = -Infinity;
      for (const d of [0, -1, 1, -2, 2, -3, 3, -4, 4]) {
        if (stop()) return null;
        await seekTo(o, origAt(t + d * step));
        const score = ssim(picture(ca, o, f * 2), b);
        if (score > best + 1e-4) { best = score; shift = d * step; }
      }
    }
    let worst = { score: 1, ms: atMs };
    for (let k = 0; k < SAMPLE_FRAMES; k++) {
      if (stop()) return null;
      const t = frameAt(k);
      await Promise.all([seekTo(s, t), seekTo(o, origAt(t + shift))]);
      const score = ssim(picture(ca, o, f), picture(cb, s, f));
      if (score < worst.score) worst = { score, ms: atMs + t * 1000 };
    }
    return stop() ? null : { ...worst, shift };
  } catch {
    return null;
  } finally {
    for (const v of [o, s]) if (v) { v.removeAttribute("src"); v.load(); }
  }
}

function VideoEditor(props: { paths: string[] }) {
  const single = props.paths.length === 1;
  const [results] = createResource(() => props.paths, (ps) => Promise.allSettled(ps.map((p) => api.videoInfo(p))));
  const infos = createMemo(() => (results() ?? []).flatMap((r) => (r.status === "fulfilled" ? [r.value] : [])));
  const errors = createMemo(() => (results() ?? []).flatMap((r, i) => (r.status === "rejected" ? [{ path: props.paths[i], error: String(r.reason) }] : [])));
  const [idx, setIdx] = createSignal(0);
  const cur = () => infos()[idx()] as VideoInfo | undefined;

  const [s, setS] = createStore<VSettings>({ ...VPRESETS[1].s });
  const preset = () => VPRESETS.find((p) => sameSettings(p.s, s))?.id ?? null;
  const [trim, setTrim] = createSignal<[number, number]>([0, 0]);
  createEffect(on(() => cur()?.path, () => { const c = cur(); if (c) setTrim([0, c.durationMs]); }));
  const trimmed = () => single && !!cur() && (trim()[0] > 0 || trim()[1] < cur()!.durationMs);
  const trimOf = (info: VideoInfo): [number, number] | null => (single && trimmed() && info === cur() ? trim() : null);

  const [name, setName] = createSignal(single ? `${stemOf(props.paths[0])} (compressed)` : " (compressed)");
  const nameError = () => {
    const n = name();
    if (single && !n.trim()) return "Enter a name";
    if (BAD_NAME.test(single ? n.trim() : n)) return "Names can't contain < > : \" / \\ | ? * or end with a dot or space";
    return null;
  };
  const destFor = (p: string) => `${parent(p)!.replace(/\\$/, "")}\\${single ? name().trim() : stemOf(p) + name()}.mp4`;

  const jobFor = (info: VideoInfo): VideoJob => {
    const p = plan(info, s, trimOf(info));
    const t = trimOf(info);
    return {
      source: info.path, dest: destFor(info.path), width: p.sw, height: p.sh, fps: p.fps, videoBitrate: p.vb,
      audioBitrate: p.audio, hevc: s.hevc, trimStartMs: t ? Math.round(t[0]) : undefined, trimEndMs: t ? Math.round(t[1]) : undefined,
    };
  };
  const curPlan = () => (cur() ? plan(cur()!, s, trimOf(cur()!)) : null);
  /** A preset card: the resolution it gives and the size the video (or all of them) would come
   *  out at, so it shows that every preset compresses, and how much. */
  const presetHint = (p: (typeof VPRESETS)[number]) => {
    const list = infos();
    if (!list.length) return p.hint;
    const c = cur() ?? list[0];
    const pl = plan(c, p.s, trimOf(c));
    const res = `${Math.min(pl.ow, pl.oh)}p`;
    if (p.s.mode === "size") return `${res} · ${p.hint}`;
    const before = list.reduce((a, i) => a + i.size, 0);
    const after = list.reduce((a, i) => a + plan(i, p.s, trimOf(i)).bytes, 0);
    return after >= before * 0.9 ? `${res} · about the same size` : `${res} · ≈ ${formatSize(after)}`;
  };

  // ---- player ----
  let vid!: HTMLVideoElement;
  let svid: HTMLVideoElement | undefined;
  let strip!: HTMLCanvasElement;
  let tl!: HTMLDivElement;
  const [time, setTime] = createSignal(0);
  const [playing, setPlaying] = createSignal(false);
  const [muted, setMuted] = createSignal(true);
  const [unplayable, setUnplayable] = createSignal(false);
  const [view, setView] = createSignal<"orig" | "split" | "sample">("orig");
  /** The view picked (Compare after a preview). Outside the sample only the original exists, so
   *  it shows there for a moment, and the picked view comes back inside. */
  const [wanted, setWanted] = createSignal<"orig" | "split" | "sample">("orig");

  const [sample, setSample] = createSignal<(VideoSample & { key: string; atMs: number; source: string }) | null>(null);
  const [sampling, setSampling] = createSignal(false);
  const keyOf = (j: VideoJob) => JSON.stringify({ ...j, dest: "", trimStartMs: 0, trimEndMs: 0 });
  const sampleFresh = () => {
    const sm = sample(), c = cur();
    return !!sm && !!c && sm.source === c.path && sm.key === keyOf(jobFor(c));
  };
  const sampleHere = () => !!sample() && sample()!.source === cur()?.path;
  /** The sample is on screen (alone, or beside the original in the split view). */
  const inSample = () => sampleHere() && view() !== "orig";
  const showingSample = () => sampleHere() && view() === "sample";
  /** Split view: where the divider is, 0–1 across the picture. */
  const [vsplit, setVsplit] = createSignal(0.5);
  const [sampleProgress, setSampleProgress] = createSignal(0);
  onMount(() => {
    const un = listen<number>("media-sample-progress", (e) => setSampleProgress(e.payload));
    onCleanup(() => { un.then((f) => f()); });
  });
  // The sample leads (it loops); the original follows it in the split view.
  const active = () => (inSample() && svid ? svid : vid);

  // ---- quality check (of the latest sample, while it plays) ----
  const [quality, setQuality] = createSignal<{ path: string; busy: boolean; score?: number; ms?: number; shift?: number } | null>(null);
  /** How far the sample is from the original (seconds, from the check): the side-by-side view lines them up by it. */
  const shift = () => { const q = quality(); return q && q.path === sample()?.path ? q.shift ?? 0 : 0; };
  /** The check of the sample on screen (none once the settings changed since it). */
  const ownQuality = () => { const q = quality(); return q && q.path === sample()?.path && sampleFresh() ? q : null; };
  const checking = () => !!ownQuality()?.busy;
  const result = createMemo(() => {
    const q = ownQuality();
    return q && !q.busy && q.score != null ? { ...verdict(q.score), score: q.score, ms: q.ms ?? 0 } : null;
  });
  /** Pauses on the sample's weakest moment, side by side, to look at it closely. */
  const showWorst = () => {
    const v = result();
    if (!v) return;
    svid?.pause();
    vid?.pause();
    if (view() === "orig") switchView("split");
    seek(v.ms);
  };

  // ---- zoom: the picture at actual pixels, to look for blocks and smearing ----
  let screen: HTMLDivElement | undefined;
  const [box, setBox] = createSignal({ w: 1, h: 1 });
  const [zoom, setZoom] = createSignal<number | null>(null);
  const [pan, setPan] = createSignal({ x: 0, y: 0 });
  /** The picture's own size (as shown, turned upright). */
  const nat = () => { const p = curPlan(); return p ? { w: p.dw, h: p.dh } : { w: 16, h: 9 }; };
  const fit = () => Math.min(box().w / nat().w, box().h / nat().h);
  const scale = () => zoom() ?? fit();
  const dims = () => ({ w: nat().w * scale(), h: nat().h * scale() });
  const clampPan = (p: { x: number; y: number }) => {
    const mx = Math.max(0, (dims().w - box().w) / 2), my = Math.max(0, (dims().h - box().h) / 2);
    return { x: clamp(p.x, -mx, mx), y: clamp(p.y, -my, my) };
  };
  const frameLeft = () => box().w / 2 - dims().w / 2 + pan().x;
  const frameTop = () => box().h / 2 - dims().h / 2 + pan().y;
  /** Zooms keeping the point (cx, cy) from the screen's middle in place; null fits the screen.
   *  A small video is fitted larger than its pixels, so its 100% is smaller than "fit". */
  const zoomTo = (next: number | null, cx = 0, cy = 0) => {
    const old = scale();
    if (next != null) next = clamp(next, Math.min(fit(), 1), 8);
    if (next == null || Math.abs(next - fit()) < 1e-3) { setZoom(null); setPan({ x: 0, y: 0 }); return; }
    const p = pan();
    setZoom(next);
    setPan(clampPan({ x: cx - ((cx - p.x) * next) / old, y: cy - ((cy - p.y) * next) / old }));
  };
  createEffect(on(() => cur()?.path, () => { setZoom(null); setPan({ x: 0, y: 0 }); }));
  const attachScreen = (el: HTMLDivElement) => {
    screen = el;
    const ro = new ResizeObserver(() => setBox({ w: el.clientWidth || 1, h: el.clientHeight || 1 }));
    ro.observe(el);
    const wheel = (e: WheelEvent) => {
      if (unplayable() && !inSample()) return;
      e.preventDefault();
      const r = el.getBoundingClientRect();
      zoomTo(clamp(scale() * (e.deltaY < 0 ? 1.2 : 1 / 1.2), 0, 8), e.clientX - r.left - box().w / 2, e.clientY - r.top - box().h / 2);
    };
    el.addEventListener("wheel", wheel, { passive: false });
    onCleanup(() => { ro.disconnect(); el.removeEventListener("wheel", wheel); });
  };
  /** A click plays or pauses; a drag pans the zoomed picture. */
  const onScreenDown = (e: PointerEvent) => {
    const el = screen;
    if (!el || e.button !== 0 || (e.target as HTMLElement).closest(".mx-vsplit, .mx-ab, .mx-zoom, .mx-poster")) return;
    const start = { x: e.clientX, y: e.clientY, pan: pan() };
    let moved = false;
    el.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      if (Math.abs(ev.clientX - start.x) + Math.abs(ev.clientY - start.y) > 4) moved = true;
      if (moved && zoom() != null) setPan(clampPan({ x: start.pan.x + ev.clientX - start.x, y: start.pan.y + ev.clientY - start.y }));
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      if (!moved) togglePlay();
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  };
  const onScreenDbl = (e: MouseEvent) => {
    if (!screen || (e.target as HTMLElement).closest(".mx-vsplit, .mx-ab, .mx-zoom, .mx-poster")) return;
    const r = screen.getBoundingClientRect();
    zoomTo(zoom() == null ? 1 : null, e.clientX - r.left - box().w / 2, e.clientY - r.top - box().h / 2);
  };

  // ---- full screen: the picture alone, as it would be watched ----
  const [full, setFull] = createSignal(false);
  const toggleFull = () => {
    if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
    else screen?.requestFullscreen().catch(() => {});
  };
  onMount(() => {
    const change = () => setFull(!!screen && document.fullscreenElement === screen);
    document.addEventListener("fullscreenchange", change);
    onCleanup(() => {
      document.removeEventListener("fullscreenchange", change);
      if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
    });
  });

  let raf = 0;
  const tick = () => {
    const sm = sample();
    if (inSample() && svid && sm) {
      setTime(sm.atMs + svid.currentTime * 1000);
      if (view() === "split" && vid && !unplayable()) {
        const target = sm.atMs / 1000 + svid.currentTime + shift();
        if (Math.abs(vid.currentTime - target) > 0.12) vid.currentTime = target;
        if (svid.paused !== vid.paused) svid.paused ? vid.pause() : vid.play().catch(() => {});
      }
    } else if (vid) {
      setTime(vid.currentTime * 1000);
      // Playback stays inside the trimmed part.
      if (!vid.paused && single && vid.currentTime * 1000 >= trim()[1] - 30) vid.currentTime = trim()[0] / 1000;
    }
    raf = requestAnimationFrame(tick);
  };
  onMount(() => (raf = requestAnimationFrame(tick)));
  onCleanup(() => cancelAnimationFrame(raf));

  const seek = (ms: number) => {
    const c = cur();
    if (!c) return;
    ms = clamp(ms, 0, c.durationMs);
    const sm = sample();
    // Inside the sample, the comparison shows; outside it, only the original exists.
    let within = false;
    if (sampleHere() && sm && ms >= sm.atMs && ms <= sm.atMs + sm.durationMs && svid) {
      if (view() === "orig" && wanted() !== "orig") setView(wanted());
      svid.currentTime = Math.min((ms - sm.atMs) / 1000, sm.durationMs / 1000 - 0.05);
      within = view() !== "orig";
    } else if (inSample()) {
      svid?.pause();
      setView("orig");
    }
    if (vid) vid.currentTime = ms / 1000 + (within ? shift() : 0);
    setTime(ms);
  };
  const togglePlay = () => {
    const v = active();
    if (!v || (unplayable() && !inSample())) return;
    if (v.paused) {
      if (!inSample() && single && (vid.currentTime * 1000 < trim()[0] || vid.currentTime * 1000 >= trim()[1] - 50)) vid.currentTime = trim()[0] / 1000;
      v.play().catch(() => {});
    } else {
      v.pause();
      vid?.pause();
    }
  };
  const switchView = (to: "orig" | "split" | "sample") => {
    const sm = sample();
    if (!sm || to === view()) return;
    const wasPlaying = !active().paused;
    const t = time();
    svid?.pause();
    vid?.pause();
    setView(to);
    setWanted(to);
    if (to !== "orig" && svid) {
      svid.currentTime = clamp((t - sm.atMs) / 1000, 0, sm.durationMs / 1000 - 0.05);
      if (to === "split") vid.currentTime = sm.atMs / 1000 + svid.currentTime + shift();
    } else vid.currentTime = t / 1000;
    if (wasPlaying) queueMicrotask(() => active().play().catch(() => {}));
  };
  const onSplitDown = (e: PointerEvent) => {
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    const box = el.parentElement!.getBoundingClientRect();
    el.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => setVsplit(clamp((ev.clientX - box.left) / box.width, 0, 1));
    const up = () => { el.removeEventListener("pointermove", move); el.removeEventListener("pointerup", up); };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  };
  const setStart = () => { const t = time(); setTrim(([, e]) => [Math.min(t, e - 500), e]); };
  const setEnd = () => { const t = time(); setTrim(([b]) => [b, Math.max(t, b + 500)]); };

  /** Counts preview requests: a newer one replaces the one being encoded. */
  let sampleSeq = 0;
  const makeSample = async () => {
    const c = cur();
    if (!c) return;
    const j = jobFor(c);
    const lo = single ? trim()[0] : 0, hi = single ? trim()[1] : c.durationMs;
    const at = Math.round(clamp(time(), lo, Math.max(lo, hi - SAMPLE_MS)));
    const my = ++sampleSeq;
    setSampleProgress(0);
    setSampling(true);
    try {
      const r = await api.videoSample(j, at, SAMPLE_MS);
      if (r) {
        svid?.pause();
        vid?.pause();
        setSample({ ...r, key: keyOf(j), atMs: at, source: c.path });
        setView("split");
        setWanted("split");
        // Checked while it plays: a sample replaced in the meantime stops the check.
        const path = r.path;
        setQuality({ path, busy: true });
        checkSample(fileUrl(c.path), fileUrl(path), at, r.durationMs, plan(c, s, trimOf(c)).fps || c.fps || 30, c.fps || 30, () => sample()?.path !== path)
          .then((q) => {
            if (sample()?.path !== path) return;
            setQuality(q ? { path, busy: false, ...q } : null);
            // Now that the shift is known, the two halves show the same moment.
            if (q && svid && vid && view() === "split") vid.currentTime = at / 1000 + svid.currentTime + q.shift;
          });
        // Both start together at the sample's first frame.
        queueMicrotask(() => {
          if (!svid) return;
          svid.currentTime = 0;
          vid.currentTime = at / 1000;
          svid.play().catch(() => {});
        });
      }
    } catch (e) {
      if (my === sampleSeq) toast(`Couldn't make a preview: ${e}`, "error");
    } finally {
      if (my === sampleSeq) setSampling(false);
    }
  };
  /** Letting go of the timeline outside the sample while comparing: the comparison follows, with
   *  a new sample there (one only covers its own few seconds). */
  const followWithSample = () => {
    const sm = sample(), c = cur();
    if (!sm || !c || sm.source !== c.path || wanted() === "orig") return;
    const t = time();
    if (t >= sm.atMs && t <= sm.atMs + sm.durationMs) return;
    void makeSample();
  };

  // Frames along the timeline, once per video.
  createEffect(on(() => cur()?.path, (path) => {
    if (!path || !strip) return;
    let stopped = false;
    strip.getContext("2d")?.clearRect(0, 0, strip.width, strip.height);
    const c = cur()!;
    requestAnimationFrame(() => drawFilmstrip(fileUrl(path), strip, c.durationMs, () => stopped));
    onCleanup(() => (stopped = true));
  }));

  // Dragging on the timeline: the handles move the trim points, anywhere else seeks.
  const msAt = (clientX: number) => {
    const r = tl.getBoundingClientRect();
    return clamp((clientX - r.left) / r.width, 0, 1) * (cur()?.durationMs ?? 0);
  };
  const onTimelineDown = (e: PointerEvent) => {
    const c = cur();
    if (!c || e.button !== 0) return;
    const r = tl.getBoundingClientRect();
    const px = (ms: number) => r.left + (ms / c.durationMs) * r.width;
    const [a, b] = trim();
    const what = single && Math.abs(e.clientX - px(a)) < 9 ? "start" : single && Math.abs(e.clientX - px(b)) < 9 ? "end" : "seek";
    tl.setPointerCapture(e.pointerId);
    vid?.pause();
    const move = (ev: PointerEvent) => {
      const ms = msAt(ev.clientX);
      if (what === "start") setTrim(([, end]) => [Math.min(ms, end - 500), end]);
      else if (what === "end") setTrim(([st]) => [st, Math.max(ms, st + 500)]);
      seek(what === "start" ? trim()[0] : what === "end" ? trim()[1] : ms);
    };
    move(e);
    const up = () => {
      tl.removeEventListener("pointermove", move);
      tl.removeEventListener("pointerup", up);
      if (what === "seek") followWithSample();
    };
    tl.addEventListener("pointermove", move);
    tl.addEventListener("pointerup", up);
  };

  let followTimer: number | undefined;
  onCleanup(() => clearTimeout(followTimer));
  const onKey = (e: KeyboardEvent) => {
    const k = e.key.toLowerCase();
    if (k === " ") { e.preventDefault(); togglePlay(); }
    else if (k === "i" && single) { e.preventDefault(); setStart(); }
    else if (k === "o" && single) { e.preventDefault(); setEnd(); }
    else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      const step = e.shiftKey ? 5000 : e.altKey ? 1000 / 30 : 1000;
      seek(time() + (e.key === "ArrowLeft" ? -step : step));
      // Once the keys stop, a comparison follows like it does after a drag on the timeline.
      clearTimeout(followTimer);
      followTimer = window.setTimeout(followWithSample, 600);
    } else if (k === "m") { e.preventDefault(); setMuted(!muted()); }
    else if (k === "f") { e.preventDefault(); toggleFull(); }
  };

  // ---- totals ----
  const totals = createMemo(() => {
    const list = infos();
    let before = 0, after = 0, measured = false;
    for (const info of list) {
      before += info.size;
      const p = plan(info, s, trimOf(info));
      const sm = sample();
      if (sm && info === cur() && sampleFresh()) {
        after += (sm.bytes / sm.durationMs) * p.durMs + 16_000;
        measured = true;
      } else after += p.bytes;
    }
    return { before, after, measured };
  });

  const submit = () => {
    const list = infos();
    if (!list.length || nameError()) return;
    const items = list.map(jobFor);
    const before = list.reduce((a, i) => a + i.size, 0);
    closeMediaEditor();
    compressMedia("video", api.compressVideos(items), items.map((i) => i.source), before);
  };

  const pct = (ms: number) => `${(ms / (cur()?.durationMs || 1)) * 100}%`;
  const short = () => (cur() ? Math.min(plan(cur()!, s, null).dw, plan(cur()!, s, null).dh) : 0);

  const stage = (
    <Show when={!results.loading} fallback={<div class="mx-center"><Loader2 class="spin" size={22} /><span>Reading video…</span></div>}>
      <Show when={cur()} fallback={
        <div class="mx-center err"><AlertTriangle size={26} /><span>{errors()[0]?.error ?? "Couldn't read this video"}</span></div>
      }>
        {(c) => (
          <div class="mx-video">
            <Pager paths={infos().map((i) => i.path)} idx={idx()} onIdx={(i) => { setSample(null); setView("orig"); setWanted("orig"); setIdx(i); }} />
            <div class="mx-screen" ref={attachScreen} classList={{ zoomed: zoom() != null, pixel: scale() >= 2 }}
              onPointerDown={onScreenDown} onDblClick={onScreenDbl}>
              <div class="mx-vframe" style={{ left: `${frameLeft()}px`, top: `${frameTop()}px`, width: `${dims().w}px`, height: `${dims().h}px` }}>
                {/* In the split view the sound comes from the original only. */}
                <video ref={vid} src={fileUrl(c().path)} preload="auto" muted={muted() || showingSample()} playsinline
                  classList={{ hidden: showingSample() || unplayable() }}
                  onPlay={() => !inSample() && setPlaying(true)} onPause={() => !inSample() && setPlaying(false)}
                  onLoadedData={() => setUnplayable(false)} onError={() => setUnplayable(true)} />
                <Show when={sample()}>
                  {(sm) => (
                    <video ref={svid} src={fileUrl(sm().path)} loop playsinline muted={muted() || view() === "split"}
                      classList={{ hidden: !inSample() }}
                      style={{ "clip-path": view() === "split" ? `inset(0 0 0 ${clamp(vsplit() * box().w - frameLeft(), 0, dims().w)}px)` : "none" }}
                      onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} />
                  )}
                </Show>
              </div>
              <Show when={sampleHere() && view() === "split"}>
                <div class="mx-vsplit" style={{ left: `${vsplit() * 100}%` }} onPointerDown={onSplitDown} onClick={(e) => e.stopPropagation()}>
                  <span class="mx-knob"><ChevronLeft size={13} /><ChevronRight size={13} /></span>
                </div>
                <span class="mx-tag left bottom">Original</span>
                <span class="mx-tag right bottom">Compressed</span>
              </Show>
              <Show when={unplayable()}>
                <div class="mx-poster">
                  <img src={thumbUrl(c().path, 512)} alt="" onError={(e) => (e.currentTarget.style.display = "none")} />
                  <span>This format can't play here, but it can still be compressed.</span>
                </div>
              </Show>
              <Show when={sample() && sample()!.source === c().path}>
                <div class="mx-ab">
                  <Seg value={view()} onChange={switchView}
                    options={[{ value: "orig", label: "Original" }, { value: "split", label: "Compare" }, { value: "sample", label: "Compressed" }]} />
                  <Show when={inSample()}>
                    <span class="mx-ab-note" classList={{ stale: !sampleFresh() }}>
                      {sampleFresh() ? `${SAMPLE_MS / 1000}-second sample from ${fmtTime(sample()!.atMs, false)}` : "Settings changed since this sample"}
                    </span>
                  </Show>
                  <Show when={sampleHere() && view() === "orig" && wanted() !== "orig"}>
                    <span class="mx-ab-note">Outside the {SAMPLE_MS / 1000}-second sample: the original, until one is made here</span>
                  </Show>
                  <Show when={checking()}>
                    <span class="mx-ab-note"><Loader2 class="spin" size={11} /> Checking quality…</span>
                  </Show>
                  <Show when={result()}>
                    {(r) => (
                      <button class="mx-quality" data-level={r().level} onClick={showWorst}
                        title={`Compared frame by frame with the original (${SAMPLE_FRAMES} frames). Its weakest moment, at ${fmtTime(r().ms)}, scores ${r().score.toFixed(3)} (1 is identical): click to pause there side by side.`}>
                        <Show when={r().level === "same" || r().level === "good"} fallback={<AlertTriangle size={12} />}><CircleCheck size={12} /></Show>
                        {r().text}
                      </button>
                    )}
                  </Show>
                </div>
              </Show>
              <Show when={!unplayable() || inSample()}>
                <div class="mx-zoom mx-vzoom">
                  <button classList={{ on: zoom() == null }} onClick={() => zoomTo(null)} title="Fit the screen">Fit</button>
                  <button classList={{ on: zoom() === 1 }} onClick={() => zoomTo(1)} title="Actual pixels, to look for blocks and smearing (double-click; scroll to zoom)">100%</button>
                  <span>{Math.round(scale() * 100)}%</span>
                  <span class="mx-zoom-sep" />
                  <button onClick={toggleFull} title={full() ? "Leave full screen (F or Esc)" : "Full screen (F)"}>
                    <Show when={full()} fallback={<Maximize2 size={13} />}><Minimize2 size={13} /></Show>
                  </button>
                </div>
              </Show>
              <Show when={sampling()}>
                <div class="mx-busy">
                  <Loader2 class="spin" size={16} /> Encoding a {SAMPLE_MS / 1000}-second sample… <b>{Math.round(sampleProgress() * 100)}%</b>
                  <span class="mx-busy-bar"><span style={{ width: `${sampleProgress() * 100}%` }} /></span>
                </div>
              </Show>
            </div>
            <div class="mx-transport">
              <button class="icon-btn" onClick={togglePlay} disabled={unplayable()} title={playing() ? "Pause (Space)" : "Play (Space)"}>
                <Show when={playing()} fallback={<Play size={16} fill="currentColor" />}><Pause size={16} fill="currentColor" /></Show>
              </button>
              <button class="icon-btn" onClick={() => setMuted(!muted())} title={muted() ? "Sound on (M)" : "Sound off (M)"}>
                <Show when={muted()} fallback={<Volume2 size={16} />}><VolumeX size={16} /></Show>
              </button>
              <span class="mx-time">{fmtTime(time())} <span class="muted">/ {fmtTime(c().durationMs)}</span></span>
              <span class="mx-grow" />
              <button class="btn mx-sample-btn" classList={{ fresh: sampleFresh() }} disabled={sampling()} onClick={makeSample}
                title={`Encode ${SAMPLE_MS / 1000} seconds from the playhead with these settings, to compare, check the quality and measure the size. Fast motion is the hardest to compress: put the playhead there.`}>
                <Sparkles size={14} /> {sampleFresh() ? "Preview again" : "Preview quality"}
              </button>
            </div>
            <div class="mx-tl" ref={tl} onPointerDown={onTimelineDown} classList={{ trimmable: single }}>
              <canvas class="mx-strip" ref={strip} />
              <Show when={single}>
                <div class="mx-tl-dim" style={{ left: 0, width: pct(trim()[0]) }} />
                <div class="mx-tl-dim" style={{ left: pct(trim()[1]), right: 0 }} />
                <div class="mx-tl-range" style={{ left: pct(trim()[0]), width: pct(trim()[1] - trim()[0]) }}>
                  <span class="mx-handle start" title="Drag to trim the start (I)" />
                  <span class="mx-handle end" title="Drag to trim the end (O)" />
                </div>
              </Show>
              <Show when={sample() && sample()!.source === c().path}>
                <div class="mx-tl-sample" classList={{ stale: !sampleFresh() }} style={{ left: pct(sample()!.atMs), width: pct(sample()!.durationMs) }} />
              </Show>
              <div class="mx-playhead" style={{ left: pct(time()) }} />
            </div>
            <Show when={single}>
              <div class="mx-trimbar">
                <span class="mx-trim-text">
                  <Show when={trimmed()} fallback={<span class="muted">Drag the ends of the timeline to trim · I / O set them at the playhead</span>}>
                    Keeping <b>{fmtTime(trim()[0])}</b> – <b>{fmtTime(trim()[1])}</b> <span class="muted">({fmtTime(trim()[1] - trim()[0])})</span>
                  </Show>
                </span>
                <button class="btn small" onClick={setStart} title="Start here (I)">Start here</button>
                <button class="btn small" onClick={setEnd} title="End here (O)">End here</button>
                <Show when={trimmed()}>
                  <button class="btn small" onClick={() => setTrim([0, c().durationMs])} title="Keep the whole video"><RotateCcw size={13} /> Reset</button>
                </Show>
              </div>
            </Show>
          </div>
        )}
      </Show>
    </Show>
  );

  const side = (
    <Show when={cur()}>
      {(c) => (
        <>
          <Section title="Preset">
            <Presets items={VPRESETS} current={preset()} onPick={(p) => setS({ ...p.s })} hintOf={presetHint} />
          </Section>
          <Section title="Size" aside={<Seg value={s.mode} onChange={(v) => setS("mode", v)} options={[{ value: "quality", label: "By quality" }, { value: "size", label: "By file size" }]} />}>
            <Show when={s.mode === "quality"} fallback={
              <>
                <div class="mx-target">
                  <input class="text-input" type="number" min="1" step="1" value={s.targetMb}
                    onInput={(e) => { const v = +e.currentTarget.value; if (v > 0) setS("targetMb", v); }} />
                  <span class="muted">MB</span>
                  <For each={[8, 10, 25, 50]}>
                    {(mb) => <button class="mx-chip" classList={{ on: s.targetMb === mb }} onClick={() => setS("targetMb", mb)}>{mb}</button>}
                  </For>
                </div>
                <Show when={curPlan()?.tooSmall}
                  fallback={<p class="mx-hint">{single ? "" : "Each video at most this size. "}Bitrate {((curPlan()?.vb ?? 0) / 1e6).toFixed(2)} Mbps for {fmtTime(curPlan()?.durMs ?? 0, false)}{single ? "" : " (this one)"}</p>}>
                  <p class="mx-hint warn"><AlertTriangle size={13} /> Too small for {fmtTime(curPlan()!.durMs, false)} of video. Trim it, lower the resolution, or allow more MB.</p>
                </Show>
              </>
            }>
              <div class="mx-slider">
                <input type="range" min="0" max="100" value={s.quality} onInput={(e) => setS("quality", +e.currentTarget.value)} />
                <span class="mx-slider-val">{qualityLabel(s.quality)}</span>
              </div>
              <p class="mx-hint">{((curPlan()?.vb ?? 0) / 1e6).toFixed(2)} Mbps video{c().videoBitrate ? ` · original ${(c().videoBitrate / 1e6).toFixed(1)} Mbps` : ""}</p>
            </Show>
          </Section>
          <Section title="Resolution">
            <select class="mx-select" value={s.res && s.res < short() ? s.res : 0} onChange={(e) => setS("res", +e.currentTarget.value)}>
              <option value={0}>Keep {curPlan()?.dw} × {curPlan()?.dh}</option>
              <For each={RESOLUTIONS.filter((r) => r < short())}>
                {(r) => {
                  const p = () => plan(c(), { ...s, res: r }, null);
                  return <option value={r}>{r}p · {p().ow} × {p().oh}</option>;
                }}
              </For>
            </select>
          </Section>
          <Section title="Frame rate">
            <Seg value={curPlan()?.fps ?? 0} onChange={(v) => setS("fps", v)}
              options={[{ value: 0, label: c().fps ? `Keep ${Math.round(c().fps)} fps` : "Keep" }, ...[30, 24].filter((f) => f < c().fps - 0.5).map((f) => ({ value: f, label: `${f} fps` }))]} />
          </Section>
          <Section title="Format">
            <Seg value={s.hevc} onChange={(v) => setS("hevc", v)}
              options={[{ value: false, label: "H.264", title: "Plays everywhere" }, { value: true, label: "HEVC", title: "About 40% smaller at the same quality; some older devices and sites can't play it" }]} />
            <p class="mx-hint">{s.hevc ? "Smaller files; some older devices and websites can't play HEVC." : "MP4 that plays on every device and website."}</p>
          </Section>
          <Section title="Sound">
            <Show when={c().hasAudio} fallback={<p class="mx-hint">This video has no sound.</p>}>
              <div class="mx-row">
                <Seg value={s.audio > 0} onChange={(v) => setS("audio", v ? 128 : 0)} options={[{ value: true, label: "Keep" }, { value: false, label: "Remove" }]} />
                <Show when={s.audio > 0}>
                  <select class="mx-select small" value={s.audio} onChange={(e) => setS("audio", +e.currentTarget.value)}>
                    <For each={[64, 96, 128, 160, 192]}>{(k) => <option value={k}>{k} kbps</option>}</For>
                  </select>
                </Show>
              </div>
            </Show>
          </Section>
          <NameField single={single} value={name()} ext=".mp4" onInput={setName} error={nameError()}
            example={`${stemOf(props.paths[0])}${name()}.mp4`} />
          <Show when={errors().length}>
            <p class="mx-hint warn"><AlertTriangle size={13} /> {plural(errors().length, "video")} can't be read and will be skipped: {errors().map((e) => basename(e.path)).join(", ")}</p>
          </Show>
        </>
      )}
    </Show>
  );

  return (
    <Shell icon={Clapperboard} title={single ? "Compress video" : `Compress ${props.paths.length} videos`}
      sub={cur() ? `${basename(cur()!.path)} · ${cur()!.codec} · ${curPlan()!.dw} × ${curPlan()!.dh} · ${Math.round(cur()!.fps)} fps · ${fmtTime(cur()!.durationMs, false)}` : basename(props.paths[0])}
      stage={stage} side={side} onKey={onKey}
      canSubmit={infos().length > 0 && !nameError()} onSubmit={submit}
      summary={
        <Summary before={totals().before} after={infos().length ? totals().after : null} approx
          note={totals().measured ? "Measured from the preview sample" : infos().length ? "Estimate · “Preview quality” measures it" : ""} />
      } />
  );
}

// ---------- pictures ----------

/** "auto": PNG for pictures with transparency, JPEG for the rest (decided per picture). */
interface ISettings { fmt: "auto" | "jpeg" | "png"; quality: number; maxSide: number }

const IPRESETS: { id: string; label: string; hint: string; s: Omit<ISettings, "fmt"> }[] = [
  { id: "balanced", label: "Balanced", hint: "2560 px · 82%", s: { quality: 82, maxSide: 2560 } },
  { id: "web", label: "Web", hint: "1920 px · 78%", s: { quality: 78, maxSide: 1920 } },
  { id: "small", label: "Small", hint: "1280 px · 70%", s: { quality: 70, maxSide: 1280 } },
  { id: "best", label: "Best quality", hint: "Full size · 92%", s: { quality: 92, maxSide: 0 } },
];
const SIDES = [3840, 2560, 1920, 1600, 1280, 1024, 800];

function ImageEditor(props: { paths: string[] }) {
  const single = props.paths.length === 1;
  const [idx, setIdx] = createSignal(0);
  const path = () => props.paths[idx()];
  const [info] = createResource(path, (p) => api.imageInfo(p));
  const [all] = createResource(() => (single ? null : props.paths), (ps) => api.folderStats(ps));
  const [s, setS] = createStore<ISettings>({ fmt: "auto", quality: 82, maxSide: 2560 });
  /** Whether the picture on screen comes out as PNG. */
  const png = () => s.fmt === "png" || (s.fmt === "auto" && !!info()?.alpha);
  const opts = () => ({ png: s.fmt === "png", auto: s.fmt === "auto", quality: s.quality, maxSide: s.maxSide });
  const preset = () => IPRESETS.find((p) => p.s.quality === s.quality && p.s.maxSide === s.maxSide)?.id ?? null;

  const outSize = () => {
    const i = info();
    if (!i) return null;
    const long = Math.max(i.width, i.height);
    const k = s.maxSide && long > s.maxSide ? s.maxSide / long : 1;
    return { w: Math.max(1, Math.round(i.width * k)), h: Math.max(1, Math.round(i.height * k)) };
  };

  const afterKey = settled(() => (info() ? { path: info()!.path, ...opts() } : null), 180);
  const [after] = createResource(afterKey, (k) => (k ? api.imagePreview(k.path, k) : Promise.resolve(null)));
  // The original at the same size, near-lossless, so zooming in compares like for like.
  const beforeKey = settled(() => (info() ? { path: info()!.path, maxSide: s.maxSide, alpha: info()!.alpha } : null), 180);
  const [before] = createResource(beforeKey, (k) => (k ? api.imagePreview(k.path, { png: k.alpha, auto: false, quality: 100, maxSide: k.maxSide }) : Promise.resolve(null)));
  createEffect(() => { if (after.error) toast(String(after.error), "error"); });

  const [name, setName] = createSignal(single ? `${stemOf(props.paths[0])} (compressed)` : " (compressed)");
  // With "Auto" the backend sets each picture's extension to match its format.
  const ext = () => (!single && s.fmt === "auto" ? ".jpg / .png" : png() ? ".png" : ".jpg");
  const nameError = () => {
    const n = name();
    if (single && !n.trim()) return "Enter a name";
    if (BAD_NAME.test(single ? n.trim() : n)) return "Names can't contain < > : \" / \\ | ? * or end with a dot or space";
    return null;
  };
  const destFor = (p: string) => `${parent(p)!.replace(/\\$/, "")}\\${single ? name().trim() : stemOf(p) + name()}${png() && single ? ".png" : ".jpg"}`;

  const fresh = () => {
    const a = after.latest, k = afterKey(), o = opts();
    return !!a && !!k && !after.loading && k.png === o.png && k.auto === o.auto && k.quality === o.quality && k.maxSide === o.maxSide;
  };
  const totals = () => {
    const i = info(), a = after.latest;
    if (!i) return { before: 0, after: null as number | null, approx: false };
    // Pictures differ too much to extrapolate a batch from one, so it shows the one on screen.
    return { before: i.size, after: a?.bytes ?? null, approx: !fresh() };
  };

  const submit = () => {
    if (nameError() || !info()) return;
    const items = props.paths.map((p) => ({ source: p, dest: destFor(p) }));
    const before = single ? totals().before : all()?.bytes ?? 0;
    closeMediaEditor();
    compressMedia("image", api.compressImages(items, opts()), props.paths, before);
  };

  const stage = (
    <div class="mx-image">
      <Pager paths={props.paths} idx={idx()} onIdx={setIdx} />
      <Show when={!info.error} fallback={<div class="mx-center err"><AlertTriangle size={26} /><span>{String(info.error)}</span></div>}>
        <Show when={info() && outSize()} fallback={<div class="mx-center"><Loader2 class="spin" size={22} /><span>Opening picture…</span></div>}>
          <Compare before={before.latest?.dataUrl ?? info()!.preview} after={after.latest?.dataUrl} w={outSize()!.w} h={outSize()!.h}
            busy={after.loading} alpha={info()!.alpha}
            left={`Original · ${formatSize(info()!.size)}`}
            right={after.latest ? `${png() ? "PNG" : `JPEG ${s.quality}%`} · ${formatSize(after.latest.bytes)}` : "Compressed"} />
        </Show>
      </Show>
    </div>
  );

  const side = (
    <Show when={info()}>
      {(i) => {
        const long = () => Math.max(i().width, i().height);
        return (
          <>
            <Section title="Preset">
              <Presets items={IPRESETS} current={preset()} onPick={(p) => setS({ ...p.s })} />
            </Section>
            <Section title="Format">
              <Seg value={s.fmt} onChange={(v) => setS("fmt", v)}
                options={[
                  { value: "auto", label: "Auto", title: "PNG for pictures with transparency, JPEG for the rest" },
                  { value: "jpeg", label: "JPEG", title: "Much smaller; best for photos" },
                  { value: "png", label: "PNG", title: "Lossless; keeps transparency" },
                ]} />
              <p class="mx-hint">
                {s.fmt === "auto" ? (single ? `${i().alpha ? "PNG: this picture has transparency" : "JPEG: no transparency to keep"}.` : "Each picture: PNG when it has transparency, otherwise JPEG.")
                  : s.fmt === "png" ? "Lossless: only resizing makes it smaller." : "Much smaller; best for photos and screenshots."}
              </p>
              <Show when={s.fmt === "jpeg" && i().alpha}><p class="mx-hint warn"><AlertTriangle size={13} /> Transparent parts become white in JPEG.</p></Show>
            </Section>
            <Show when={s.fmt !== "png"}>
              <Section title="Quality" aside={<span class="mx-val">{s.quality}%</span>}>
                <div class="mx-slider">
                  <input type="range" min="10" max="100" value={s.quality} onInput={(e) => setS("quality", +e.currentTarget.value)} />
                </div>
                <p class="mx-hint">{s.quality >= 90 ? "Visually the same as the original" : s.quality >= 75 ? "Hard to tell apart; much smaller" : s.quality >= 55 ? "Small; fine detail softens" : "Smallest; visible blockiness"}</p>
              </Section>
            </Show>
            <Section title="Size">
              <select class="mx-select" value={s.maxSide && s.maxSide < long() ? s.maxSide : 0} onChange={(e) => setS("maxSide", +e.currentTarget.value)}>
                <option value={0}>Original · {i().width} × {i().height}</option>
                <For each={SIDES.filter((x) => x < long())}>
                  {(x) => {
                    const k = x / long();
                    return <option value={x}>{x} px · {Math.round(i().width * k)} × {Math.round(i().height * k)}</option>;
                  }}
                </For>
              </select>
              <p class="mx-hint">Longest side. Phones and most screens show 1920–2560 px.</p>
            </Section>
            <NameField single={single} value={name()} ext={ext()} onInput={setName} error={nameError()}
              example={`${stemOf(path())}${name()}${png() ? ".png" : ".jpg"}`} />
            <p class="mx-hint mx-privacy">Location, camera and other details stored in the file aren't copied.</p>
          </>
        );
      }}
    </Show>
  );

  return (
    <Shell icon={ImageDown} title={single ? "Compress picture" : `Compress ${props.paths.length} pictures`}
      sub={info() ? `${basename(info()!.path)} · ${info()!.format} · ${info()!.width} × ${info()!.height}` : basename(path())}
      stage={stage} side={side} canSubmit={!!info() && !nameError()} onSubmit={submit}
      summary={
        <Summary before={totals().before} after={totals().after} approx={totals().approx}
          note={!outSize() ? "" : `${single ? "" : `This picture (${idx() + 1} of ${props.paths.length}) · `}${outSize()!.w} × ${outSize()!.h} px · exact size`} />
      } />
  );
}

/** Paints how much each pixel of `after` differs from `before`: a dimmed grey copy of the
 *  original with the changes glowing on top (amplified ×8, so mild JPEG loss is visible). */
async function drawDifferences(before: string, after: string, canvas: HTMLCanvasElement) {
  const load = (src: string) => new Promise<HTMLImageElement>((res, rej) => {
    const i = new Image();
    i.onload = () => res(i);
    i.onerror = rej;
    i.src = src;
  });
  const [a, b] = await Promise.all([load(before), load(after)]);
  const w = b.naturalWidth, h = b.naturalHeight;
  const work = document.createElement("canvas");
  work.width = w;
  work.height = h;
  const x = work.getContext("2d", { willReadFrequently: true })!;
  x.drawImage(a, 0, 0, w, h);
  const pa = x.getImageData(0, 0, w, h).data;
  x.clearRect(0, 0, w, h);
  x.drawImage(b, 0, 0, w, h);
  const out = x.getImageData(0, 0, w, h);
  const pb = out.data;
  for (let i = 0; i < pb.length; i += 4) {
    const d = (Math.abs(pa[i] - pb[i]) + Math.abs(pa[i + 1] - pb[i + 1]) + Math.abs(pa[i + 2] - pb[i + 2])) * (8 / 3);
    const grey = (pa[i] * 0.3 + pa[i + 1] * 0.59 + pa[i + 2] * 0.11) * 0.22;
    pb[i] = Math.min(255, grey + d);
    pb[i + 1] = Math.min(255, grey + d * 0.55);
    pb[i + 2] = Math.min(255, grey + d * 0.15);
    pb[i + 3] = 255;
  }
  canvas.width = w;
  canvas.height = h;
  canvas.getContext("2d")!.putImageData(out, 0, 0);
}

/** Before/after slider: drag to compare; wheel zooms at the pointer, drag pans when zoomed, double-click toggles 100%. */
function Compare(props: { before?: string; after?: string; w: number; h: number; busy: boolean; alpha: boolean; left: string; right: string }) {
  let stage!: HTMLDivElement;
  const [box, setBox] = createSignal({ w: 1, h: 1 });
  const [zoom, setZoom] = createSignal<number | null>(null);
  const [pan, setPan] = createSignal({ x: 0, y: 0 });
  const [split, setSplit] = createSignal(0.5);
  const fit = () => Math.min((box().w - 24) / props.w, (box().h - 24) / props.h, 1);
  const scale = () => zoom() ?? fit();
  const dims = () => ({ w: props.w * scale(), h: props.h * scale() });
  const clampPan = (p: { x: number; y: number }) => {
    const mx = Math.max(0, (dims().w - box().w) / 2 + 12), my = Math.max(0, (dims().h - box().h) / 2 + 12);
    return { x: clamp(p.x, -mx, mx), y: clamp(p.y, -my, my) };
  };
  const left = () => box().w / 2 - dims().w / 2 + pan().x;
  const top = () => box().h / 2 - dims().h / 2 + pan().y;

  const zoomTo = (next: number | null, cx = 0, cy = 0) => {
    const old = scale();
    if (next == null || next <= fit() + 1e-3) { setZoom(null); setPan({ x: 0, y: 0 }); return; }
    const p = pan();
    setZoom(next);
    setPan(clampPan({ x: cx - ((cx - p.x) * next) / old, y: cy - ((cy - p.y) * next) / old }));
  };
  // A new output size (resize setting) starts from "fit" again.
  createEffect(on(() => [props.w, props.h], () => { setZoom(null); setPan({ x: 0, y: 0 }); }, { defer: true }));

  onMount(() => {
    const ro = new ResizeObserver(() => setBox({ w: stage.clientWidth, h: stage.clientHeight }));
    ro.observe(stage);
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      const r = stage.getBoundingClientRect();
      zoomTo(clamp(scale() * (e.deltaY < 0 ? 1.2 : 1 / 1.2), 0, 8), e.clientX - r.left - box().w / 2, e.clientY - r.top - box().h / 2);
    };
    stage.addEventListener("wheel", wheel, { passive: false });
    onCleanup(() => { ro.disconnect(); stage.removeEventListener("wheel", wheel); });
  });

  const onDown = (e: PointerEvent) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".mx-zoom")) return;
    const onHandle = !!(e.target as HTMLElement).closest(".mx-split");
    const r = stage.getBoundingClientRect();
    const panning = zoom() != null && !onHandle;
    const start = { x: e.clientX, y: e.clientY, pan: pan() };
    stage.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      if (panning) setPan(clampPan({ x: start.pan.x + ev.clientX - start.x, y: start.pan.y + ev.clientY - start.y }));
      else setSplit(clamp((ev.clientX - r.left) / r.width, 0, 1));
    };
    if (!panning) move(e);
    const up = () => { stage.removeEventListener("pointermove", move); stage.removeEventListener("pointerup", up); };
    stage.addEventListener("pointermove", move);
    stage.addEventListener("pointerup", up);
  };

  // "Differences": where the compressed picture departs from the original, brightened so
  // even small JPEG changes show (at "Fit" zoom they are otherwise invisible).
  const [diff, setDiff] = createSignal(false);
  const [diffBusy, setDiffBusy] = createSignal(false);
  let diffCanvas!: HTMLCanvasElement;
  createEffect(() => {
    const a = props.before, b = props.after;
    if (!diff() || !a || !b) return;
    let stale = false;
    onCleanup(() => (stale = true));
    setDiffBusy(true);
    drawDifferences(a, b, diffCanvas).catch(() => {}).finally(() => !stale && setDiffBusy(false));
  });

  const splitPx = () => split() * box().w;
  return (
    <div class="mx-compare" ref={stage} onPointerDown={onDown} classList={{ zoomed: zoom() != null }}
      onDblClick={(e) => {
        if ((e.target as HTMLElement).closest(".mx-zoom")) return;
        const r = stage.getBoundingClientRect();
        zoomTo(zoom() == null ? 1 : null, e.clientX - r.left - box().w / 2, e.clientY - r.top - box().h / 2);
      }}>
      <div class="mx-img" classList={{ pixel: scale() >= 2, checker: props.alpha }}
        style={{ left: `${left()}px`, top: `${top()}px`, width: `${dims().w}px`, height: `${dims().h}px` }}>
        <Show when={props.before}><img src={props.before} alt="Original" draggable={false} /></Show>
        <Show when={props.after}>
          <img src={props.after} alt="Compressed" draggable={false} classList={{ hidden: diff() }}
            style={{ "clip-path": `inset(0 0 0 ${clamp(splitPx() - left(), 0, dims().w)}px)` }} />
        </Show>
        <canvas ref={diffCanvas} classList={{ hidden: !diff() }}
          style={{ "clip-path": `inset(0 0 0 ${clamp(splitPx() - left(), 0, dims().w)}px)` }} />
      </div>
      <div class="mx-split" style={{ left: `${splitPx()}px` }}>
        <span class="mx-knob"><ChevronLeft size={13} /><ChevronRight size={13} /></span>
      </div>
      <span class="mx-tag left">{props.left}</span>
      <span class="mx-tag right">
        {diff() ? "Differences, brightened" : props.right}
        <Show when={props.busy || diffBusy()}><Loader2 class="spin" size={12} /></Show>
      </span>
      <Show when={zoom() == null && !diff()}>
        <span class="mx-tag hint">Drag to compare · scroll or double-click to zoom in on details</span>
      </Show>
      <div class="mx-zoom">
        <button classList={{ on: diff() }} onClick={() => setDiff(!diff())}
          title="Highlight what compression changed (bright = more change)">Differences</button>
        <span class="mx-zoom-sep" />
        <button classList={{ on: zoom() == null }} onClick={() => zoomTo(null)} title="Fit the window">Fit</button>
        <button classList={{ on: zoom() === 1 }} onClick={() => zoomTo(1)} title="Actual pixels (double-click)">100%</button>
        <span>{Math.round(scale() * 100)}%</span>
      </div>
    </div>
  );
}
