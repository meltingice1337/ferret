// Folder sizes for the Size column: counted in the background by the backend (foldersize.rs),
// which reports them as `folder-size` events, also while still counting.
import { listen } from "@tauri-apps/api/event";
import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { api } from "./api";
import { isRecycle, isWsl, pathKey } from "./path";
import type { FolderSize } from "./types";

export const [folderSizes, setFolderSizes] = createStore<Record<string, FolderSize>>({});

/** A new count only replaces a finished one once it's finished too, so a refresh doesn't
 *  show the size counting up from zero again. */
function take(s: Record<string, FolderSize>, size: FolderSize) {
  const k = pathKey(size.path);
  if (size.done || !s[k]?.done) s[k] = size;
}

listen<FolderSize>("folder-size", (ev) => setFolderSizes(produce((s) => take(s, ev.payload))));

export const folderSize = (path: string): FolderSize | undefined => folderSizes[pathKey(path)];

/** Network shares and Linux folders are slow to walk, so they aren't counted. */
export const sizesApply = (dir: string) => !isRecycle(dir) && !dir.startsWith("\\\\") && !isWsl(dir);

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
