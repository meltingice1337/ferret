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

  // Archive entries are extracted to a cache before previewing.
  const [real] = createResource(() => props.entry, (e) => (e.isDir && !e.inArchive ? Promise.resolve(e.path) : realPath(e)));
  const e = () => props.entry;
  const kind = () => kindOf(e());

  return (
    <ErrorBoundary fallback={(err) => <ErrorNote>{friendly(String(err?.message ?? err))}</ErrorNote>}>
    <Show when={real()} fallback={<Show when={real.state === "ready"}><ErrorNote>Couldn't open this item</ErrorNote></Show>}>
      {(path) => (
        <Switch fallback={<FallbackView path={path()} entry={e()} large={props.large} setMeta={setMeta} />}>
          <Match when={e().isDir}>
            <FolderView path={path()} setMeta={setMeta} onOpen={(p) => navigate(activeTab(), p)} />
          </Match>
          <Match when={isArchive(e()) && !e().inArchive}>
            <ArchiveView entry={e()} setMeta={setMeta}
              onBrowse={() => navigate(activeTab(), e().path)}
              onExtract={() => extractSmart(e().path)} />
          </Match>
          <Match when={kind() === "image"}>
            <ImageView path={path()} ext={IMAGE_SHELL_ONLY.has(e().ext) ? "shell" : e().ext} setMeta={setMeta} large={props.large} />
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
      )}
    </Show>
    </ErrorBoundary>
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
