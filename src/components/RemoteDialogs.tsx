// The server connection form, and the questions a connection asks (password, a key's
// passphrase, whether to trust a server's key).

import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { createResource, createSignal, For, Show } from "solid-js";
import { createStore, unwrap } from "solid-js/store";
import { api } from "../lib/api";
import { AlertTriangle, FolderOpen, KeyRound, Loader2, Server } from "../lib/icons";
import { answerPrompt, blankConnection, openConnection } from "../lib/remote";
import { setConnections, setUi, ui } from "../lib/state";
import type { Connection, HostConfig } from "../lib/types";
import { Modal } from "./Dialogs";
import "../remote.css";

// ---------- the connection form ----------

export function ConnectionDialog() {
  return (
    <Show when={ui.connectionForm} keyed>
      {(form) => <ConnectionForm editing={form.connection} />}
    </Show>
  );
}

function ConnectionForm(props: { editing: Connection | null }) {
  const isNew = !props.editing;
  const [c, setC] = createStore<Connection>(structuredClone(props.editing ? unwrap(props.editing) : blankConnection()));
  const [hosts] = createResource(() => api.remoteConfigHosts().catch(() => [] as HostConfig[]));
  const [test, setTest] = createSignal<null | { busy: true } | { ok: string } | { error: string }>(null);
  const [error, setError] = createSignal("");
  const close = () => setUi("connectionForm", null);

  /** Fills the form from a ~/.ssh/config host (everything stays editable). */
  const pick = (alias: string) => {
    const h = hosts()?.find((x) => x.alias === alias);
    if (!h) return setC("sshAlias", null);
    setC({
      sshAlias: h.alias,
      name: !c.name || c.name === c.sshAlias ? h.alias : c.name,
      host: h.hostName ?? h.alias,
      port: h.port ?? 22,
      user: h.user ?? "",
      identityFile: h.identityFiles[0] ?? null,
    });
    setTest(null);
  };

  const browseKey = async () => {
    const picked = await openDialog({ title: "Choose a private key file", defaultPath: c.identityFile ?? "~/.ssh" }).catch(() => null);
    if (typeof picked === "string") setC("identityFile", picked);
  };

  const valid = () => !!c.host.trim() && c.port > 0 && c.port < 65536;
  const runTest = async () => {
    setTest({ busy: true });
    try {
      const r = await api.remoteTest(unwrap(c));
      setTest({ ok: `Connected. Home folder ${r.home}${r.arch ? ` · Linux (${r.arch}): folder sizes and search run on the server` : " · files only (SFTP): no folder sizes or search there"}` });
    } catch (e) {
      setTest(e === "Cancelled" ? null : { error: String(e) });
    }
  };
  const save = async () => {
    if (!valid()) return setError("Enter the server's address and a port between 1 and 65535");
    try {
      const all = await api.remoteSaveConnection({ ...unwrap(c), name: c.name.trim() || c.sshAlias || c.host.trim() });
      setConnections(all);
      close();
      // A new connection (the backend adds it last, with its id) opens right away.
      const added = all[all.length - 1];
      if (isNew && added) openConnection(added.id);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <Modal title={isNew ? "Add a server" : `Edit “${props.editing!.name}”`} onClose={close} width={560} class="conn-form">
      <form class="modal-body" onSubmit={(e) => { e.preventDefault(); save(); }}>
        <Show when={(hosts() ?? []).length}>
          <div class="field">
            <div class="field-label"><span>From ~/.ssh/config</span><small>Fills in the form; you can change everything</small></div>
            <div class="field-control">
              <select value={c.sshAlias ?? ""} onChange={(e) => pick(e.currentTarget.value)}>
                <option value="">None</option>
                <For each={hosts()}>{(h) => <option value={h.alias}>{h.alias}{h.hostName && h.hostName !== h.alias ? ` (${h.hostName})` : ""}</option>}</For>
              </select>
            </div>
          </div>
        </Show>
        <div class="field">
          <div class="field-label"><span>Name</span><small>As the sidebar shows it</small></div>
          <div class="field-control">
            <input class="text-input" value={c.name} placeholder={c.sshAlias || c.host || "Home server"} spellcheck={false}
              ref={(el) => queueMicrotask(() => el.focus())}
              onInput={(e) => setC("name", e.currentTarget.value)} />
          </div>
        </div>
        <div class="field">
          <div class="field-label"><span>Server</span><small>Its address or name, and SSH port</small></div>
          <div class="field-control conn-host">
            <input class="text-input" value={c.host} placeholder="192.168.0.10 or server.example.com" spellcheck={false}
              onInput={(e) => { setC("host", e.currentTarget.value); setTest(null); }} />
            <input class="text-input conn-port" type="number" min="1" max="65535" value={c.port} title="Port"
              onInput={(e) => setC("port", Number(e.currentTarget.value) || 0)} />
          </div>
        </div>
        <div class="field">
          <div class="field-label"><span>User</span><small>Empty: your Windows user name</small></div>
          <div class="field-control">
            <input class="text-input" value={c.user} placeholder="root" spellcheck={false} onInput={(e) => setC("user", e.currentTarget.value)} />
          </div>
        </div>
        <div class="field">
          <div class="field-label"><span>Sign in with</span>
            <small>{c.auth === "auto" ? "The SSH agent and your keys in ~/.ssh first, then a password" : c.auth === "key" ? "Only this key file" : "A password only"}</small>
          </div>
          <div class="field-control">
            <select value={c.auth} onChange={(e) => setC("auth", e.currentTarget.value as Connection["auth"])}>
              <option value="auto">Keys, then password</option>
              <option value="key">A key file</option>
              <option value="password">A password</option>
            </select>
          </div>
        </div>
        <Show when={c.auth !== "password"}>
          <div class="field">
            <div class="field-label"><span>Key file</span><small>{c.auth === "key" ? "Its passphrase is asked for when needed" : "Optional: tried before the others"}</small></div>
            <div class="field-control conn-key">
              <input class="text-input" value={c.identityFile ?? ""} placeholder="~/.ssh/id_ed25519" spellcheck={false}
                onInput={(e) => setC("identityFile", e.currentTarget.value || null)} />
              <button type="button" class="btn" onClick={browseKey}><KeyRound size={14} />Browse…</button>
            </div>
          </div>
        </Show>
        <div class="field">
          <div class="field-label"><span>Opens in</span><small>Empty: your home folder there</small></div>
          <div class="field-control">
            <input class="text-input" value={c.startDir ?? ""} placeholder="/home/you" spellcheck={false}
              onInput={(e) => setC("startDir", e.currentTarget.value.trim() || null)} />
          </div>
        </div>
        <label class="field conn-check">
          <div class="field-label"><span>Search its files with Ctrl+K</span><small>While it's connected; nothing is kept once it disconnects</small></div>
          <input type="checkbox" checked={c.search} onChange={(e) => setC("search", e.currentTarget.checked)} />
        </label>

        <Show when={test()}>
          {(t) => (
            <div class="conn-test" classList={{ ok: "ok" in t(), bad: "error" in t() }}>
              <Show when={"busy" in t()}><Loader2 size={14} class="spin" />Connecting…</Show>
              <Show when={"ok" in t()}><Server size={14} />{(t() as { ok: string }).ok}</Show>
              <Show when={"error" in t()}><AlertTriangle size={14} />{(t() as { error: string }).error}</Show>
            </div>
          )}
        </Show>
        <Show when={error()}><div class="conn-test bad"><AlertTriangle size={14} />{error()}</div></Show>

        <div class="modal-actions">
          <button type="button" class="btn conn-test-btn" disabled={!valid() || (test() !== null && "busy" in test()!)} onClick={runTest}>
            <FolderOpen size={14} />Test connection
          </button>
          <button type="button" class="btn" onClick={close}>Cancel</button>
          <button type="submit" class="btn accent" disabled={!valid()}>{isNew ? "Add and connect" : "Save"}</button>
        </div>
      </form>
    </Modal>
  );
}

// ---------- the questions ----------

export function RemotePromptDialog() {
  return (
    <Show when={ui.remotePrompt} keyed>
      {(p) => {
        const [values, setValues] = createSignal<string[]>(p.fields.map(() => ""));
        const [remember, setRemember] = createSignal(false);
        const submit = (e: Event) => {
          e.preventDefault();
          answerPrompt(p.kind === "secret" ? values() : [], remember());
        };
        return (
          <Modal title={p.title} onClose={() => answerPrompt(null)} width={460} class="remote-prompt">
            <form class="modal-body ask" onSubmit={submit}>
              <Show when={p.message}><p class="ask-msg"><span>{p.message}</span></p></Show>
              <Show when={p.fingerprint}><code class="fingerprint">{p.fingerprint}</code></Show>
              <For each={p.fields}>
                {(f, i) => (
                  <label class="prompt-field">
                    <Show when={f.label && p.fields.length > 1}><span>{f.label}</span></Show>
                    <input class="text-input" type={f.secret ? "password" : "text"} spellcheck={false} autocomplete="off"
                      placeholder={p.fields.length === 1 ? f.label : ""}
                      ref={(el) => i() === 0 && queueMicrotask(() => el.focus())}
                      onInput={(e) => setValues((v) => v.map((x, j) => (j === i() ? e.currentTarget.value : x)))} />
                  </label>
                )}
              </For>
              <Show when={p.canRemember}>
                <label class="prompt-remember">
                  <input type="checkbox" checked={remember()} onChange={(e) => setRemember(e.currentTarget.checked)} />
                  Remember (in Windows Credential Manager)
                </label>
              </Show>
              <div class="modal-actions">
                <button type="button" class="btn" onClick={() => answerPrompt(null)}
                  ref={(el) => p.kind !== "secret" && queueMicrotask(() => el.focus())}>Cancel</button>
                <button type="submit" class="btn" classList={{ danger: p.danger, accent: !p.danger }}>
                  {p.confirmLabel ?? (p.kind === "secret" ? "Connect" : "OK")}
                </button>
              </div>
            </form>
          </Modal>
        );
      }}
    </Show>
  );
}
