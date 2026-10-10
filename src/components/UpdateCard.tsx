import { openUrl } from "@tauri-apps/plugin-opener";
import { Download, ExternalLink, LoaderCircle, Sparkles, X } from "lucide-solid";
import { For, Show } from "solid-js";
import { formatSize, plural } from "../lib/format";
import { dismissUpdate, installUpdate, parseNotes, setUpdater, showWhatsNew, updater } from "../lib/updater";

const notes = () => parseNotes(updater.notes);
const changeCount = () => notes().sections.reduce((n, s) => n + s.items.length, 0);
// Release notes come from the update feed; only ever open links to Ferret's own repository.
const REPO = "https://github.com/meltingice1337/ferret/";

/**
 * "Ferret X is available": a small card first, then the changes in "What's new", which is
 * where the update is started. "Later" hides it until the next version.
 */
export default function UpdateCard() {
  const busy = () => updater.phase === "downloading" || updater.phase === "installing";
  const visible = () =>
    busy() || (updater.version !== updater.dismissed && (updater.phase === "available" || (updater.phase === "error" && !!updater.version)));
  const pct = () => (updater.total ? updater.done / updater.total : -1);
  return (
    <Show when={visible() && !updater.notesOpen}>
      <div class="job update-card" classList={{ error: updater.phase === "error" }}>
        <div class="job-top">
          <span class="job-icon"><Show when={busy()} fallback={<Sparkles size={14} />}><LoaderCircle size={14} class="spin" /></Show></span>
          <span class="job-label">Ferret {updater.version} is available</span>
          <Show when={!busy()}>
            <button class="icon-btn tiny" title="Later" onClick={dismissUpdate}><X size={12} /></button>
          </Show>
        </div>
        <Show when={busy()}>
          <div class="job-bar"><div classList={{ indet: pct() < 0 || updater.phase === "installing" }} style={{ width: `${Math.max(0, pct()) * 100}%` }} /></div>
        </Show>
        <div class="job-sub">
          <Show when={updater.phase === "downloading"}>
            <span>Downloading…</span>
            <span>{updater.total ? `${formatSize(updater.done)} / ${formatSize(updater.total)}` : formatSize(updater.done)}</span>
          </Show>
          <Show when={updater.phase === "installing"}><span>Installing, Ferret will restart…</span></Show>
          <Show when={updater.phase === "error"}><span class="job-cur" title={updater.error}>Update failed: {updater.error}</span></Show>
          <Show when={updater.phase === "available"}>
            <span>You have {updater.current}{changeCount() ? ` · ${plural(changeCount(), "change")}` : ""}</span>
          </Show>
        </div>
        <Show when={!busy()}>
          <div class="update-actions">
            <button class="btn" onClick={dismissUpdate}>Later</button>
            <Show when={updater.phase === "error"} fallback={<button class="btn accent" onClick={showWhatsNew}>What's new</button>}>
              <button class="btn accent" onClick={installUpdate}>Try again</button>
            </Show>
          </div>
        </Show>
      </div>
    </Show>
  );
}

/** The changes in the offered version, with the choice to update now or later. */
export function WhatsNewDialog() {
  const close = () => setUpdater("notesOpen", false);
  const released = () =>
    updater.date ? new Date(`${updater.date}T12:00:00`).toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" }) : "";
  const link = () => (notes().link.startsWith(REPO) ? notes().link : "");
  return (
    <Show when={updater.notesOpen}>
      <div class="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && close()}>
        <div class="modal whats-new" role="dialog" aria-label="What's new" tabIndex={-1}
          ref={(el) => queueMicrotask(() => el.focus())}
          onKeyDown={(e) => { if (e.key === "Escape") close(); e.stopPropagation(); }}>
          <header class="wn-hero">
            <img src="/ferret-256.png" alt="" width={56} height={56} />
            <div class="wn-title">
              <div class="wn-kicker">Update available</div>
              <h2>Ferret {updater.version}</h2>
              <div class="wn-sub">You have {updater.current}{released() ? ` · released ${released()}` : ""}</div>
            </div>
            <button class="icon-btn" title="Close (Esc)" onClick={close}><X size={16} /></button>
          </header>
          <div class="wn-body">
            <For each={notes().sections} fallback={<p class="wn-empty">This version has no release notes.</p>}>
              {(s) => (
                <section class="wn-section">
                  <h3 class={`wn-tag ${s.title.split(" ")[0].toLowerCase()}`}>{s.title}</h3>
                  <ul>
                    <For each={s.items}>{(item) => <li>{withCode(item)}</li>}</For>
                  </ul>
                </section>
              )}
            </For>
            <Show when={link()}>
              <button class="wn-link" onClick={() => openUrl(link())}>Full changelog on GitHub<ExternalLink size={12} /></button>
            </Show>
          </div>
          <footer class="wn-actions">
            <span class="wn-note">Ferret restarts to finish updating.</span>
            <button class="btn" onClick={dismissUpdate}>Later</button>
            <button class="btn accent" onClick={installUpdate}><Download size={14} />Update & restart</button>
          </footer>
        </div>
      </div>
    </Show>
  );
}

/** `code` spans in commit subjects. */
function withCode(text: string) {
  return text.split(/(`[^`]+`)/).map((part) => (part.startsWith("`") && part.endsWith("`") && part.length > 2 ? <code>{part.slice(1, -1)}</code> : part));
}
