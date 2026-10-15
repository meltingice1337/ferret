// Self-update from GitHub Releases (tauri-plugin-updater). Ferret checks shortly after
// startup and every few hours, then offers the update; nothing installs without a click.
// The installer is signed with the release key, and the plugin rejects anything else.
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { createStore } from "solid-js/store";
import { describeError, log } from "./log";
import { toast } from "./state";

const FIRST_CHECK_MS = 15_000;
const CHECK_EVERY_MS = 6 * 60 * 60 * 1000;

export type UpdatePhase = "idle" | "checking" | "available" | "downloading" | "installing" | "error";

export const [updater, setUpdater] = createStore({
  current: "",
  phase: "idle" as UpdatePhase,
  version: "",
  notes: "",
  /** Release date of the offered version (YYYY-MM-DD). */
  date: "",
  /** The "What's new" dialog is open. */
  notesOpen: false,
  done: 0,
  total: 0,
  error: "",
  /** The user chose "Later" for this version; the card stays hidden until the next one. */
  dismissed: "",
  lastChecked: 0,
});

let pending: Update | null = null;

export async function checkForUpdate(manual = false) {
  if (updater.phase === "checking" || updater.phase === "downloading" || updater.phase === "installing") return;
  setUpdater({ phase: "checking", error: "" });
  try {
    const u = await check();
    setUpdater("lastChecked", Date.now());
    if (!u) {
      log.info("update check: up to date");
      pending = null;
      setUpdater("phase", "idle");
      if (manual) toast("Ferret is up to date");
      return;
    }
    log.info(`update check: ${u.version} available`);
    pending = u;
    setUpdater({ phase: "available", version: u.version, notes: u.body ?? "", date: u.date?.match(/^\d{4}-\d{2}-\d{2}/)?.[0] ?? "" });
    if (manual) setUpdater("dismissed", "");
  } catch (e) {
    log.warn(`update check failed: ${describeError(e)}`);
    setUpdater({ phase: manual ? "error" : "idle", error: String(e) });
    if (manual) toast(`Couldn't check for updates: ${e}`, "error");
  }
}

/** Downloads, verifies and runs the installer, then restarts into the new version. */
export async function installUpdate() {
  setUpdater("notesOpen", false);
  if (!pending) return;
  setUpdater({ phase: "downloading", done: 0, total: 0, error: "" });
  log.info(`installing update ${pending.version}`);
  try {
    await pending.downloadAndInstall((ev) => {
      if (ev.event === "Started") setUpdater("total", ev.data.contentLength ?? 0);
      else if (ev.event === "Progress") setUpdater("done", (d) => d + ev.data.chunkLength);
      else if (ev.event === "Finished") setUpdater("phase", "installing");
    });
    // The Windows installer usually closes Ferret itself; this covers the case where it doesn't.
    await relaunch();
  } catch (e) {
    log.error(`update install failed: ${describeError(e)}`);
    setUpdater({ phase: "error", error: String(e) });
  }
}

// After a failed download the update is still on offer (the dot by the version stays).
export const dismissUpdate = () => setUpdater({
  dismissed: updater.version, notesOpen: false,
  phase: updater.phase === "error" ? (updater.version ? "available" : "idle") : updater.phase,
});

export const showWhatsNew = () => setUpdater("notesOpen", true);

/** A plain explanation of an update failure (the raw error stays available as details). */
export function updateErrorText(raw: string) {
  if (/signature|verify|minisign|pubkey/i.test(raw)) return "The download failed its signature check, so nothing was installed.";
  if (/timed? ?out|request|network|connect|dns|resolve|offline|tls|certificate/i.test(raw)) return "Couldn't download the update. Check your connection and try again.";
  if (/denied|access|permission|elevat/i.test(raw)) return "Windows didn't allow the installer to run.";
  return "Something went wrong while updating.";
}

export { parseNotes } from "./notes";

/** Starts the periodic check; also nudges about an outdated search service once. */
export function startUpdater() {
  getVersion().then((v) => setUpdater("current", v)).catch(() => {});
  if (import.meta.env.DEV) (window as any).__ferretUpdater = { updater, setUpdater };
  // Dev builds would offer the latest release over themselves; the end-to-end test build
  // (mode "e2e") must not reach the network at all.
  if (import.meta.env.DEV || import.meta.env.MODE === "e2e") return;
  setTimeout(() => {
    checkForUpdate();
    setInterval(checkForUpdate, CHECK_EVERY_MS);
  }, FIRST_CHECK_MS);
  setTimeout(checkIndexer, FIRST_CHECK_MS / 3);
}

/** After an update the installed FerretIndexer service may be older than this app. */
async function checkIndexer() {
  const outdated = await invoke<boolean>("search_service_outdated").catch(() => false);
  if (!outdated) return;
  log.info("search service is out of date");
  toast("The drive index service is out of date", "info", { label: "Update…", run: updateIndexer });
}

export async function updateIndexer() {
  try {
    await invoke("search_install_service");
    toast("Updating the drive index service…");
  } catch (e) {
    toast(String(e), "error");
  }
}
