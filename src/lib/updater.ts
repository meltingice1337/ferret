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
      pending = null;
      setUpdater("phase", "idle");
      if (manual) toast("Ferret is up to date");
      return;
    }
    pending = u;
    setUpdater({ phase: "available", version: u.version, notes: u.body ?? "", date: u.date?.match(/^\d{4}-\d{2}-\d{2}/)?.[0] ?? "" });
    if (manual) setUpdater("dismissed", "");
  } catch (e) {
    setUpdater({ phase: manual ? "error" : "idle", error: String(e) });
    if (manual) toast(`Couldn't check for updates: ${e}`, "error");
  }
}

/** Downloads, verifies and runs the installer, then restarts into the new version. */
export async function installUpdate() {
  setUpdater("notesOpen", false);
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

const SECTIONS: Record<string, string> = { feat: "New", fix: "Fixes", perf: "Improvements", refactor: "Improvements", style: "Improvements" };
const ORDER = ["New", "Improvements", "Fixes", "Other changes"];

/** "## Bug fixes" and the like map onto the same sections as commit prefixes. */
function headingSection(text: string) {
  if (/feat|new|added/i.test(text)) return "New";
  if (/fix|bug/i.test(text)) return "Fixes";
  if (/perf|improv|chang|polish/i.test(text)) return "Improvements";
  return text;
}

/**
 * Release notes are the commit subjects since the last release ("- feat: …", "- fix(x): …")
 * plus a "Full changelog" link, possibly with "## Heading" sections written by hand.
 * Groups them for reading; items keep their inline markdown (rendered by the dialog).
 */
export function parseNotes(md: string) {
  const groups = new Map<string, string[]>();
  let link = "";
  let heading = "";
  for (const raw of md.split(/\r?\n/)) {
    const h = raw.trim().match(/^#{1,6}\s+(.+)$/);
    if (h) { heading = headingSection(h[1].trim()); continue; }
    const line = raw.trim().replace(/^[-*]\s+/, "");
    if (!line || /^no changes listed$/i.test(line)) continue;
    const full = line.match(/^\*\*full changelog:?\*\*:?\s*(\S+)/i);
    if (full) { link = full[1]; continue; }
    const m = line.match(/^(\w+)(?:\([^)]*\))?!?:\s*(.+)$/);
    if (m?.[1].toLowerCase() === "release") continue;
    const title = (m && SECTIONS[m[1].toLowerCase()]) || heading || "Other changes";
    const text = m ? m[2] : line;
    // Capitalize the first letter, even behind markdown like "**".
    groups.set(title, [...(groups.get(title) ?? []), text.replace(/\p{L}/u, (c) => c.toUpperCase())]);
  }
  const order = [...ORDER.filter((t) => groups.has(t) && t !== "Other changes"),
    ...[...groups.keys()].filter((t) => !ORDER.includes(t)),
    ...(groups.has("Other changes") ? ["Other changes"] : [])];
  return { sections: order.map((title) => ({ title, items: groups.get(title)! })), link };
}

/** Starts the periodic check; also nudges about an outdated search service once. */
export function startUpdater() {
  getVersion().then((v) => setUpdater("current", v)).catch(() => {});
  if (import.meta.env.DEV) (window as any).__ferretUpdater = { updater, setUpdater };
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
