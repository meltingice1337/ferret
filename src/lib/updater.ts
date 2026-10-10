// Self-update from GitHub Releases (tauri-plugin-updater). Ferret checks shortly after
// startup and every few hours, then offers the update; nothing installs without a click.
// The installer is signed with the release key, and the plugin rejects anything else.
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { createStore } from "solid-js/store";
import { toast } from "./state";

const FIRST_CHECK_MS = 15_000;
const CHECK_EVERY_MS = 6 * 60 * 60 * 1000;

export type UpdatePhase = "idle" | "checking" | "available" | "downloading" | "installing" | "error";

export const [updater, setUpdater] = createStore({
  current: "",
  phase: "idle" as UpdatePhase,
  version: "",
  notes: "",
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
      pending = null;
      setUpdater("phase", "idle");
      if (manual) toast("Ferret is up to date");
      return;
    }
    pending = u;
    setUpdater({ phase: "available", version: u.version, notes: u.body ?? "" });
    if (manual) setUpdater("dismissed", "");
  } catch (e) {
    setUpdater({ phase: manual ? "error" : "idle", error: String(e) });
    if (manual) toast(`Couldn't check for updates: ${e}`, "error");
  }
}

/** Downloads, verifies and runs the installer, then restarts into the new version. */
export async function installUpdate() {
  if (!pending) return;
  setUpdater({ phase: "downloading", done: 0, total: 0, error: "" });
  try {
    await pending.downloadAndInstall((ev) => {
      if (ev.event === "Started") setUpdater("total", ev.data.contentLength ?? 0);
      else if (ev.event === "Progress") setUpdater("done", (d) => d + ev.data.chunkLength);
      else if (ev.event === "Finished") setUpdater("phase", "installing");
    });
    // The Windows installer usually closes Ferret itself; this covers the case where it doesn't.
    await relaunch();
  } catch (e) {
    setUpdater({ phase: "error", error: String(e) });
  }
}

export const dismissUpdate = () => setUpdater({ dismissed: updater.version, phase: updater.phase === "error" ? "idle" : updater.phase });

/** Starts the periodic check; also nudges about an outdated search service once. */
export function startUpdater() {
  getVersion().then((v) => setUpdater("current", v)).catch(() => {});
  // Dev builds would offer the latest release over themselves.
  if (import.meta.env.DEV) return;
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
