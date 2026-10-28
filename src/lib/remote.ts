// Server connections (SSH): opening and closing them, the connection form, the questions they
// ask (passwords, server keys), and files opened from them being uploaded back.

import { batch } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";
import { isRemote, remoteParts } from "./path";
import {
  allPanes, ask, connections, OFFLINE, navigate, newTab, activeTab, setConnections, setRemoteState, setUi, showOffline, toast, ui, type Tab,
} from "./state";
import type { Connection, RemotePrompt } from "./types";

/** A new connection's form, filled in from what's known so far. */
export const blankConnection = (): Connection => ({
  id: "", name: "", kind: "ssh", host: "", port: 22, user: "", sshAlias: null, auth: "auto", identityFile: null, startDir: null, search: false,
});

/** Opens a connection (connecting if need be) in the tab, or a new one. */
export async function openConnection(id: string, inNewTab = false, tab: Tab = activeTab()) {
  const c = connections().find((x) => x.id === id);
  try {
    const path = await api.remoteConnect(id);
    if (inNewTab) await newTab(path);
    else await navigate(tab, path);
  } catch (e) {
    if (e !== "Cancelled") toast(`Couldn't connect to ${c?.name ?? id}: ${e}`, "error");
  }
}

export async function disconnect(id: string) {
  await api.remoteDisconnect(id).catch(() => {});
}

export const editConnection = (connection: Connection | null) => setUi("connectionForm", { connection });

export async function removeConnection(c: Connection) {
  const ok = await ask({
    title: `Remove “${c.name}”?`,
    message: "Ferret forgets this connection (and a password it remembered for it). Nothing changes on the server, or in ~/.ssh.",
    confirmLabel: "Remove", danger: true,
  });
  if (ok === null) return;
  setConnections(await api.remoteDeleteConnection(c.id));
}

/** Answers the question shown (null: cancelled). */
export function answerPrompt(values: string[] | null, remember = false) {
  const p = ui.remotePrompt;
  if (!p) return;
  setUi("remotePrompt", null);
  api.remoteAnswer(p.id, values, remember).catch(() => {});
  queueMicrotask(showNext);
}

// ---------- what the backend tells ----------

/** Questions come one at a time; more wait their turn. */
const waiting: RemotePrompt[] = [];
const showNext = () => {
  if (!ui.remotePrompt && waiting.length) setUi("remotePrompt", waiting.shift()!);
};

listen<RemotePrompt>("remote-prompt", (ev) => {
  waiting.push(ev.payload);
  showNext();
});

listen<{ id: string; state: "connecting" | "connected" | "disconnected"; error: string | null }>("remote-status", (ev) => {
  const { id, state } = ev.payload;
  setRemoteState(id, state);
  // Closed (by the user, after a while unused, or the network): its tabs keep their folder,
  // with a Connect button.
  if (state === "disconnected") {
    batch(() => {
      for (const t of allPanes()) {
        if (remoteParts(t.p)?.id === id && t.error[0]() !== OFFLINE && !t.loading[0]()) showOffline(t, t.p);
      }
    });
  }
});

listen<Connection[]>("remote-connections", (ev) => setConnections(ev.payload));

listen<{ path: string; name: string; connection: string; error: string | null }>("remote-synced", (ev) => {
  const { name, connection, error } = ev.payload;
  if (error) toast(`Couldn't upload “${name}” to ${connection}: ${error}`, "error");
  else toast(`Saved “${name}” to ${connection}`);
});

/** What the window knows about the sessions (another of this window's actions may have opened one). */
export function refreshRemoteState() {
  api.remoteStatus().then((all) => batch(() => all.forEach((s) => setRemoteState(s.id, s.connected ? "connected" : "disconnected")))).catch(() => {});
}

/** The server's name for messages about a path on it. */
export const serverName = (p: string) => (isRemote(p) ? connections().find((c) => c.id === remoteParts(p)?.id)?.name ?? "the server" : "");

/** Toast for an action that doesn't work on servers yet. */
export const notOnServers = (what: string) => toast(`${what} isn't available for files on a server`);
