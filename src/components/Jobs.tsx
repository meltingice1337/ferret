import { ArchiveRestore, Check, Copy, FileArchive, FilePlus, Minimize2, MoveRight, PackageOpen, Pencil, RotateCw, Trash2, TriangleAlert, X } from "../lib/icons";
import { For, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import { api } from "../lib/api";
import { drag, dragLabel, externalDrag } from "../lib/drag";
import { formatSize } from "../lib/format";
import { jobs } from "../lib/ops";
import { friendly } from "../lib/state";
import { createSignal, onCleanup } from "solid-js";
import { basename } from "../lib/path";

const ICON: Record<string, any> = {
  copy: Copy, move: MoveRight, delete: Trash2, recycle: Trash2, extract: PackageOpen, compress: FileArchive,
  rename: Pencil, new: FilePlus, restore: ArchiveRestore, media: Minimize2,
};

function eta(j: { bytesTotal: number; bytesDone: number }, r: number) {
  if (!r || !j.bytesTotal) return "";
  const s = (j.bytesTotal - j.bytesDone) / r;
  if (s < 1) return "";
  return s < 60 ? `${Math.ceil(s)}s left` : `${Math.ceil(s / 60)} min left`;
}

export function JobsPanel() {
  // Jobs that finish within ~0.6 s never get a card (the toast is enough).
  const [now, setNow] = createSignal(performance.now());
  const timer = setInterval(() => setNow(performance.now()), 250);
  onCleanup(() => clearInterval(timer));
  const list = () => Object.values(jobs)
    .filter((j) => j.status === "error" || j.status === "waiting" || (j.started !== undefined && now() - j.started > 600 && (j.status === "running" || (j.filesTotal > 1 || j.bytesTotal > 8 * 1024 * 1024))))
    .sort((a, b) => a.id - b.id);
  return (
    <Show when={list().length}>
      <div class="jobs" ref={(el) => {
        // Toasts sit above the job cards instead of overlapping them.
        const ro = new ResizeObserver(() => requestAnimationFrame(() => document.documentElement.style.setProperty("--jobs-h", `${el.isConnected ? el.offsetHeight + 8 : 0}px`)));
        ro.observe(el);
        onCleanup(() => { ro.disconnect(); document.documentElement.style.setProperty("--jobs-h", "0px"); });
      }}>
        <For each={list()}>
          {(j) => {
            const pct = () => (j.bytesTotal ? j.bytesDone / j.bytesTotal : j.filesTotal ? j.filesDone / j.filesTotal : j.status === "running" ? -1 : 1);
            return (
              <div class="job" classList={{ running: j.status === "running", done: j.status === "done", error: j.status === "error", cancelled: j.status === "cancelled", waiting: j.status === "waiting" }}>
                <div class="job-top">
                  <span class="job-icon"><Dynamic component={j.status === "done" ? Check : j.status === "waiting" ? TriangleAlert : ICON[j.kind] ?? Copy} size={14} /></span>
                  <span class="job-label" title={j.label}>{j.label}</span>
                  <Show when={j.status === "running"}>
                    <button class="icon-btn tiny" title="Cancel" onClick={() => api.cancelJob(j.id)}><X size={12} /></button>
                  </Show>
                </div>
                {/* Nothing finished yet: the sliding bar says it has started. */}
                <div class="job-bar"><div classList={{ indet: pct() < 0 || (j.status === "running" && pct() === 0) }} style={{ width: `${Math.max(0, pct()) * 100}%` }} /></div>
                {/* Paused on an item it can't handle, like Explorer's "File in use" dialog. */}
                <Show when={j.status === "waiting"}>
                  <div class="job-ask">
                    <span class="job-question">{friendly(j.question ?? "")}</span>
                    <div class="job-ask-actions">
                      <button class="btn" onClick={() => api.answerJob(j.id, "retry")}><RotateCw size={13} />Try again</button>
                      <button class="btn" onClick={() => api.answerJob(j.id, "skip")}>Skip</button>
                      <button class="btn" title="Skip every item that can't be handled, without asking again" onClick={() => api.answerJob(j.id, "skipAll")}>Skip all</button>
                      <button class="btn warn" onClick={() => api.cancelJob(j.id)}>Cancel</button>
                    </div>
                  </div>
                </Show>
                <div class="job-sub" style={j.status === "waiting" ? { display: "none" } : undefined}>
                  <Show when={j.status === "running"} fallback={<span>{j.status === "done" ? "Done" : j.status === "cancelled" ? "Cancelled" : friendly(j.error ?? "Failed")}</span>}>
                    {/* One item taking long (a big folder): say so, so it doesn't look stuck. */}
                    <span class="job-cur" title={j.current}>
                      {!j.current ? "Preparing…" : now() - (j.currentSince ?? now()) > 3000
                        ? (j.kind === "recycle" || j.kind === "delete" ? `Still on “${j.current}”, big folders take a while` : `Still on “${j.current}”…`)
                        : j.current}
                    </span>
                    <span>
                      {j.bytesTotal ? `${formatSize(j.bytesDone)} / ${formatSize(j.bytesTotal)}` : j.filesTotal ? `${j.filesDone} / ${j.filesTotal}` : ""}
                      {(j.rate ?? 0) > 0 ? ` · ${formatSize(j.rate!)}/s${eta(j, j.rate!) ? ` · ${eta(j, j.rate!)}` : ""}` : ""}
                    </span>
                  </Show>
                </div>
              </div>
            );
          }}
        </For>
      </div>
    </Show>
  );
}

export function DragGhost() {
  return (
    <>
      <Show when={drag()}>
        {(d) => (
          <div class="drag-ghost" style={{ transform: `translate(${d().x + 14}px, ${d().y + 12}px)` }}>
            <span class="drag-count">{d().paths.length}</span>
            <span>{dragLabel(d())}</span>
          </div>
        )}
      </Show>
      <Show when={externalDrag()}>
        {(x) => (
          <div class="drop-banner">
            Drop {x().count > 1 ? `${x().count} items` : "here"} to copy into <b>{x().target ? basename(x().target!) : "this folder"}</b>
          </div>
        )}
      </Show>
    </>
  );
}
