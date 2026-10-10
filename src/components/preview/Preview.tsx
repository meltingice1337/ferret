import { createEffect, createResource, createSignal, ErrorBoundary, Match, on, Show, Switch } from "solid-js";
import { kindOf, typeLabel } from "../../lib/format";
import { extractSmart, realPath } from "../../lib/ops";
import { activeTab, friendly, navigate } from "../../lib/state";
import type { Entry } from "../../lib/types";
import { isArchive } from "../menus";
import {
  ArchiveView, AudioView, CodeView, CsvView, DocxView, ErrorNote, FolderView, FontView, ImageView, JsonView,
  MarkdownView, NoPreview, PdfView, ShellThumbView, VideoView, type Meta,
} from "./renderers";

const TEXTISH = new Set(["code", "text", "data"]);
const SHELL_THUMB = new Set(["doc", "sheet", "exe"]);
const IMAGE_SHELL_ONLY = new Set(["heic", "heif", "tif", "tiff", "raw", "cr2", "nef", "arw", "dng", "psd"]);

/** Picks the right renderer for an entry. `large` = Quick Look. */
export default function Preview(props: { entry: Entry; large?: boolean; onMeta?: (m: Meta[]) => void }) {
  const [meta, setMeta] = createSignal<Meta[]>([]);
  createEffect(on(() => props.entry.path, () => setMeta([])));
  createEffect(() => props.onMeta?.(meta()));

  // Archive entries are extracted to a cache before previewing. Until the next item is ready
  // the current one stays on screen.
  const [real] = createResource(() => props.entry, async (e) => {
    const path = e.isDir && !e.inArchive ? e.path : await realPath(e);
    return path ? { e, path } : null;
  });
  const isImage = () => !!real() && !real()!.e.isDir && kindOf(real()!.e) === "image";

  return (
    <ErrorBoundary fallback={(err, reset) => {
      createEffect(on(() => props.entry.path, reset, { defer: true }));
      return <ErrorNote>{friendly(String(err?.message ?? err))}</ErrorNote>;
    }}>
    <Show when={real()} fallback={<Show when={real.state === "ready"}><ErrorNote>Couldn't open this item</ErrorNote></Show>}>
      {(r) => (
        // One image viewer serves consecutive pictures (it swaps them without a blank frame);
        // every other kind of preview is built fresh for each item.
        <Show when={isImage()} fallback={<Show when={r()} keyed>{(it) => <View e={it.e} path={it.path} large={props.large} setMeta={setMeta} />}</Show>}>
          <ImageView path={r().path} ext={IMAGE_SHELL_ONLY.has(r().e.ext) ? "shell" : r().e.ext} setMeta={setMeta} large={props.large} />
        </Show>
      )}
    </Show>
    </ErrorBoundary>
  );
}

function View(props: { e: Entry; path: string; large?: boolean; setMeta: (m: Meta[]) => void }) {
  const e = () => props.e;
  const kind = () => kindOf(e());
  const path = () => props.path;
  const setMeta = props.setMeta;
  return (
    <Switch fallback={<FallbackView path={path()} entry={e()} large={props.large} setMeta={setMeta} />}>
      <Match when={e().isDir}>
        <FolderView path={path()} setMeta={setMeta} onOpen={(p) => navigate(activeTab(), p)} />
      </Match>
      <Match when={isArchive(e()) && !e().inArchive}>
        <ArchiveView entry={e()} setMeta={setMeta}
          onBrowse={() => navigate(activeTab(), e().path)}
          onExtract={() => extractSmart(e().path)} />
      </Match>
      <Match when={kind() === "video"}><VideoView path={path()} setMeta={setMeta} large={props.large} /></Match>
      <Match when={kind() === "audio"}><AudioView path={path()} setMeta={setMeta} large={props.large} /></Match>
      <Match when={kind() === "pdf"}><PdfView path={path()} large={props.large} setMeta={setMeta} /></Match>
      <Match when={kind() === "font"}><FontView path={path()} name={e().name} /></Match>
      <Match when={e().ext === "csv" || e().ext === "tsv"}><CsvView path={path()} ext={e().ext} setMeta={setMeta} large={props.large} /></Match>
      <Match when={e().ext === "json" || e().ext === "jsonc" || e().ext === "geojson"}>
        <JsonView path={path()} name={e().name} ext={e().ext} setMeta={setMeta} large={props.large} />
      </Match>
      <Match when={e().ext === "md" || e().ext === "markdown"}>
        <MarkdownView path={path()} name={e().name} ext={e().ext} setMeta={setMeta} large={props.large} />
      </Match>
      <Match when={TEXTISH.has(kind())}>
        <CodeView path={path()} name={e().name} ext={e().ext} setMeta={setMeta} large={props.large} />
      </Match>
      <Match when={/^(docx|docm|dotx)$/.test(e().ext)}>
        <DocxView path={path()} entry={e()} setMeta={setMeta} large={props.large} />
      </Match>
      <Match when={SHELL_THUMB.has(kind())}><ShellThumbView path={path()} entry={e()} large={props.large} /></Match>
    </Switch>
  );
}

/** Unknown types: show as text if it decodes as text, otherwise the no-preview placeholder. */
function FallbackView(props: { path: string; entry: Entry; large?: boolean; setMeta: (m: Meta[]) => void }) {
  const [probe] = createResource(() => props.path, async (p) => {
    const { api } = await import("../../lib/api");
    return api.readText(p, 64 * 1024).catch(() => null);
  });
  return (
    <Show when={probe.state === "ready"}>
      <Show when={probe() && !probe()!.binary && props.entry.size > 0} fallback={
        <Show when={props.entry.size > 0} fallback={<ErrorNote>Empty file · {typeLabel(props.entry)}</ErrorNote>}>
          <NoPreview path={props.path} entry={props.entry} large={props.large} />
        </Show>
      }>
        <CodeView path={props.path} name={props.entry.name} ext={props.entry.ext} setMeta={props.setMeta} large={props.large} />
      </Show>
    </Show>
  );
}
