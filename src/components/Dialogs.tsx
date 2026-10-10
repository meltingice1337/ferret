import { AlertTriangle, ExternalLink, FolderOpen, ShieldCheck, X } from "lucide-solid";
import { createMemo, createResource, createSignal, ErrorBoundary, For, onMount, Show, type JSX } from "solid-js";
import { api } from "../lib/api";
import { formatDate, formatSize, typeLabel } from "../lib/format";
import { batchRename } from "../lib/ops";
import { basename, parent } from "../lib/path";
import { activeTab, ASK_ALT, navigate, reload, setUi, toast, ui } from "../lib/state";
import type { Entry, Policy } from "../lib/types";
import { Thumb } from "./FileView";

function Modal(props: { title: string; onClose: () => void; children: JSX.Element; width?: number; class?: string }) {
  return (
    <div class="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class={`modal ${props.class ?? ""}`} role="dialog" aria-label={props.title} tabIndex={-1}
        style={props.width ? { width: `min(${props.width}px, calc(100vw - 40px))` } : undefined}
        ref={(el) => queueMicrotask(() => { if (!el.contains(document.activeElement)) el.focus(); })}
        onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); props.onClose(); } e.stopPropagation(); }}>
        <header class="modal-head">
          <h2>{props.title}</h2>
          <button class="icon-btn" onClick={props.onClose} title="Close (Esc)"><X size={16} /></button>
        </header>
        {props.children}
      </div>
    </div>
  );
}

// ---------- confirm / prompt ----------

export function AskDialog() {
  return (
    <Show when={ui.dialog} keyed>
      {(d) => {
        const [value, setValue] = createSignal(d.input ?? "");
        const done = (v: string | null) => { setUi("dialog", null); d.resolve(v); };
        return (
          <Modal title={d.title} onClose={() => done(null)} width={440}>
            <form class="modal-body ask" onSubmit={(e) => { e.preventDefault(); done(d.input !== undefined ? value() : ""); }}>
              <Show when={d.message}>
                <p class="ask-msg" classList={{ shield: d.shield }}>
                  <Show when={d.shield}><ShieldCheck size={28} class="ask-shield" /></Show>
                  <span>{d.message}</span>
                </p>
              </Show>
              <Show when={d.input !== undefined}>
                <input class="text-input" type={d.password ? "password" : "text"} value={value()} spellcheck={false}
                  ref={(el) => queueMicrotask(() => { el.focus(); el.select(); })}
                  onInput={(e) => setValue(e.currentTarget.value)} />
              </Show>
              <div class="modal-actions">
                <button type="button" class="btn" onClick={() => done(null)}>Cancel</button>
                <Show when={d.alt}>
                  <button type="button" class="btn" onClick={() => done(ASK_ALT)}>
                    <Show when={d.shield}><ShieldCheck size={14} /></Show>{d.alt}
                  </button>
                </Show>
                <button type="submit" class="btn" classList={{ danger: d.danger, accent: !d.danger }}
                  ref={(el) => d.input === undefined && queueMicrotask(() => el.focus())}>
                  <Show when={d.shield}><ShieldCheck size={14} /></Show>{d.confirmLabel ?? "OK"}
                </button>
              </div>
            </form>
          </Modal>
        );
      }}
    </Show>
  );
}

// ---------- copy/move conflicts ----------

export function ConflictDialog() {
  return (
    <Show when={ui.conflict} keyed>
      {(c) => {
        const done = (p: Policy | null) => { setUi("conflict", null); c.resolve(p); };
        const n = c.conflicts.length;
        return (
          <Modal title={n === 1 ? `“${c.conflicts[0].name}” already exists` : `${n} items already exist`} onClose={() => done(null)} width={560}>
            <div class="modal-body">
              <p class="ask-msg">The destination <b>{basename(c.dest)}</b> already has {n === 1 ? "an item" : "items"} with the same name.</p>
              <div class="conflicts">
                <For each={c.conflicts.slice(0, 6)}>
                  {(x) => (
                    <div class="conflict">
                      <div class="conflict-name">{x.name}</div>
                      <div class="conflict-cols">
                        <div><small>Incoming</small>{x.isDir ? "Folder" : formatSize(x.sourceSize)} · {formatDate(x.sourceModified)}</div>
                        <div><small>Existing</small>{x.isDir ? "Folder" : formatSize(x.targetSize)} · {formatDate(x.targetModified)}
                          <Show when={!x.isDir && (x.sourceModified ?? 0) > (x.targetModified ?? 0)}><span class="newer">older</span></Show>
                        </div>
                      </div>
                    </div>
                  )}
                </For>
                <Show when={n > 6}><div class="pv-note small">…and {n - 6} more</div></Show>
              </div>
              <div class="conflict-choices">
                <button class="choice" onClick={() => done("replace")} ref={(el) => queueMicrotask(() => el.focus())}>
                  <b>Replace</b><span>Overwrite the existing {n === 1 ? "item" : "items"}; folders are merged</span>
                </button>
                <button class="choice" onClick={() => done("keepBoth")}><b>Keep both</b><span>Incoming items get a numbered name, e.g. “name (2)”</span></button>
                <button class="choice" onClick={() => done("skip")}><b>Skip</b><span>Leave the existing {n === 1 ? "item" : "items"} untouched</span></button>
              </div>
              <div class="modal-actions"><button class="btn" onClick={() => done(null)}>Cancel {c.op}</button></div>
            </div>
          </Modal>
        );
      }}
    </Show>
  );
}

// ---------- properties ----------

export function PropertiesDialog() {
  return (
    <Show when={ui.properties} keyed>
      {(paths) => (
        <ErrorBoundary fallback={(err) => { toast(`Properties: ${String(err?.message ?? err)}`, "error"); setUi("properties", null); return null; }}>
          <Properties paths={paths} />
        </ErrorBoundary>
      )}
    </Show>
  );
}

function Properties(props: { paths: string[] }) {
  const close = () => setUi("properties", null);
  const single = props.paths.length === 1;
  const [info, { refetch }] = createResource(() => (single ? props.paths[0] : null), (p) => api.itemInfo(p));
  const [stats] = createResource(() => props.paths, (p) => api.folderStats(p));
  const entry = createMemo<Entry | null>(() => {
    const i = info();
    if (!i) return null;
    return { name: i.name, path: i.path, isDir: i.isDir, isSymlink: i.isSymlink, hidden: i.hidden, size: i.size, modified: i.modified, created: i.created, ext: i.isDir ? "" : (i.name.split(".").pop() ?? "").toLowerCase(), inArchive: false };
  });
  const toggleAttr = async (attr: "readonly" | "hidden") => {
    const i = info();
    if (!i) return;
    try {
      await api.setAttributes(i.path, attr === "readonly" ? !i.readonly : i.readonly, attr === "hidden" ? !i.hidden : i.hidden);
      refetch();
      reload(activeTab());
    } catch (e) {
      toast(String(e), "error");
    }
  };
  const Row = (p: { label: string; children: JSX.Element }) => <><dt>{p.label}</dt><dd>{p.children}</dd></>;

  return (
    <Modal title={single ? `${basename(props.paths[0])} properties` : `${props.paths.length} items`} onClose={close} width={520}>
      <div class="modal-body props">
        <div class="props-hero">
          <Show when={entry()} fallback={<div class="props-icon multi">{props.paths.length}</div>}>
            {(e) => <div class="props-icon"><Thumb entry={e()} size={64} /></div>}
          </Show>
          <div>
            <div class="props-name">{single ? basename(props.paths[0]) : `${props.paths.length} items`}</div>
            <div class="muted">{entry() ? typeLabel(entry()!) : `in ${basename(parent(props.paths[0]) ?? "")}`}</div>
          </div>
        </div>
        <dl class="props-grid">
          <Row label="Location">
            <button class="link" onClick={() => { close(); navigate(activeTab(), parent(props.paths[0])!, { select: props.paths[0] }); }}>{parent(props.paths[0])}</button>
          </Row>
          <Row label="Size">
            <Show when={stats()} fallback={<span class="muted">Calculating…</span>}>
              {(s) => <>{formatSize(s().bytes)} <span class="muted">({s().bytes.toLocaleString()} bytes){s().partial ? " · some items unreadable" : ""}</span></>}
            </Show>
          </Row>
          <Show when={(stats()?.dirs ?? 0) + (stats()?.files ?? 0) > 1 || info()?.isDir}>
            <Row label="Contains">{stats() ? `${stats()!.files.toLocaleString()} files, ${stats()!.dirs.toLocaleString()} folders` : "…"}</Row>
          </Show>
          <Show when={info()}>
            {(i) => (
              <>
                <Show when={i().linkTarget}><Row label="Target">{i().linkTarget}</Row></Show>
                <Row label="Created">{formatDate(i().created)}</Row>
                <Row label="Modified">{formatDate(i().modified)}</Row>
                <Row label="Accessed">{formatDate(i().accessed)}</Row>
                <Row label="Attributes">
                  <label class="check"><input type="checkbox" checked={i().readonly} onChange={() => toggleAttr("readonly")} /> Read-only</label>
                  <label class="check"><input type="checkbox" checked={i().hidden} onChange={() => toggleAttr("hidden")} /> Hidden</label>
                  <Show when={i().system}><span class="muted">System</span></Show>
                </Row>
              </>
            )}
          </Show>
        </dl>
        <div class="modal-actions">
          <button class="btn" onClick={() => api.revealInExplorer(props.paths[0])}><FolderOpen size={14} /> Show in Explorer</button>
          <button class="btn" onClick={() => api.showProperties(props.paths).catch((e) => toast(String(e), "error"))}><ExternalLink size={14} /> Windows properties…</button>
          <button class="btn accent" onClick={close}>Close</button>
        </div>
      </div>
    </Modal>
  );
}

// ---------- batch rename ----------

export function BatchRenameDialog() {
  return (
    <Show when={ui.batchRename} keyed>
      {(paths) => <BatchRename paths={paths} />}
    </Show>
  );
}

function BatchRename(props: { paths: string[] }) {
  const close = () => setUi("batchRename", null);
  const [find, setFind] = createSignal("");
  const [replace, setReplace] = createSignal("");
  const [regex, setRegex] = createSignal(false);
  const [matchCase, setMatchCase] = createSignal(false);
  const [template, setTemplate] = createSignal("");
  const [start, setStart] = createSignal(1);
  const [casing, setCasing] = createSignal<"keep" | "lower" | "upper" | "title">("keep");
  let findInput!: HTMLInputElement;
  onMount(() => findInput.focus());

  const preview = createMemo(() => {
    let re: RegExp | null = null;
    let error = "";
    if (find()) {
      try {
        re = regex() ? new RegExp(find(), matchCase() ? "g" : "gi") : new RegExp(find().replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), matchCase() ? "g" : "gi");
      } catch (e) {
        error = String(e);
      }
    }
    const width = String(start() + props.paths.length - 1).length;
    const rows = props.paths.map((p, i) => {
      const name = basename(p);
      const dot = name.lastIndexOf(".");
      const stem = dot > 0 ? name.slice(0, dot) : name;
      const ext = dot > 0 ? name.slice(dot) : "";
      let next = template()
        ? template()
            .replace(/\{name\}/g, stem)
            .replace(/\{ext\}/g, ext)
            .replace(/\{n(?::(\d+))?\}/g, (_, pad) => String(start() + i).padStart(pad ? +pad : width, "0"))
        : name;
      if (re) next = next.replace(re, replace());
      if (casing() === "lower") next = next.toLowerCase();
      else if (casing() === "upper") next = next.toUpperCase();
      else if (casing() === "title") next = next.replace(/\b\p{L}/gu, (c) => c.toUpperCase());
      return { from: p, name, to: next };
    });
    const names = rows.map((r) => r.to.toLowerCase());
    const dupes = new Set(names.filter((n, i) => names.indexOf(n) !== i));
    return { rows, error, dupes, changed: rows.filter((r) => r.to !== r.name).length };
  });

  const apply = async () => {
    const p = preview();
    if (p.error || p.dupes.size) return;
    close();
    await batchRename(p.rows.filter((r) => r.to !== r.name).map((r) => ({ from: r.from, to: r.to })));
  };

  return (
    <Modal title={`Rename ${props.paths.length} items`} onClose={close} width={720} class="batch">
      <div class="modal-body" onKeyDown={(e) => {
        if (e.key === "Enter" && !(e.target as HTMLElement).closest("select, button")) { e.preventDefault(); apply(); }
      }}>
        <div class="br-grid">
          <label>Find<input ref={findInput} class="text-input" value={find()} onInput={(e) => setFind(e.currentTarget.value)} placeholder="Text or pattern" /></label>
          <label>Replace with<input class="text-input" value={replace()} onInput={(e) => setReplace(e.currentTarget.value)} placeholder={regex() ? "Use $1 for groups" : ""} /></label>
          <label>Template<input class="text-input" value={template()} onInput={(e) => setTemplate(e.currentTarget.value)} placeholder="e.g. Trip_{n:3}{ext}" /></label>
          <label>Start at<input class="text-input" type="number" value={start()} onInput={(e) => setStart(+e.currentTarget.value || 0)} /></label>
        </div>
        <div class="br-opts">
          <label class="check"><input type="checkbox" checked={regex()} onChange={(e) => setRegex(e.currentTarget.checked)} /> Regular expression</label>
          <label class="check"><input type="checkbox" checked={matchCase()} onChange={(e) => setMatchCase(e.currentTarget.checked)} /> Match case</label>
          <select value={casing()} onChange={(e) => setCasing(e.currentTarget.value as any)}>
            <option value="keep">Keep case</option><option value="lower">lowercase</option>
            <option value="upper">UPPERCASE</option><option value="title">Title Case</option>
          </select>
          <span class="muted">Template tokens: {"{name} {ext} {n} {n:3}"}</span>
        </div>
        <Show when={preview().error}><div class="br-error"><AlertTriangle size={14} /> {preview().error}</div></Show>
        <div class="br-list">
          <For each={preview().rows}>
            {(r) => (
              <div class="br-row" classList={{ same: r.to === r.name, dupe: preview().dupes.has(r.to.toLowerCase()) }}>
                <span class="br-from">{r.name}</span><span class="br-arrow">→</span><span class="br-to">{r.to}</span>
              </div>
            )}
          </For>
        </div>
        <div class="modal-actions">
          <span class="muted" style={{ "margin-right": "auto" }}>
            {preview().dupes.size ? "Some new names collide" : `${preview().changed} of ${props.paths.length} will change`}
          </span>
          <button class="btn" onClick={close}>Cancel</button>
          <button class="btn accent" disabled={!!preview().error || preview().dupes.size > 0 || !preview().changed} onClick={apply}>Rename</button>
        </div>
      </div>
    </Modal>
  );
}
