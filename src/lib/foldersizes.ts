// Folder sizes for the Size column: counted in the background by the backend (foldersize.rs),
// which reports them as `folder-size` events, also while still counting.
import { listen } from "@tauri-apps/api/event";
import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { api } from "./api";
import { isRemote, isVirtual, isWsl, pathKey, remoteParts } from "./path";
import type { FolderSize } from "./types";

export const [folderSizes, setFolderSizes] = createStore<Record<string, FolderSize>>({});

/** A new count only replaces a finished one once it's finished too, so a refresh shows the
 *  old size (marked as refreshing) rather than counting up from zero again. An older count
 *  never replaces a newer one: the finished recount can arrive before the reply that said
 *  it was starting. A finished count does replace one shown as refreshing, though: a count
 *  that used earlier sizes of folders inside carries their (older) time. */
function take(s: Record<string, FolderSize>, size: FolderSize) {
  if (uncounted(size.path)) return;
  const k = pathKey(size.path);
  const old = s[k];
  if (!old?.done || (size.done && ((old.refreshing && !size.refreshing) || size.countedAt >= old.countedAt))) s[k] = size;
}

// Counting a folder of 3,000 folders sends thousands of these: applied in bundles (a few times
// a second), not one by one, or the list redraws itself for every single folder.
let incoming: FolderSize[] = [];
let flushTimer: number | undefined;
let lastSort = 0;
let sortTimer: number | undefined;
listen<FolderSize>("folder-size", (ev) => {
  incoming.push(ev.payload);
  flushTimer ??= window.setTimeout(flush, 250);
});
function flush() {
  flushTimer = undefined;
  const batch = incoming;
  incoming = [];
  setFolderSizes(produce((s) => batch.forEach((x) => take(s, x))));
  if (batch.some((x) => x.done)) resort();
}

/** What the counting is doing (for the status bar): folders on screen waiting or being counted,
 *  others counted in the background, and a few of the ones being counted right now. */
type SizeActivity = { left: number; background: number; counting: string[] };
export const [sizeActivity, setSizeActivity] = createSignal<SizeActivity>({ left: 0, background: 0, counting: [] });
listen<SizeActivity>("folder-sizes-activity", (ev) => setSizeActivity(ev.payload));

/** Bumped when finished sizes arrived: a list sorted by size puts its folders in order again.
 *  At most once a second, so rows don't keep jumping (and re-sorting thousands of items for
 *  every counted folder doesn't slow scrolling). */
export const [sizeOrder, setSizeOrder] = createSignal(0);
function resort() {
  if (sortTimer !== undefined) return;
  const wait = Math.max(0, 1000 - (performance.now() - lastSort));
  sortTimer = window.setTimeout(() => {
    sortTimer = undefined;
    lastSort = performance.now();
    setSizeOrder((n) => n + 1);
  }, wait);
}

export const folderSize = (path: string): FolderSize | undefined => folderSizes[pathKey(path)];

/** Network shares are slow to walk, so they aren't counted. (WSL folders and those on servers
 *  Ferret connects to are counted by Linux itself, which is fast.) */
export const sizesApply = (dir: string) => !isVirtual(dir) && (isWsl(dir) || isRemote(dir) || !dir.startsWith("\\\\"));

/** Why a WSL or server folder gets no size, if it doesn't: Linux's /proc and /sys describe the
 *  running system rather than hold files (/proc alone "contains" 128 TB), and in WSL /mnt/c is
 *  the Windows drive itself, very slow to walk from Linux and counted directly when opened as C:. */
export function uncounted(path: string): string | undefined {
  if (isRemote(path)) {
    const linux = remoteParts(path)?.linux ?? "";
    return /^\/(proc|sys)(\/|$)/.test(linux) ? "Linux system information, not files on disk" : undefined;
  }
  if (!isWsl(path)) return;
  const m = /^\\\\[^\\]+\\[^\\]+\\(.*)$/.exec(path);
  const linux = m ? m[1] : "";
  if (/^(proc|sys)(\\|$)/.test(linux)) return "Linux system information, not files on disk";
  const drive = /^mnt\\([a-z])(\\|$)/i.exec(linux);
  if (drive) return `Windows drive ${drive[1].toUpperCase()}: (open it from Drives to see folder sizes)`;
}

/** Ask for the sizes of a tab's folders (in display order); replaces that tab's earlier ask.
 *  `dir` is the folder the tab shows. */
export async function requestFolderSizes(tabId: number, paths: string[], dir?: string) {
  const known = await api.folderSizes(tabId, paths, false, dir).catch(() => [] as FolderSize[]);
  if (known.length) {
    setFolderSizes(produce((s) => known.forEach((k) => take(s, k))));
    resort();
  }
}

/** Bumped when sizes were marked out of date, so open tabs ask again. */
export const [sizesEpoch, setSizesEpoch] = createSignal(0);

/** Something changed inside these folders (and, with `deep`, anywhere below them). */
export async function invalidateFolderSizes(paths: string[], deep = false) {
  if (!paths.length) return;
  await api.folderSizesInvalidate(paths, deep).catch(() => {});
  setSizesEpoch((n) => n + 1);
}
