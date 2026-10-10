// Fit-to-width PDF rendering for the narrow preview pane (pages render as they scroll in).

import { createEffect, createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import { fileUrl } from "../../lib/api";
import type { SetMeta } from "./renderers";

let pdfjs: Promise<typeof import("pdfjs-dist")> | null = null;
function loadPdfjs() {
  pdfjs ??= Promise.all([import("pdfjs-dist"), import("pdfjs-dist/build/pdf.worker.min.mjs?url")]).then(([lib, worker]) => {
    lib.GlobalWorkerOptions.workerSrc = worker.default;
    return lib;
  });
  return pdfjs;
}

const MAX_PAGES = 200;

export default function PdfPages(props: { path: string; setMeta: SetMeta; large?: boolean }) {
  const [doc, setDoc] = createSignal<any>(null);
  const [error, setError] = createSignal<string | null>(null);
  let container!: HTMLDivElement;

  createEffect(() => {
    const path = props.path;
    let cancelled = false;
    let task: any;
    setDoc(null);
    setError(null);
    loadPdfjs().then((lib) => {
      if (cancelled) return;
      task = lib.getDocument({ url: fileUrl(path), disableAutoFetch: true });
      task.promise.then((d: any) => {
        if (cancelled) return task?.destroy?.();
        setDoc(d);
        props.setMeta([{ label: "Pages", value: String(d.numPages) }]);
      }).catch((e: unknown) => !cancelled && setError(String(e)));
    });
    onCleanup(() => { cancelled = true; task?.destroy?.(); });
  });

  return (
    <Show when={!error()} fallback={<div class="pv-center pv-note">Couldn't render this PDF.<br />{error()}</div>}>
      <div class="pv-pdfjs" classList={{ large: props.large }} ref={container}>
        <Show when={doc()} fallback={<div class="pv-center pv-note">Loading PDF…</div>}>
          {(d) => (
            <For each={Array.from({ length: Math.min(d().numPages, MAX_PAGES) }, (_, i) => i + 1)}>
              {(n) => <PdfPage doc={d()} n={n} root={container} />}
            </For>
          )}
        </Show>
      </div>
    </Show>
  );
}

function PdfPage(props: { doc: any; n: number; root: HTMLElement }) {
  let canvas!: HTMLCanvasElement;
  let box!: HTMLDivElement;
  const [aspect, setAspect] = createSignal(1.294);

  createEffect(() => {
    let rendered = false;
    let renderTask: any;
    const io = new IntersectionObserver(async (entries) => {
      if (rendered || !entries.some((e) => e.isIntersecting)) return;
      rendered = true;
      const page = await props.doc.getPage(props.n);
      const base = page.getViewport({ scale: 1 });
      setAspect(base.height / base.width);
      const dpr = window.devicePixelRatio || 1;
      const scale = (box.clientWidth / base.width) * dpr;
      const vp = page.getViewport({ scale });
      canvas.width = Math.floor(vp.width);
      canvas.height = Math.floor(vp.height);
      renderTask = page.render({ canvas, canvasContext: canvas.getContext("2d")!, viewport: vp });
      renderTask.promise.catch(() => {});
    }, { root: props.root, rootMargin: "400px 0px" });
    io.observe(box);
    onCleanup(() => { io.disconnect(); renderTask?.cancel?.(); });
  });

  return (
    <div class="pdf-page" ref={box} style={{ "aspect-ratio": `1 / ${aspect()}` }}>
      <canvas ref={canvas} />
      <span class="pdf-num">{props.n}</span>
    </div>
  );
}

// ---------- grid thumbnails ----------

const thumbCache = new Map<string, string>();
let running = 0;
const queue: (() => void)[] = [];
const slot = () => new Promise<void>((r) => (running < 2 ? (running++, r()) : queue.push(() => (running++, r()))));
const release = () => { running--; queue.shift()?.(); };

/** First page of a PDF rendered to a data URL (cached, two at a time). */
export function PdfThumb(props: { path: string; size: number; fallback: JSX.Element }) {
  const [src, setSrc] = createSignal<string | null>(thumbCache.get(`${props.path}|${props.size}`) ?? null);
  const [failed, setFailed] = createSignal(false);
  createEffect(() => {
    const key = `${props.path}|${props.size}`;
    if (thumbCache.has(key)) return setSrc(thumbCache.get(key)!);
    let cancelled = false;
    onCleanup(() => (cancelled = true));
    (async () => {
      await slot();
      try {
        if (cancelled) return;
        const lib = await loadPdfjs();
        const task = lib.getDocument({ url: fileUrl(props.path), disableAutoFetch: true });
        const doc = await task.promise;
        const page = await doc.getPage(1);
        const base = page.getViewport({ scale: 1 });
        const px = props.size * (window.devicePixelRatio || 1);
        const vp = page.getViewport({ scale: px / Math.max(base.width, base.height) });
        const canvas = document.createElement("canvas");
        canvas.width = Math.ceil(vp.width);
        canvas.height = Math.ceil(vp.height);
        await page.render({ canvas, canvasContext: canvas.getContext("2d")!, viewport: vp }).promise;
        const url = canvas.toDataURL("image/png");
        task.destroy();
        thumbCache.set(key, url);
        if (!cancelled) setSrc(url);
      } catch {
        if (!cancelled) setFailed(true);
      } finally {
        release();
      }
    })();
  });
  return (
    <Show when={src()} fallback={props.fallback}>
      <img class="thumb pdf-thumb" src={src()!} style={{ "max-width": `${props.size}px`, "max-height": `${props.size}px` }} alt="" draggable={false} />
    </Show>
  );
  void failed;
}
