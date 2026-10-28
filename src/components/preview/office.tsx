// Spreadsheet and slide-deck previews (src-tauri/src/office.rs reads the files).
import { LoaderCircle } from "../../lib/icons";
import { createEffect, createMemo, createResource, createSignal, For, Index, onCleanup, Show, type JSX } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { fileUrl } from "../../lib/api";
import { plural } from "../../lib/format";
import { ErrorNote, type SetMeta } from "./renderers";
import "../../office.css";

const Spinner = () => <div class="pv-center"><LoaderCircle size={20} class="spin" /></div>;

// ---------- spreadsheets ----------

interface SheetPreview {
  sheets: string[];
  sheet: number;
  rows: string[][];
  firstRow: number;
  firstCol: number;
  totalRows: number;
  totalCols: number;
}

/** A, B … Z, AA, AB … for a 0-based column. */
export function columnName(i: number) {
  let s = "";
  for (let n = i + 1; n > 0; n = Math.floor((n - 1) / 26)) s = String.fromCharCode(65 + ((n - 1) % 26)) + s;
  return s;
}

/** A workbook's sheet as a grid with Excel's column letters and row numbers, its sheets as tabs. */
export function SheetView(props: { path: string; setMeta: SetMeta; large?: boolean }) {
  const [sheet, setSheet] = createSignal(0);
  const [data] = createResource(() => ({ path: props.path, sheet: sheet() }), (k) => invoke<SheetPreview>("sheet_preview", k));
  const [scroll, setScroll] = createSignal(0);
  const [viewH, setViewH] = createSignal(400);
  const ROW_H = 26;

  const grid = createMemo(() => {
    const d = data.latest;
    if (!d) return null;
    const cols = Math.max(0, ...d.rows.slice(0, 500).map((r) => r.length));
    const widths = Array.from({ length: cols }, (_, c) => {
      const longest = Math.max(1, ...d.rows.slice(0, 200).map((r) => (r[c] ?? "").length));
      return Math.min(280, Math.max(56, longest * 7.2 + 18));
    });
    return { ...d, cols, widths };
  });

  createEffect(() => {
    const g = grid();
    if (!g) return;
    props.setMeta([
      ...(g.sheets.length > 1 ? [{ label: "Sheets", value: String(g.sheets.length) }] : []),
      { label: "Rows", value: `${g.totalRows.toLocaleString()}${g.rows.length < g.totalRows ? ` (first ${g.rows.length.toLocaleString()} shown)` : ""}` },
      { label: "Columns", value: g.totalCols.toLocaleString() },
    ]);
  });

  const visible = createMemo(() => {
    const g = grid();
    if (!g) return { start: 0, rows: [] as string[][] };
    const start = Math.max(0, Math.floor(scroll() / ROW_H) - 10);
    return { start, rows: g.rows.slice(start, start + Math.ceil(viewH() / ROW_H) + 20) };
  });

  return (
    <Show when={!data.error} fallback={<ErrorNote>{String(data.error)}</ErrorNote>}>
      <Show when={grid()} fallback={<Spinner />}>
        {(g) => {
          const tmpl = () => `48px ${g().widths.map((w) => `${w}px`).join(" ")}`;
          const total = () => 48 + g().widths.reduce((a, b) => a + b, 0);
          return (
            <div class="pv-sheet">
              <Show when={g().rows.length} fallback={<ErrorNote>This sheet is empty</ErrorNote>}>
                <div class="pv-csv" classList={{ loading: data.loading }}
                  ref={(el) => { const ro = new ResizeObserver(() => requestAnimationFrame(() => setViewH(el.clientHeight))); ro.observe(el); onCleanup(() => ro.disconnect()); }}
                  onScroll={(e) => setScroll(e.currentTarget.scrollTop)}>
                  <div class="csv-head" style={{ "grid-template-columns": tmpl(), width: `${total()}px` }}>
                    <span class="csv-rn" />
                    <Index each={g().widths}>{(_, c) => <span class="csv-h sheet-col">{columnName(g().firstCol + c)}</span>}</Index>
                  </div>
                  <div class="csv-body" style={{ height: `${g().rows.length * ROW_H}px`, width: `${total()}px` }}>
                    <For each={visible().rows}>
                      {(r, i) => (
                        <div class="csv-row" style={{ "grid-template-columns": tmpl(), transform: `translateY(${(visible().start + i()) * ROW_H}px)` }}>
                          <span class="csv-rn">{g().firstRow + visible().start + i() + 1}</span>
                          <Index each={Array.from({ length: g().cols }, (_, c) => r[c] ?? "")}>
                            {(v) => <span class="csv-c" classList={{ num: /^-?[\d.,]+(e[+-]?\d+)?%?$/i.test(v()) }} title={v()}>{v()}</span>}
                          </Index>
                        </div>
                      )}
                    </For>
                  </div>
                </div>
              </Show>
              <Show when={g().sheets.length > 1}>
                <div class="sheet-tabs" role="tablist">
                  <For each={g().sheets}>
                    {(name, i) => (
                      <button role="tab" class="sheet-tab" classList={{ on: i() === g().sheet }} aria-selected={i() === g().sheet}
                        onClick={() => { setScroll(0); setSheet(i()); }}>{name}</button>
                    )}
                  </For>
                </div>
              </Show>
            </div>
          );
        }}
      </Show>
    </Show>
  );
}

// ---------- slide decks ----------

interface Run { text: string; size: number; bold: boolean; italic: boolean; underline: boolean; color: string | null; font: string | null; caps: string | null }
interface Para { align: string; level: number; bullet: string | null; runs: Run[]; size: number; line: number; before: number }
interface Shape {
  x: number; y: number; w: number; h: number; rot: number; flipH: boolean; flipV: boolean;
  geom: string; fill: string | null; line: string | null; lineW: number;
  image: string | null; crop: [number, number, number, number] | null;
  paras: Para[]; anchor: string; inset: [number, number, number, number];
  table: string[][] | null; label: string | null; vert: string | null;
}
interface Slide { background: string | null; backgroundImage: string | null; shapes: Shape[]; hidden: boolean }
interface Deck { width: number; height: number; slides: Slide[]; total: number }

/** EMU (positions and sizes in a deck) per CSS pixel, and per point. */
const EMU_PX = 9525;
const PT_PX = 4 / 3;
const px = (emu: number) => emu / EMU_PX;

const ALIGN: Record<string, JSX.CSSProperties["text-align"]> = { l: "left", ctr: "center", r: "right", just: "justify", dist: "justify" };
const ANCHOR: Record<string, string> = { t: "flex-start", ctr: "center", b: "flex-end" };

/** "16:9", "4:3"… or the size in inches. */
function aspect(w: number, h: number) {
  const r = w / h;
  const known: [number, string][] = [[16 / 9, "16:9"], [4 / 3, "4:3"], [16 / 10, "16:10"], [1, "1:1"]];
  const hit = known.find(([k]) => Math.abs(k - r) < 0.01);
  return hit ? hit[1] : `${(w / 914400).toFixed(1)} × ${(h / 914400).toFixed(1)} in`;
}

/** A deck's slides, one under another, scaled to the pane. */
export function SlidesView(props: { path: string; setMeta: SetMeta; large?: boolean }) {
  const [deck] = createResource(() => props.path, (path) => invoke<Deck>("slides_preview", { path }));
  const [width, setWidth] = createSignal(300);

  createEffect(() => {
    const d = deck();
    if (!d) return;
    const hidden = d.slides.filter((s) => s.hidden).length;
    props.setMeta([
      { label: "Slides", value: `${d.total}${hidden ? ` (${hidden} hidden)` : ""}` },
      { label: "Slide size", value: aspect(d.width, d.height) },
    ]);
  });

  return (
    <Show when={!deck.error} fallback={<ErrorNote>{String(deck.error)}</ErrorNote>}>
      <Show when={deck()} fallback={<Spinner />}>
        {(d) => (
          <div class="pv-slides" ref={(el) => {
            const ro = new ResizeObserver(() => setWidth(el.clientWidth));
            ro.observe(el);
            onCleanup(() => ro.disconnect());
          }}>
            <Show when={d().slides.length} fallback={<ErrorNote>This deck has no slides</ErrorNote>}>
              <For each={d().slides}>
                {(s, i) => <SlideBox slide={s} n={i() + 1} w={d().width} h={d().height} fit={Math.max(120, width() - (props.large ? 48 : 24))} />}
              </For>
              <Show when={d().total > d().slides.length}>
                <div class="slide-more">{plural(d().total - d().slides.length, "more slide")} not shown</div>
              </Show>
            </Show>
          </div>
        )}
      </Show>
    </Show>
  );
}

function SlideBox(props: { slide: Slide; n: number; w: number; h: number; fit: number }) {
  const scale = () => props.fit / px(props.w);
  // Slides off screen are drawn once they come near (a long deck with pictures stays quick).
  const [near, setNear] = createSignal(false);
  return (
    <figure class="slide" classList={{ hidden: props.slide.hidden }}>
      <div class="slide-frame" style={{ width: `${props.fit}px`, height: `${(props.fit * props.h) / props.w}px` }}
        ref={(el) => {
          const io = new IntersectionObserver((es) => { if (es.some((e) => e.isIntersecting)) { setNear(true); io.disconnect(); } }, { rootMargin: "800px 0px" });
          io.observe(el);
          onCleanup(() => io.disconnect());
        }}>
        <Show when={near()}>
          <div class="slide-stage" style={{
            width: `${px(props.w)}px`, height: `${px(props.h)}px`, transform: `scale(${scale()})`,
            background: props.slide.background ?? "#fff",
            "background-image": props.slide.backgroundImage ? `url("${fileUrl(props.slide.backgroundImage)}")` : undefined,
          }}>
            <For each={props.slide.shapes}>{(sh) => <ShapeBox s={sh} />}</For>
          </div>
        </Show>
      </div>
      <figcaption>{props.n}{props.slide.hidden ? " · hidden" : ""}</figcaption>
    </figure>
  );
}

function ShapeBox(props: { s: Shape }) {
  const s = () => props.s;
  const box = (): JSX.CSSProperties => {
    const t = [s().rot ? `rotate(${s().rot}deg)` : "", s().flipH ? "scaleX(-1)" : "", s().flipV ? "scaleY(-1)" : ""].filter(Boolean).join(" ");
    const geom = s().geom;
    return {
      left: `${px(s().x)}px`, top: `${px(s().y)}px`, width: `${px(s().w)}px`, height: `${px(s().h)}px`,
      transform: t || undefined,
      background: geom !== "line" ? s().fill ?? undefined : undefined,
      border: geom !== "line" && s().line ? `${Math.max(0.75, s().lineW) * PT_PX}px solid ${s().line}` : undefined,
      "border-radius": geom === "ellipse" ? "50%" : geom.startsWith("round") ? `${Math.min(px(s().w), px(s().h)) * 0.17}px` : undefined,
    };
  };
  // Text keeps its direction when the box is flipped.
  const unflip = () => [s().flipH ? "scaleX(-1)" : "", s().flipV ? "scaleY(-1)" : ""].filter(Boolean).join(" ") || undefined;
  return (
    <div class="sh" style={box()}>
      <Show when={s().geom === "line" && s().line}>
        <svg class="sh-line" width="100%" height="100%" preserveAspectRatio="none" viewBox={`0 0 ${Math.max(1, px(s().w))} ${Math.max(1, px(s().h))}`}>
          <line x1="0" y1="0" x2={px(s().w)} y2={px(s().h)} stroke={s().line!} stroke-width={Math.max(0.75, s().lineW) * PT_PX} />
        </svg>
      </Show>
      <Show when={s().image}>
        {(src) => {
          const c = () => s().crop ?? [0, 0, 0, 0];
          const kw = () => 1 / Math.max(0.01, 1 - c()[0] - c()[2]);
          const kh = () => 1 / Math.max(0.01, 1 - c()[1] - c()[3]);
          return (
            <img class="sh-img" src={fileUrl(src())} alt="" loading="lazy" draggable={false}
              style={{ width: `${kw() * 100}%`, height: `${kh() * 100}%`, left: `${-c()[0] * kw() * 100}%`, top: `${-c()[1] * kh() * 100}%` }} />
          );
        }}
      </Show>
      <Show when={s().label}><div class="sh-label">{s().label}</div></Show>
      <Show when={s().table}>
        {(rows) => (
          <table class="sh-table">
            <tbody><For each={rows()}>{(r) => <tr><For each={r}>{(c) => <td>{c}</td>}</For></tr>}</For></tbody>
          </table>
        )}
      </Show>
      <Show when={s().paras.length}>
        <div class="sh-text" classList={{ vert: !!s().vert, up: s().vert === "vert270" }} style={{
          "justify-content": ANCHOR[s().anchor] ?? "flex-start", transform: unflip(),
          padding: `${px(s().inset[1])}px ${px(s().inset[2])}px ${px(s().inset[3])}px ${px(s().inset[0])}px`,
        }}>
          <For each={s().paras}>
            {(p, i) => (
              <p style={{
                "text-align": ALIGN[p.align] ?? "left", "padding-left": p.level ? `${p.level * 36}px` : undefined,
                // Single spacing is about 1.2 lines; PowerPoint leaves out the first paragraph's space above.
                "line-height": `${1.2 * p.line}`, "margin-top": i() && p.before ? `${p.before * PT_PX}px` : undefined,
                "min-height": p.runs.length ? undefined : `${p.size * PT_PX * 1.2 * p.line}px`,
              }}>
                <Show when={p.bullet}><span class="sh-bullet" style={{ "font-size": `${(p.runs[0]?.size ?? p.size) * PT_PX}px`, color: p.runs[0]?.color ?? undefined }}>{p.bullet}</span></Show>
                <For each={p.runs}>
                  {(r) => (r.text === "\n" ? <br /> : (
                    <span style={{
                      "font-size": `${r.size * PT_PX}px`, "font-weight": r.bold ? 700 : undefined, "font-style": r.italic ? "italic" : undefined,
                      "text-decoration": r.underline ? "underline" : undefined, color: r.color ?? undefined,
                      "text-transform": r.caps === "all" ? "uppercase" : undefined, "font-variant-caps": r.caps === "small" ? "small-caps" : undefined,
                      "font-family": r.font ? `"${r.font}", "Segoe UI", sans-serif` : undefined,
                    }}>{r.text}</span>
                  ))}
                </For>
              </p>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
