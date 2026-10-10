import { Download, LoaderCircle, X } from "lucide-solid";
import { Show } from "solid-js";
import { formatSize } from "../lib/format";
import { dismissUpdate, installUpdate, updater } from "../lib/updater";

/** "Ferret X is available" card: Update & restart, or Later (hidden until the next version). */
export default function UpdateCard() {
  const busy = () => updater.phase === "downloading" || updater.phase === "installing";
  const visible = () =>
    busy() || (updater.version !== updater.dismissed && (updater.phase === "available" || (updater.phase === "error" && !!updater.version)));
  const pct = () => (updater.total ? updater.done / updater.total : -1);
  return (
    <Show when={visible()}>
      <div class="job update-card" classList={{ error: updater.phase === "error" }}>
        <div class="job-top">
          <span class="job-icon"><Show when={busy()} fallback={<Download size={14} />}><LoaderCircle size={14} class="spin" /></Show></span>
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
          <Show when={updater.phase === "available" || updater.phase === "error"}>
            <span>You have {updater.current}</span>
          </Show>
        </div>
        <Show when={!busy()}>
          <div class="update-actions">
            <button class="btn" onClick={dismissUpdate}>Later</button>
            <button class="btn accent" onClick={installUpdate}>{updater.phase === "error" ? "Try again" : "Update & restart"}</button>
          </div>
        </Show>
      </div>
    </Show>
  );
}
