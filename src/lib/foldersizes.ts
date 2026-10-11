// Folder sizes for the Size column: counted in the background by the backend (foldersize.rs),
// which reports them as `folder-size` events, also while still counting.
import { listen } from "@tauri-apps/api/event";
import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { api } from "./api";
import { isRecycle, isWsl, pathKey } from "./path";
import type { FolderSize } from "./types";

export const [folderSizes, setFolderSizes] = createStore<Record<string, FolderSize>>({});

/** A new count only replaces a finished one once it's finished too, so a refresh shows the
 *  old size (marked as refreshing) rather than counting up from zero again. An older count
 *  never replaces a newer one: the finished recount can arrive before the reply that said
 *  it was starting. */
function take(s: Record<string, FolderSize>, size: FolderSize) {
  if (uncounted(size.path)) return;
  const k = pathKey(size.path);
  const old = s[k];
  if (!old?.done || (size.done && size.countedAt >= old.countedAt)) s[k] = size;
}

listen<FolderSize>("folder-size", (ev) => setFolderSizes(produce((s) => take(s, ev.payload))));

export const folderSize = (path: string): FolderSize | undefined => folderSizes[pathKey(path)];

/** Network shares are slow to walk, so they aren't counted. (WSL folders are counted by
 *  Linux itself, which is fast.) */
export const sizesApply = (dir: string) => !isRecycle(dir) && (isWsl(dir) || !dir.startsWith("\\\\"));

/** Why a WSL folder gets no size, if it doesn't: Linux's /proc and /sys describe the running
 *  system rather than hold files (/proc alone "contains" 128 TB), and /mnt/c is the Windows
 *  drive itself, very slow to walk from Linux and counted directly when opened as C:. */
export function uncounted(path: string): string | undefined {
  if (!isWsl(path)) return;
  const m = /^\\\\[^\\]+\\[^\\]+\\(.*)$/.exec(path);
  const linux = m ? m[1] : "";
  if (/^(proc|sys)(\\|$)/.test(linux)) return "Linux system information, not files on disk";
  const drive = /^mnt\\([a-z])(\\|$)/i.exec(linux);
  if (drive) return `Windows drive ${drive[1].toUpperCase()}: (open it from Drives to see folder sizes)`;
}

/** Ask for the sizes of a tab's folders (in display order); replaces that tab's earlier ask. */
export async function requestFolderSizes(tabId: number, paths: string[]) {
  const known = await api.folderSizes(tabId, paths).catch(() => [] as FolderSize[]);
  if (known.length) setFolderSizes(produce((s) => known.forEach((k) => take(s, k))));
}

/** Bumped when sizes were marked out of date, so open tabs ask again. */
export const [sizesEpoch, setSizesEpoch] = createSignal(0);

/** Something changed inside these folders (and, with `deep`, anywhere below them). */
export async function invalidateFolderSizes(paths: string[], deep = false) {
  if (!paths.length) return;
  await api.folderSizesInvalidate(paths, deep).catch(() => {});
  setSizesEpoch((n) => n + 1);
}
