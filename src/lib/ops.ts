// High-level file actions shared by menus, keyboard shortcuts and drag & drop.
// Every mutating action records an undo step where Windows allows one.

import { batch, createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api } from "./api";
import { entryFilter } from "./filter";
import { invalidateFolderSizes } from "./foldersizes";
import { plural } from "./format";
import { isVirtual, basename, inRecycleBin, isArchiveName, isRecycle, isSearch, isWithin, parent, pathKey, samePath } from "./path";
import {
  otherPane, setJobsRunning,
  activeTab, allPanes, ask, ASK_ALT, beginTypeAhead, dismissToast, elevateWindow, friendly, navigate, places, refreshRecycleInfo, reload, setUi,
  showToast, toast, ui, type Tab,
} from "./state";
import { formatSize } from "./format";
import type { Entry, JobUpdate, Policy } from "./types";

// ---------- jobs ----------

/** `currentSince`: when the job moved on to its current item (one that takes long gets a note). */
export const [jobs, setJobs] = createStore<Record<number, JobUpdate & { rate?: number; started?: number; currentSince?: number }>>({});
type JobDone = (u: JobUpdate) => void;
const jobCallbacks = new Map<number, JobDone>();
/** Final updates of recently finished jobs. Fast jobs can finish before `invoke`
 *  even returns their id, so `track` must be able to pick up a result that already arrived. */
const finished = new Map<number, JobUpdate>();
const speed = new Map<number, { t: number; b: number; rate: number }>();

listen<JobUpdate>("job-update", (ev) => {
  const u = ev.payload;
  // Smoothed throughput for the progress card.
  const now = performance.now();
  const s = speed.get(u.id) ?? { t: now, b: u.bytesDone, rate: 0 };
  if (now - s.t >= 400) {
    const r = ((u.bytesDone - s.b) / (now - s.t)) * 1000;
    s.rate = s.rate ? s.rate * 0.6 + r * 0.4 : r;
    s.t = now;
    s.b = u.bytesDone;
  }
  speed.set(u.id, s);
  const prev = jobs[u.id];
  const currentSince = prev && prev.current === u.current && prev.filesDone === u.filesDone ? prev.currentSince ?? now : now;
  setJobs(u.id, { ...u, rate: s.rate, started: prev?.started ?? now, currentSince });
  setJobsRunning(Object.values(jobs).filter((j) => j.status === "running" || j.status === "waiting").length);
  // "waiting" (paused on an item, asking in the jobs panel) isn't the end either.
  if (u.status !== "running" && u.status !== "waiting") {
    speed.delete(u.id);
    finished.set(u.id, u);
    setTimeout(() => finished.delete(u.id), 120_000);
    const cb = jobCallbacks.get(u.id);
    jobCallbacks.delete(u.id);
    // Password prompts are handled by the caller; everything else is reported here.
    if (u.status === "error" && !quietJobs && !/password/.test(u.error ?? "") && !u.error?.startsWith(ADMIN)) toast(`${u.label} failed: ${u.error}`, "error");
    if (u.status === "cancelled") toast(`Cancelled: ${u.label}`);
    cb?.(u);
    // A finished job ticks to "Done" and leaves: the toast (with Undo) reports the result.
    // Failures stay a while longer so their details can be read.
    setTimeout(() => setJobs(produce((j) => { delete j[u.id]; })), u.status === "error" ? 8000 : 500);
    refreshAffected();
  }
});

function track(id: number, done?: JobDone) {
  if (!done) return id;
  const f = finished.get(id);
  if (f) done(f);
  else jobCallbacks.set(id, done);
  return id;
}

/** Paths operations touched since the last refresh: sizes of the folders holding them are out of date. */
const touched = new Set<string>();
let touchTimer: number | undefined;
function touch(...paths: (string | null | undefined)[]) {
  paths.forEach((p) => p && touched.add(p));
  // Operations that don't end in refreshAffected (rename, new folder…) still get counted again.
  clearTimeout(touchTimer);
  touchTimer = window.setTimeout(() => { invalidateFolderSizes([...touched]); touched.clear(); }, 1500);
}

/** Re-lists every open tab (watchers catch most changes; this covers WSL/network) and marks
 *  changed folder sizes out of date. `everywhere`: the change's place isn't known (undo/redo). */
function refreshAffected(everywhere = false) {
  for (const t of allPanes()) reload(t);
  refreshRecycleInfo();
  if (everywhere) invalidateFolderSizes(allPanes().map((t) => t.p), true);
  else invalidateFolderSizes([...touched]);
  touched.clear();
}

// ---------- undo journal ----------

interface UndoStep {
  label: string;
  undo: () => Promise<unknown>;
  redo?: () => Promise<unknown>;
  /** The toast that offered to undo this step; it goes once the step is undone. */
  toast?: number;
}

const undoStack: UndoStep[] = [];
const redoStack: UndoStep[] = [];
export const [undoLabel, setUndoLabel] = createSignal<string | null>(null);
export const [redoLabel, setRedoLabel] = createSignal<string | null>(null);

function syncLabels() {
  setUndoLabel(undoStack.at(-1)?.label ?? null);
  setRedoLabel(redoStack.at(-1)?.label ?? null);
}

function record(step: UndoStep, announce?: string) {
  undoStack.push(step);
  if (undoStack.length > 100) undoStack.shift();
  redoStack.length = 0;
  syncLabels();
  // The toast's Undo is for this step only: once later steps are on top of it, Ctrl+Z
  // (or the toast) would undo something else.
  if (announce) step.toast = showToast(announce, "info", {
    label: "Undo",
    run: () => (undoStack.at(-1) === step ? undo() : toast(undoStack.includes(step) ? "Undo the later changes first" : "Already undone")),
  });
}

/** Selects items an undo brought back, if they're in the folder being viewed. */
function selectRestored(result: unknown) {
  const paths = typeof result === "string" ? [result]
    : Array.isArray(result) ? result.filter((p): p is string => typeof p === "string")
    : result && typeof result === "object" && Array.isArray((result as JobUpdate).created) ? (result as JobUpdate).created : [];
  const t = activeTab();
  const here = paths.filter((p) => samePath(parent(p), t.p));
  if (here.length) selectAfterReload(t, here);
}

/** Operations still running. Undo/redo wait for them so a fast Ctrl+Z hits the right step. */
const inflight = new Set<Promise<unknown>>();
function tracked<T>(p: Promise<T>): Promise<T> {
  inflight.add(p);
  p.finally(() => inflight.delete(p)).catch(() => {});
  return p;
}
/** True once they're done; false if one is still going after a moment (a long copy, or a job
 *  waiting on the user), so undo says so instead of firing whenever that job ends. */
const settle = () => !inflight.size ? Promise.resolve(true)
  : Promise.race([Promise.allSettled([...inflight]).then(() => true), new Promise<boolean>((r) => setTimeout(() => r(false), 1500))]);
const BUSY_NOTE = "Wait for the operation in progress to finish (or cancel it), then try again";

/** Paths an operation is currently working on; repeated commands ignore them. */
const busy = new Set<string>();
export const isBusy = (p: string) => busy.has(pathKey(p));

/** While > 0, job failures are reported by the undo/redo caller instead of the job listener. */
let quietJobs = 0;
const reason = (e: unknown) => friendly(e instanceof JobEnded ? e.update.error ?? "failed" : String(e));

export async function undo() {
  if (!(await settle())) return toast(BUSY_NOTE);
  const step = undoStack.pop();
  syncLabels();
  if (!step) return toast("Nothing to undo");
  if (step.toast) dismissToast(step.toast);
  quietJobs++;
  try {
    const result = await step.undo();
    if (step.redo) redoStack.push(step);
    syncLabels();
    toast(`Undone: ${step.label}`);
    selectRestored(result);
  } catch (e) {
    toast(`Couldn't undo ${step.label}: ${reason(e)}`, "error");
  } finally {
    quietJobs--;
  }
  refreshAffected(true);
}

export async function redo() {
  if (!(await settle())) return toast(BUSY_NOTE);
  const step = redoStack.pop();
  syncLabels();
  if (!step?.redo) return toast("Nothing to redo");
  quietJobs++;
  try {
    await step.redo();
    undoStack.push(step);
    syncLabels();
    toast(`Redone: ${step.label}`);
  } catch (e) {
    toast(`Couldn't redo ${step.label}: ${reason(e)}`, "error");
  } finally {
    quietJobs--;
  }
  refreshAffected(true);
}

/** Runs a job to completion and resolves with its final update. */
/** A job that ran and failed or was cancelled; the job listener already told the user. */
class JobEnded extends Error {
  constructor(public update: JobUpdate) { super(update.error ?? update.status); }
}

function runJob(start: Promise<number>): Promise<JobUpdate> {
  return new Promise((resolve, reject) => {
    start.then((id) => track(id, (u) => (u.status === "done" ? resolve(u) : reject(new JobEnded(u))))).catch(reject);
  });
}

/** Paths that still exist (checked for up to 300 items; beyond that assume they do). */
async function stillThere(paths: string[]): Promise<string[]> {
  if (paths.length > 300) return paths;
  const checks = await Promise.all(paths.map((p) => api.resolveInput(p).then((r) => r.kind !== "missing").catch(() => true)));
  return paths.filter((_, i) => checks[i]);
}

/** Reports an action failure unless the job listener already did (or the user said no). */
function fail(what: string, e: unknown) {
  if (e instanceof JobEnded || e instanceof Declined) return;
  toast(`${what}: ${e}`, "error");
}

// ---------- administrator permission ----------

/** Errors from operations Windows only allows administrators to do start with this. */
const ADMIN = "elevate:";
/** The user declined to continue as administrator. */
class Declined extends Error {}

function adminReason(e: unknown): string | null {
  const m = e instanceof JobEnded ? e.update.error ?? "" : typeof e === "string" ? e : "";
  return m.startsWith(ADMIN) ? m.slice(ADMIN.length) : null;
}

/** Runs `normal`; when Windows says it needs administrator rights, asks first (like Explorer)
 *  and runs `elevated` instead, which goes through the shell and shows the UAC prompt. */
async function withAdmin<T>(normal: () => Promise<T>, elevated: () => Promise<T>): Promise<T> {
  try {
    return await normal();
  } catch (e) {
    const why = adminReason(e);
    if (why === null) throw e;
    const ok = await ask({
      title: "Administrator permission needed", shield: true, confirmLabel: "Just this action", alt: "Elevate window",
      message: `${why}. “Just this action” asks Windows once, for this operation only. “Elevate window” restarts this window as administrator (same tabs); then do it again there.`,
    });
    if (ok === ASK_ALT) {
      elevateWindow();
      throw new Declined();
    }
    if (ok === null) throw new Declined();
    return elevated();
  }
}

const shellJob = (op: import("./types").ShellOp) => runJob(api.shellOp(op, true));
const recycle = (paths: string[]) =>
  withAdmin(() => runJob(api.deleteItems(paths, false)), () => shellJob({ kind: "delete", items: paths, permanent: false }));
const destroy = (paths: string[]) =>
  withAdmin(() => runJob(api.deleteItems(paths, true)), () => shellJob({ kind: "delete", items: paths, permanent: true }));
/** Brings recycled items back by their original paths. */
const unrecycle = (paths: string[]) =>
  withAdmin(() => api.restoreFromTrash(paths), async () => (await shellJob({ kind: "restore", items: await api.findInTrash(paths) })).created);
const moveBack = (pairs: [string, string][]) =>
  withAdmin(() => runJob(api.undoMoves(pairs)), () => shellJob({ kind: "moveBack", pairs }));
const renameTo = (path: string, name: string) =>
  withAdmin(() => api.renameItem(path, name), async () => (await shellJob({ kind: "rename", path, newName: name })).created[0] ?? path);

/** Recycle Bin items stay put: they can only be restored or deleted. */
function binGuard(paths: string[], what: string): boolean {
  if (!paths.some(inRecycleBin)) return false;
  toast(`Items in the Recycle Bin can't be ${what}. Restore them first.`, "error");
  return true;
}

// ---------- helpers ----------

const describe = (paths: string[]) => (paths.length === 1 ? `“${basename(paths[0])}”` : plural(paths.length, "item"));

/** Selected entries of a tab. As in Explorer, the focused item alone (Ctrl+arrows, Esc) isn't a target. */
export function targetsOf(t: Tab): Entry[] {
  if (t.loading[0]()) return [];
  const sel = new Set(t.selection[0]().map(pathKey));
  if (!sel.size) return [];
  const shown = t.ordered[0]();
  const entries = (shown.length ? shown : visibleEntries(t)).filter((e) => !busy.has(pathKey(e.path)));
  return entries.filter((e) => sel.has(pathKey(e.path)));
}

/** Entries currently shown (respects the filter box and hidden-items setting). */
export function visibleEntries(t: Tab): Entry[] {
  const match = entryFilter(t.filter[0]());
  const showHidden = t.view[0]().showHidden;
  return t.entries[0]().filter((e) => (showHidden || !e.hidden) && (!match || match(e)));
}

/** Keeps Dropzone entries pointing at files after they move (old → new path). */
function remapShelf(map: Map<string, string>) {
  if (!ui.shelf.length || !map.size) return;
  setUi("shelf", ui.shelf.map((p) => map.get(pathKey(p)) ?? p));
}

function selectAfterReload(t: Tab, paths: string[], rename = false) {
  reload(t).then(() => {
    batch(() => {
      t.selection[1](paths);
      t.cursor[1](paths[0] ?? null);
      t.anchor[1](paths[0] ?? null);
      if (rename && paths[0]) t.renaming[1](paths[0]);
      t.reveal[1]((n) => n + 1);
    });
  });
}

const inArchive = (paths: string[]) => paths.some((p) => /\.(zip|7z|rar|tar|tgz|gz|xz|zst|bz2|jar|apk|cbz|cbr)\\/i.test(p) || p.includes(".zip\\"));

// ---------- clipboard ----------

/** In-flight clipboard write; paste waits for it so a fast Ctrl+C → Ctrl+V never reads stale contents. */
let clipboardWrite: Promise<unknown> = Promise.resolve();

export async function copyToClipboard(paths: string[], cut: boolean) {
  if (!paths.length || binGuard(paths, cut ? "cut" : "copied")) return;
  if (cut && inArchive(paths)) return toast("Items inside an archive can't be cut. Copy them instead.", "error");
  try {
    const write = api.clipboardWriteFiles(paths, cut);
    clipboardWrite = write.catch(() => {});
    await write;
    setUi("clipboard", { paths, cut });
    toast(`${cut ? "Cut" : "Copied"} ${describe(paths)} to the clipboard`);
  } catch (e) {
    toast(`Clipboard error: ${e}`, "error");
  }
}

/** A search tab lists files from many folders: nothing goes "into" it. */
const SEARCH_TAB_NOTE = "Search results aren't a folder. Open a folder to add items to it";

export async function paste(dest: string) {
  if (isRecycle(dest)) return toast("To delete files, drop them on the Recycle Bin or press Delete", "error");
  if (isSearch(dest)) return toast(SEARCH_TAB_NOTE, "error");
  let clip;
  try {
    await clipboardWrite;
    clip = await api.clipboardReadFiles();
  } catch (e) {
    return toast(`Clipboard error: ${e}`, "error");
  }
  if (!clip || !clip.paths.length) return toast("Nothing to paste: the clipboard has no files");
  if (!clip.cut) return transferTo("copy", clip.paths, dest);
  // A cut that was already pasted (here or in Explorer) has nothing left to move.
  const sources = await stillThere(clip.paths);
  if (sources.length) await transferTo("move", sources, dest);
  // Like Explorer, a pasted cut leaves the clipboard; what wasn't moved (skipped, cancelled) stays cut.
  const left = sources.length ? await stillThere(sources) : [];
  const ours = await api.clipboardAfterCut(clip.paths, left).catch(() => true);
  if (ours) setUi("clipboard", left.length ? { paths: left, cut: true } : null);
  if (!sources.length) toast("Nothing to paste: the cut items have already been moved");
}

export const isCut = (path: string) => !!ui.clipboard?.cut && ui.clipboard.paths.some((p) => samePath(p, path));

// ---------- copy / move ----------

function askConflict(conflicts: import("./types").Conflict[], op: "copy" | "move", dest: string): Promise<Policy | null> {
  return new Promise((resolve) => setUi("conflict", { conflicts, op, dest, resolve }));
}

/** Split view: copies or moves the selection into the other pane's folder. */
export function toOtherPane(op: "copy" | "move", t = activeTab()) {
  const dest = otherPane();
  const paths = targetsOf(t).map((x) => x.path);
  if (!dest || !paths.length) return;
  if (isVirtual(dest.p)) return toast("The other pane doesn't show a folder");
  if (samePath(dest.p, t.p)) return toast("Both panes show the same folder");
  return transferTo(op, paths, dest.p);
}

export async function transferTo(op: "copy" | "move", sources: string[], dest: string): Promise<void> {
  // Dropping onto the Recycle Bin deletes.
  if (isRecycle(dest)) return deleteItems(sources, false);
  if (isSearch(dest)) return toast(SEARCH_TAB_NOTE, "error");
  touch(dest, ...(op === "move" ? sources : []));
  if (binGuard(sources, op === "move" ? "moved" : "copied")) return;
  sources = sources.filter((s) => !(op === "move" && samePath(parent(s), dest)));
  if (!sources.length) return;
  if (sources.some((s) => isWithin(dest, s))) return toast("Can't put a folder inside itself", "error");

  // Items inside archives are extracted instead of copied.
  if (inArchive(sources)) {
    const archive = sources[0];
    return extractTo(archive, dest, sources);
  }

  let policy: Policy = "keepBoth";
  const conflicts = await api.checkConflicts(sources, dest).catch(() => []);
  if (conflicts.length) {
    const p = await askConflict(conflicts, op, dest);
    if (!p) return;
    policy = p;
  }
  const asked = `${op === "move" ? "Move" : "Copy"} ${describe(sources)} to ${basename(dest)}`;
  try {
    const u = await tracked(withAdmin(
      () => runJob(api.transfer(op, sources, dest, policy)),
      () => shellJob({ kind: "transfer", op, sources, dest, policy }),
    ));
    const created = u.created;
    const t = activeTab();
    // Items the user skipped (open in another program…) stay where they were, selected there.
    const failed = (u.failed ?? []).map(([p]) => p);
    const failedKeys = new Set(failed.map(pathKey));
    const done = sources.filter((s) => !failedKeys.has(pathKey(s)));
    if (samePath(t.p, dest)) selectAfterReload(t, created);
    else if (failed.length && samePath(t.p, parent(failed[0]))) selectAfterReload(t, failed);
    // Everything skipped: say so, and there's nothing to undo.
    if (!created.length && !(u.replaced ?? []).length && !(u.pairs ?? []).length) {
      toast(failed.length ? `Nothing was ${op === "move" ? "moved" : "copied"} · ${plural(failed.length, "item")} skipped`
        : u.skipped ? `Skipped ${plural(u.skipped, "item")}: already in ${basename(dest)}` : "Nothing to do");
      return;
    }
    const skipped = u.skipped + failed.length;
    const skippedNote = skipped ? ` · skipped ${skipped}` : "";
    // The undo step names what was actually done, not what was asked for.
    const label = failed.length ? `${op === "move" ? "Move" : "Copy"} ${describe(done)} to ${basename(dest)}` : asked;
    const replaced = u.replaced ?? [];
    const pairs = u.pairs ?? [];
    const restoreReplaced = () => (replaced.length ? unrecycle(replaced) : Promise.resolve([]));
    if (op === "copy") {
      record({
        label,
        undo: async () => {
          // Copies sitting where a replaced original was are removed for good (the source
          // still exists), so the original is the item restored from the Recycle Bin.
          const rk = new Set(replaced.map(pathKey));
          const existing = await stillThere(created);
          const fresh = existing.filter((p) => !rk.has(pathKey(p)));
          const overwrote = existing.filter((p) => rk.has(pathKey(p)));
          if (!existing.length && !replaced.length) throw new Error("the copies no longer exist");
          if (fresh.length) await recycle(fresh);
          if (overwrote.length) await destroy(overwrote);
          await restoreReplaced();
        },
        redo: () => runJob(api.transfer("copy", done, dest, policy)),
      }, `Copied ${describe(done)} to ${basename(dest)}${skippedNote}`);
    } else {
      remapShelf(new Map(pairs.map(([from, to]) => [pathKey(from), to])));
      record({
        label,
        undo: async () => {
          const back = await moveBack(pairs);
          remapShelf(new Map(pairs.map(([from, to]) => [pathKey(to), from])));
          await restoreReplaced();
          return back;
        },
        redo: () => runJob(api.transfer("move", done, dest, policy)),
      }, `Moved ${describe(done)} to ${basename(dest)}${skippedNote}`);
    }
  } catch (e) {
    fail(`${asked} failed`, e);
  }
}

export async function duplicate(paths: string[]) {
  if (!paths.length) return;
  const dir = parent(paths[0]);
  if (!dir) return;
  await transferTo("copy", paths, dir);
}

/** "Copy to…" / "Move to…" with a folder picker. */
export async function transferWithPicker(op: "copy" | "move", paths: string[]) {
  const dest = await openDialog({ directory: true, title: `${op === "copy" ? "Copy" : "Move"} ${describe(paths)} to…`, defaultPath: parent(paths[0]) ?? undefined });
  if (typeof dest === "string") await transferTo(op, paths, dest);
}

// ---------- delete ----------

export async function deleteItems(paths: string[], permanent: boolean) {
  paths = paths.filter((p) => !busy.has(pathKey(p)));
  if (!paths.length) return;
  touch(...paths);
  // Deleting inside the Recycle Bin is for good.
  if (paths.some(inRecycleBin)) return purgeFromBin(paths.filter(inRecycleBin));
  if (inArchive(paths)) return toast("Deleting inside archives isn't supported yet", "error");
  if (permanent) {
    const ok = await ask({ title: `Permanently delete ${describe(paths)}?`, message: "This can't be undone. The items won't go to the Recycle Bin.", confirmLabel: "Delete permanently", danger: true });
    if (ok === null) return;
  }
  const t = activeTab();
  // Explorer selects the item after the deleted ones (or before, at the end of the list).
  const order = t.ordered[0]();
  const gone = new Set(paths.map(pathKey));
  const firstIdx = order.findIndex((e) => gone.has(pathKey(e.path)));
  const next = order.slice(Math.max(0, firstIdx)).find((e) => !gone.has(pathKey(e.path)))
    ?? [...order.slice(0, Math.max(0, firstIdx))].reverse().find((e) => !gone.has(pathKey(e.path)));
  // Mark busy and move the selection on immediately, so a second Delete acts on the next item.
  paths.forEach((p) => busy.add(pathKey(p)));
  batch(() => {
    t.selection[1](next ? [next.path] : []);
    t.cursor[1](next?.path ?? null);
    t.anchor[1](next?.path ?? null);
  });
  try {
    const u = await tracked(permanent ? destroy(paths) : recycle(paths));
    // Items skipped after "Skip" / "Skip all" stay where they were.
    const left = (u?.failed ?? []).map(([p]) => p);
    const leftKeys = new Set(left.map(pathKey));
    const done = paths.filter((p) => !leftKeys.has(pathKey(p)));
    const skippedNote = left.length ? ` · ${plural(left.length, "item")} skipped` : "";
    if (!done.length) toast(`Nothing was ${permanent ? "deleted" : "moved to the Recycle Bin"}${skippedNote}`);
    else if (permanent) toast(`Deleted ${describe(done)}${skippedNote}`);
    else
      record({
        label: `Delete ${describe(done)}`,
        undo: () => unrecycle(done),
        redo: () => recycle(done),
      }, `Moved ${describe(done)} to the Recycle Bin${skippedNote}`);
    // The skipped items get selected, so it's clear what's left (and easy to try again).
    if (left.length) selectAfterReload(t, left);
    else if (next && !t.selection[0]().length) selectAfterReload(t, [next.path]);
    if (ui.shelf.some((p) => gone.has(pathKey(p)))) setUi("shelf", ui.shelf.filter((p) => !gone.has(pathKey(p))));
  } catch (e) {
    fail("Delete failed", e);
    // Put the selection back on whatever is still there (e.g. a file locked by another program).
    const still = await stillThere(paths);
    if (still.length) selectAfterReload(t, still);
  } finally {
    paths.forEach((p) => busy.delete(pathKey(p)));
  }
}

// ---------- rename ----------

/** Renames and records undo. Throws the error message (inline rename shows it in place). */
export async function renamePath(path: string, newName: string): Promise<string> {
  if (newName === basename(path)) return path;
  touch(path);
  const to = await tracked(renameTo(path, newName));
  const oldName = basename(path);
  record({
    label: `Rename to “${newName}”`,
    undo: () => renameTo(to, oldName),
    redo: () => renameTo(path, newName),
  });
  return to;
}

export async function batchRename(pairs: { from: string; to: string }[]) {
  touch(...pairs.map((p) => p.from));
  const done: { from: string; to: string }[] = [];
  for (const p of pairs) {
    if (basename(p.from) === p.to) continue;
    try {
      const to = await renameTo(p.from, p.to);
      done.push({ from: p.from, to });
    } catch (e) {
      if (e instanceof Declined) break;
      toast(`${basename(p.from)}: ${friendly(String(e))}`, "error");
    }
  }
  if (done.length) {
    record({
      label: `Rename ${plural(done.length, "item")}`,
      undo: async () => { for (const d of [...done].reverse()) await renameTo(d.to, basename(d.from)); },
      redo: async () => { for (const d of done) await renameTo(d.from, basename(d.to)); },
    }, `Renamed ${plural(done.length, "item")}`);
  }
  refreshAffected();
  return done;
}

export function startRename(t: Tab) {
  const targets = targetsOf(t);
  if (!targets.length || binGuard(targets.map((e) => e.path), "renamed")) return;
  if (targets.length > 1) return setUi("batchRename", targets.map((e) => e.path));
  if (targets[0].inArchive) return toast("Items inside an archive can't be renamed", "error");
  t.renaming[1](targets[0].path);
}

// ---------- create ----------

export async function newFolder(t: Tab) {
  if (inArchive([t.p + "\\"])) return toast("Can't create items inside an archive", "error");
  if (isRecycle(t.p)) return toast("Can't create items in the Recycle Bin", "error");
  if (isSearch(t.p)) return toast(SEARCH_TAB_NOTE, "error");
  beginTypeAhead();
  try {
    const p = await tracked(withAdmin(
      () => api.createFolder(t.p),
      async () => (await shellJob({ kind: "newFolder", dir: t.p, name: "New folder" })).created[0],
    ));
    // Still empty when undone: removed outright (nothing to lose), and redo makes it again.
    // With something in it, it goes to the Recycle Bin and redo brings it back.
    let removed = false;
    record({
      label: "New folder",
      undo: async () => {
        removed = await api.removeEmptyFolder(p);
        if (!removed) await recycle([p]);
      },
      redo: () => (removed ? api.createFolder(parent(p)!, basename(p)) : unrecycle([p])),
    });
    selectAfterReload(t, [p], true);
  } catch (e) {
    if (!(e instanceof Declined)) toast(friendly(String(e)), "error");
  }
}

export const NEW_FILE_TYPES = [
  { label: "Text document", name: "New Text Document.txt" },
  { label: "Markdown", name: "New Document.md" },
  { label: "JSON", name: "data.json", content: "{\n  \n}\n" },
  { label: "Empty file", name: "New File" },
];

export async function newFile(t: Tab, name: string, content?: string) {
  if (isRecycle(t.p)) return toast("Can't create items in the Recycle Bin", "error");
  if (isSearch(t.p)) return toast(SEARCH_TAB_NOTE, "error");
  touch(t.p);
  beginTypeAhead();
  try {
    const p = await tracked(withAdmin(
      () => api.createFile(t.p, name, content),
      async () => (await shellJob({ kind: "newFile", dir: t.p, name })).created[0],
    ));
    record({ label: `New ${name}`, undo: () => recycle([p]), redo: () => unrecycle([p]) });
    selectAfterReload(t, [p], true);
  } catch (e) {
    if (!(e instanceof Declined)) toast(friendly(String(e)), "error");
  }
}

// ---------- Recycle Bin ----------

/** Puts Recycle Bin items (their `$Recycle.Bin` paths) back where they were deleted from. */
export async function restoreFromBin(items: string[]) {
  if (!items.length) return;
  // Something with the same name is back where an item came from: Windows would restore it
  // under a new name, so ask first rather than doing that without a word.
  const binned = activeTab().entries[0]().filter((e) => e.origin && items.some((p) => samePath(p, e.path)));
  const originals = binned.map((e) => `${e.origin!.replace(/\\$/, "")}\\${e.name}`);
  // Taken by something there now, or by another item restored with this one (deleted twice).
  const twice = originals.filter((p, i) => originals.findIndex((q) => samePath(p, q)) !== i);
  const taken = [...(await stillThere(originals)), ...twice];
  if (taken.length) {
    const one = taken.length === 1;
    const name = basename(taken[0]);
    const dot = binned[originals.findIndex((p) => samePath(p, taken[0]))]?.isDir ? -1 : name.lastIndexOf(".");
    const copyName = dot > 0 ? `${name.slice(0, dot)} - Copy${name.slice(dot)}` : `${name} - Copy`;
    const ok = await ask({
      title: one ? `“${name}” is already in ${basename(parent(taken[0]) ?? "")}` : `${plural(taken.length, "item")} already have their name taken where they were`,
      message: `Ferret can restore ${one ? "it" : "them"} next to what's there now, under ${one ? "a new name" : "new names"} like “${copyName}”. Nothing there is replaced.`,
      confirmLabel: "Keep both",
    });
    if (ok === null) return;
  }
  try {
    const u = await tracked(withAdmin(
      () => runJob(api.shellOp({ kind: "restore", items }, false)),
      () => shellJob({ kind: "restore", items }),
    ));
    const back = u.created;
    touch(...back);
    record({
      label: `Restore ${plural(back.length, "item")}`,
      undo: () => recycle(back),
      redo: () => unrecycle(back),
    }, back.length === 1 ? `Restored “${basename(back[0])}” to ${basename(parent(back[0]) ?? "")}` : `Restored ${plural(back.length, "item")}`);
  } catch (e) {
    fail("Restore failed", e);
  }
}

/** Deletes Recycle Bin items for good, after asking. */
export async function purgeFromBin(items: string[]) {
  if (!items.length) return;
  const names = activeTab().entries[0]().filter((e) => items.some((p) => samePath(p, e.path))).map((e) => e.name);
  const what = items.length === 1 && names[0] ? `“${names[0]}”` : plural(items.length, "item");
  const ok = await ask({ title: `Permanently delete ${what}?`, message: "This can't be undone.", confirmLabel: "Delete permanently", danger: true });
  if (ok === null) return;
  items.forEach((p) => busy.add(pathKey(p)));
  try {
    await tracked(withAdmin(
      () => runJob(api.shellOp({ kind: "purge", items }, false)),
      () => shellJob({ kind: "purge", items }),
    ));
    toast(`Deleted ${what}`);
  } catch (e) {
    fail("Delete failed", e);
  } finally {
    items.forEach((p) => busy.delete(pathKey(p)));
  }
}

export async function emptyBin() {
  const info = await api.recycleInfo().catch(() => places().recycle);
  if (!info.items) return toast("The Recycle Bin is already empty");
  const ok = await ask({
    title: "Empty the Recycle Bin?",
    message: `${plural(info.items, "item")} (${formatSize(info.bytes)}) will be deleted permanently. This can't be undone.`,
    confirmLabel: "Empty Recycle Bin", danger: true,
  });
  if (ok === null) return;
  // A job like any delete: progress, Cancel, and Try again / Skip for an item it can't delete.
  try {
    const u = await tracked(runJob(api.emptyRecycleBin()));
    const left = u.failed?.length ?? 0;
    toast(left ? `Emptied the Recycle Bin · ${plural(left, "item")} skipped` : "Emptied the Recycle Bin");
  } catch (e) {
    fail("Couldn't empty the Recycle Bin", e);
  }
  refreshAffected();
}

// ---------- archives ----------

async function withPassword<T>(fn: (pw?: string) => Promise<T>): Promise<T | undefined> {
  try {
    return await fn();
  } catch (err) {
    const e = err instanceof JobEnded ? err.update.error : err;
    if (e !== "password-required" && e !== "wrong-password") throw err;
    const pw = await ask({
      title: e === "wrong-password" ? "Wrong password" : "Password required",
      message: e === "wrong-password" ? "That password didn't work. Try again:" : "This archive is encrypted. Enter its password:",
      input: "", password: true, confirmLabel: "Unlock",
    });
    if (!pw) return undefined;
    return withPassword(() => fn(pw));
  }
}

export async function extractSmart(archive: string) {
  touch(parent(archive));
  try {
    const u = await withPassword((pw) => runJob(api.archiveExtractSmart(archive, pw)));
    if (!u) return;
    const t = activeTab();
    if (samePath(t.p, parent(archive))) selectAfterReload(t, u.created);
    record({ label: `Extract ${basename(archive)}`, undo: () => runJob(api.deleteItems(u.created, false)) }, `Extracted ${basename(archive)}`);
  } catch (e) {
    fail("Extract failed", e);
  }
}

export async function extractTo(archive: string, dest?: string, entries?: string[]) {
  if (!dest) {
    const picked = await openDialog({ directory: true, title: `Extract ${basename(archive)} to…`, defaultPath: parent(archive) ?? undefined });
    if (typeof picked !== "string") return;
    dest = picked;
  }
  const target = dest;
  touch(target);
  try {
    const u = await withPassword((pw) => runJob(api.archiveExtract(archive, target, entries, pw)));
    if (!u) return;
    const t = activeTab();
    if (samePath(t.p, target)) selectAfterReload(t, u.created);
    record({ label: `Extract to ${basename(target)}`, undo: () => runJob(api.deleteItems(u.created, false)) }, `Extracted to ${basename(target)}`);
  } catch (e) {
    fail("Extract failed", e);
  }
}

/** Name "Compress to ZIP" will use (unique within the folder). */
export function zipNameFor(paths: string[]): string {
  const dir = parent(paths[0]) ?? "";
  const stem = paths.length === 1 ? basename(paths[0]).replace(/(\.tar)?\.[^.\\]+$/i, "") : basename(dir) || "Archive";
  let name = `${stem}.zip`;
  const existing = new Set(activeTab().entries[0]().map((e) => e.name.toLowerCase()));
  for (let i = 2; existing.has(name.toLowerCase()); i++) name = `${stem} (${i}).zip`;
  return name;
}

export async function compress(paths: string[]) {
  if (!paths.length) return;
  const dir = parent(paths[0])!;
  touch(dir);
  const name = zipNameFor(paths);
  const dest = `${dir.replace(/\\$/, "")}\\${name}`;
  try {
    const u = await runJob(api.compressZip(paths, dest));
    const t = activeTab();
    if (samePath(t.p, dir)) selectAfterReload(t, u.created.length ? u.created : [dest], true);
    record({ label: `Compress to ${name}`, undo: () => runJob(api.deleteItems([dest], false)) });
  } catch (e) {
    fail("Compress failed", e);
  }
}

/** Runs a compress job from the video / picture editor and says how much it saved. */
export async function compressMedia(kind: "video" | "image", start: Promise<number>, sources: string[], sourceBytes: number) {
  const dir = parent(sources[0])!;
  touch(dir);
  try {
    const u = await runJob(start);
    const t = activeTab();
    if (samePath(t.p, dir)) selectAfterReload(t, u.created);
    const after = await api.folderStats(u.created).then((s) => s.bytes).catch(() => 0);
    const what = u.created.length === 1 ? basename(u.created[0]) : plural(u.created.length, kind === "video" ? "video" : "picture");
    const msg = !after ? `Saved ${what}`
      : after >= sourceBytes ? `${what} came out ${formatSize(after)}, not smaller than the original`
      : `${what}: ${formatSize(sourceBytes)} → ${formatSize(after)}, ${Math.round((1 - after / sourceBytes) * 100)}% smaller`;
    record({ label: `Compress ${what}`, undo: () => runJob(api.deleteItems(u.created, false)) }, msg);
  } catch (e) {
    fail("Compress failed", e);
  }
}

/** Real filesystem path for an entry (archive entries are extracted to a cache first). */
export async function realPath(e: Pick<Entry, "path" | "inArchive">): Promise<string | undefined> {
  if (!e.inArchive) return e.path;
  try {
    return await withPassword((pw) => api.archiveMaterialize(e.path, pw));
  } catch (err) {
    toast(`Couldn't open from archive: ${err}`, "error");
  }
}

export async function openFile(e: Pick<Entry, "path" | "inArchive">) {
  // Archives open as folders (the preview's Open button, Ctrl+K): their default app is
  // Explorer's zip folder, which with Ferret as the default file manager becomes a new Ferret
  // window after a slow detour through Explorer.
  if (isArchiveName(e.path)) return void navigate(activeTab(), e.path);
  const p = await realPath(e);
  if (p) api.openPath(p).catch((err) => toast(String(err), "error"));
}

// ---------- shell ----------

export async function openTerminal(path: string) {
  api.openTerminal(path).catch((e) => toast(`Couldn't open terminal: ${e}`, "error"));
}

export async function openInVscode(path: string) {
  api.openInVscode(path).catch((e) => toast(String(e), "error"));
}

export async function copyPaths(paths: string[], quoted = true) {
  await copyText(paths.map((p) => (quoted ? `"${p}"` : p)).join("\r\n"));
  toast(paths.length === 1 ? "Path copied" : `${paths.length} paths copied`);
}

/** Puts text on the clipboard, which replaces any files cut or copied there. */
export async function copyText(text: string) {
  await navigator.clipboard.writeText(text);
  setUi("clipboard", null);
}

/** Drops the "cut" look once the cut items are no longer on the clipboard (something else was
 *  copied, here or in another app). Runs when the window gets the focus back. */
export async function syncClipboard() {
  const mine = ui.clipboard;
  if (!mine) return;
  const clip = await api.clipboardReadFiles().catch(() => undefined);
  if (clip === undefined || ui.clipboard !== mine) return;
  const same = !!clip && clip.cut === mine.cut && clip.paths.length === mine.paths.length
    && mine.paths.every((p) => clip.paths.some((q) => samePath(p, q)));
  if (!same) setUi("clipboard", null);
}

export async function showNativeMenu(t: Tab, paths: string[], x: number, y: number) {
  try {
    const verb = paths.length ? await api.showNativeMenu(paths, x, y) : await api.showNativeBackgroundMenu(t.p, x, y);
    if (verb === "rename") startRename(t);
    else if (verb === "open" && paths.length) {
      navigate(t, paths[0]);
      paths.slice(1).forEach((p) => import("./state").then((s) => s.newTab(p, false)));
    } else if (verb !== null) setTimeout(() => reload(t), 400);
  } catch (e) {
    toast(String(e), "error");
  }
}

/** Opens a folder in the active tab or a file with its default app. */
export function openEntryAnywhere(e: Entry, inNewTab = false) {
  if (e.isDir) {
    if (inNewTab) import("./state").then((s) => s.newTab(e.path, false));
    else navigate(activeTab(), e.path);
  } else openFile(e);
}
