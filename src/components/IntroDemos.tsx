// The intro tour's small demos: a few seconds of Ferret, drawn with the app's own styles so they
// follow the accent and the theme, and never go stale the way screenshots do. They only show;
// nothing here touches a file. With animations off each one holds its most telling frame.
import { createMemo, createSignal, For, Match, onCleanup, Show, Switch, type Component, type JSX } from "solid-js";
import {
  CircleHelp, Command, Copy, FileArchive, Folder, FolderOpen, FolderSearch, HardDrive, Hash, Inbox, LoaderCircle, Monitor, Network, PackageOpen, Plus, RotateCw, Search, Server, Star, Terminal, X,
} from "../lib/icons";
import { motionOn } from "../lib/motion";
import type { IntroDemo } from "../lib/intro";
import FileIcon from "./FileIcon";
import { typeIcon } from "./typeIcons";

/** Steps through `count` frames while the demo is on screen, each for `ms(frame)`. With
 *  animations off it stays on `rest`. `jump` goes to a frame (and carries on from there). */
function useFrames(count: number, ms: (frame: number) => number, rest = count - 1) {
  const moving = motionOn();
  const [frame, setFrame] = createSignal(moving ? 0 : rest);
  let timer: number | undefined;
  const after = (i: number) => {
    clearTimeout(timer);
    timer = window.setTimeout(() => {
      const next = (i + 1) % count;
      setFrame(next);
      after(next);
    }, ms(i));
  };
  if (moving) after(0);
  onCleanup(() => clearTimeout(timer));
  const jump = (i: number) => { setFrame(i); if (moving) after(i); };
  return [frame, jump] as const;
}

const Icon = (p: { name: string; dir?: boolean; size?: number }) => (
  <FileIcon entry={{ isDir: !!p.dir, isSymlink: false, ext: p.dir ? "" : p.name.split(".").pop()!.toLowerCase() }} size={p.size ?? 14} />
);

function Row(p: { name: string; dir?: boolean; size?: string; class?: string; on?: boolean; soft?: boolean }) {
  return (
    <div class={`ix-row ${p.class ?? ""}`} classList={{ on: p.on, soft: p.soft }}>
      <Icon name={p.name} dir={p.dir} />
      <span class="ix-name">{p.name}</span>
      <Show when={p.size}><span class="ix-size">{p.size}</span></Show>
    </div>
  );
}

const Win = (p: { class?: string; children: JSX.Element }) => <div class={`ix-win ${p.class ?? ""}`}>{p.children}</div>;

function Crumbs(p: { parts: string[]; marked?: boolean }) {
  return (
    <div class="ix-crumbs">
      <For each={p.parts}>
        {(c, i) => (
          <>
            <Show when={i() > 0}><span class="ix-sep">›</span></Show>
            <span class="ix-crumb" classList={{ last: i() === p.parts.length - 1, marked: i() === 0 && p.marked }}>{c}</span>
          </>
        )}
      </For>
    </div>
  );
}

/** The mouse in a demo. `x` is a share of the window's width, `y` is pixels from its top
 *  (or any CSS length, for something that doesn't sit at a fixed height). */
function Pointer(p: { x: number; y: number | string; ghost?: string; count?: number }) {
  return (
    <div class="ix-pointer" style={{ left: `${p.x}%`, top: typeof p.y === "number" ? `${p.y}px` : p.y }}>
      <svg width="16" height="20" viewBox="0 0 16 20" aria-hidden="true">
        <path d="M1 1v15l4-3.6 2.6 6 2.8-1.2-2.6-5.9H13z" fill="#fff" stroke="#111" stroke-width="1.2" stroke-linejoin="round" />
      </svg>
      <Show when={p.ghost}>
        <div class="ix-ghost"><b>{p.count}</b>{p.ghost}</div>
      </Show>
    </div>
  );
}

// ---------- search ----------

const daysAgo = (n: number) => new Date(Date.now() - n * 864e5).toISOString().slice(0, 10);

const HITS = [
  { name: "invoice-november.pdf", drive: "E", path: "Archive\\Finance", size: "240 KB", days: 2 },
  { name: "invoice-october.pdf", drive: "D", path: "Finance\\2026", size: "212 KB", days: 5 },
  { name: "invoices.xlsx", drive: "C", path: "Users\\you\\Documents", size: "64 KB", days: 10 },
  { name: "invoice-april.pdf", drive: "D", path: "Finance\\2026", size: "198 KB", days: 200 },
  { name: "invoice-template.docx", drive: "C", path: "Users\\you\\Templates", size: "31 KB", days: 330 },
];
const LINES = [
  { name: "budget-2026.csv", path: "Finance", size: "18 KB", days: 3, no: 41, before: "Studio rent,", match: "Total due", after: ",2 400", more: 0 },
  { name: "march-notes.md", path: "Notes", size: "4 KB", days: 40, no: 12, before: "- ", match: "Total due", after: " goes out on the 30th", more: 2 },
  { name: "reminder.txt", path: "Notes", size: "1 KB", days: 90, no: 7, before: "", match: "TOTAL DUE", after: ": 1,280.00", more: 0 },
];
const CHIPS = [["", "All"], ["folder", "Folders"], ["image", "Images"], ["video", "Videos"], ["audio", "Audio"], ["doc", "Documents"], ["code", "Code"], ["archive", "Archives"]];
const CHAPTERS = ["By name", "With filters", "Inside files", "As a tab"];

interface Scene { chapter: number; query: string; ms: number; view?: "keys" | "tab"; allRow?: boolean }
const typing = (chapter: number, from: string, text: string): Scene[] =>
  [...text].map((_, i) => ({ chapter, query: from + text.slice(0, i + 1), ms: 70 }));
const SCENES: Scene[] = [
  { chapter: 0, query: "", ms: 1200, view: "keys" },
  { chapter: 0, query: "", ms: 900 },
  ...typing(0, "", "invoice"),
  { chapter: 0, query: "invoice", ms: 2200 },
  ...typing(1, "invoice", " ex"),
  { chapter: 1, query: "invoice ex", ms: 1500 },
  { chapter: 1, query: "invoice ext:", ms: 500 },
  ...typing(1, "invoice ext:", "pdf"),
  { chapter: 1, query: "invoice ext:pdf", ms: 1500 },
  ...typing(1, "invoice ext:pdf", " modified:<7d"),
  { chapter: 1, query: "invoice ext:pdf modified:<7d", ms: 2400 },
  ...typing(2, "", 'content:"total due"'),
  { chapter: 2, query: 'content:"total due"', ms: 3400 },
  { chapter: 3, query: "invoice", ms: 2200, allRow: true },
  { chapter: 3, query: "invoice", ms: 3600, view: "tab" },
];
const firstOf = (chapter: number) => SCENES.findIndex((s) => s.chapter === chapter);
const lastOf = (chapter: number) => SCENES.map((s) => s.chapter).lastIndexOf(chapter);

/** What's typed, with its filters picked out. */
const Typed = (p: { query: string }) => (
  <For each={p.query.split(/((?:ext|modified):\S*|content:"[^"]*"?)/).filter(Boolean)}>
    {(part) => (/^(ext|modified|content):/.test(part) ? <span class="ix-token">{part}</span> : <>{part}</>)}
  </For>
);

function FakePalette(p: { scene: Scene }) {
  const query = () => p.scene.query;
  const inFiles = () => query().startsWith("content:");
  const word = () => (inFiles() ? "" : query().split(" ")[0]);
  const hits = createMemo(() => {
    if (inFiles() || word().length < 2) return [];
    return HITS.filter((h) => h.name.startsWith(word())
      && (!/ext:pdf/.test(query()) || h.name.endsWith(".pdf"))
      && (!/modified:<7d/.test(query()) || h.days < 7));
  });
  const lines = () => (query() === 'content:"total due"' ? LINES : []);
  return (
    <Win class="ix-palette ix-pop">
      <div class="palette-input">
        <Search size={18} />
        <span class="ix-typed">
          <Show when={query()} fallback={<span class="ix-placeholder">Search files and folders everywhere…</span>}><span><Typed query={query()} /></span></Show>
          <span class="ix-caret" />
        </span>
        <Star size={15} />
        <CircleHelp size={15} />
        <span class="palette-scope" classList={{ on: inFiles() }}>
          <Show when={inFiles()} fallback={<><HardDrive size={13} /> Everywhere</>}><FolderSearch size={13} /> This folder</Show>
        </span>
      </div>
      <div class="palette-types">
        <For each={CHIPS}>
          {([id, label], i) => (
            <button tabIndex={-1} classList={{ on: i() === 0 }}>
              <Show when={id}>{(() => { const I = typeIcon(id); return <I size={13} />; })()}</Show>
              {label}
            </button>
          )}
        </For>
      </div>
      <div class="palette-list">
        <Show when={/ ex$/.test(query())}>
          <div class="palette-suggest active">
            <Hash size={15} />
            <span class="palette-suggest-main"><code>ext:</code> An extension, like pdf</span>
            <span class="palette-suggest-exts" />
            <kbd>Tab</kbd>
          </div>
        </Show>
        <Show when={p.scene.allRow}>
          <div class="palette-suggest active">
            <FolderSearch size={15} />
            <span class="palette-suggest-main">Open all {hits().length} matches in a tab</span>
            <span class="palette-suggest-exts" />
            <kbd>Ctrl+Shift+Enter</kbd>
          </div>
        </Show>
        <Show when={!query()}>
          <div class="palette-empty"><span>Type to search your drives, or pick a type above. <code>?</code> shows every filter · <code>&gt;</code> for commands</span></div>
        </Show>
        <For each={hits()}>
          {(h, i) => (
            <div class="palette-item file" classList={{ active: i() === 0 && !p.scene.allRow && !/ ex$/.test(query()) }}>
              <Icon name={h.name} size={16} />
              <span class="palette-name"><mark>{h.name.slice(0, word().length)}</mark>{h.name.slice(word().length)}</span>
              <span class="palette-path"><span class="drive-letter">{h.drive}</span><span class="palette-path-text">{h.path}</span></span>
              <span class="palette-meta palette-size">{h.size}</span>
              <span class="palette-meta palette-date">{daysAgo(h.days)}</span>
              <span class="palette-reveal"><FolderOpen size={14} /></span>
            </div>
          )}
        </For>
        <For each={lines()}>
          {(h, i) => (
            <div class="palette-item file has-line" classList={{ active: i() === 0 }}>
              <Icon name={h.name} size={16} />
              <span class="palette-name">{h.name}</span>
              <span class="palette-path"><span class="palette-path-text">{h.path}</span></span>
              <span class="palette-meta palette-size">{h.size}</span>
              <span class="palette-meta palette-date">{daysAgo(h.days)}</span>
              <span class="palette-reveal"><FolderOpen size={14} /></span>
              <span class="palette-line">
                <span class="palette-line-no">{h.no}</span>
                <span class="palette-line-text">{h.before}<mark>{h.match}</mark>{h.after}</span>
                <Show when={h.more}><span class="palette-line-more">+{h.more}</span></Show>
              </span>
            </div>
          )}
        </For>
      </div>
      <div class="palette-foot">
        <span class="palette-foot-main">↑↓ choose · Enter open · <b>Alt+Enter show in folder</b> · ? filters</span>
      </div>
    </Win>
  );
}

function SearchTab() {
  return (
    <Win class="ix-palette ix-pop">
      <div class="ix-tabs">
        <div class="ix-tab on"><FolderSearch size={13} /><span>“invoice”</span></div>
        <Plus size={13} class="ix-dim" />
      </div>
      <div class="ix-list ix-grow">
        <For each={HITS}>
          {(h, i) => (
            <div class="ix-row ix-found" classList={{ on: i() === 0 }}>
              <Icon name={h.name} />
              <span class="ix-name">{h.name}</span>
              <span class="ix-where">{h.drive}:\{h.path}</span>
              <span class="ix-size">{h.size}</span>
            </div>
          )}
        </For>
      </div>
      <div class="palette-foot">
        <span class="palette-foot-main">Lists the matches like a folder and stays current · <b>Ctrl+S</b> saves the search to the sidebar</span>
      </div>
    </Win>
  );
}

function SearchDemo() {
  const [frame, jump] = useFrames(SCENES.length, (i) => SCENES[i].ms, lastOf(0));
  const scene = () => SCENES[frame()];
  return (
    <div class="ix-search">
      {/* Mouse only: the arrow keys are for moving through the tour. */}
      <div class="seg ix-chapters">
        <For each={CHAPTERS}>
          {(c, i) => <button tabIndex={-1} classList={{ on: scene().chapter === i() }} onClick={() => jump(motionOn() ? firstOf(i()) : lastOf(i()))}>{c}</button>}
        </For>
      </div>
      <Switch fallback={<FakePalette scene={scene()} />}>
        <Match when={scene().view === "keys"}>
          <div class="ix-keycaps"><kbd>Ctrl</kbd><span>+</span><kbd>K</kbd></div>
        </Match>
        <Match when={scene().view === "tab"}><SearchTab /></Match>
      </Switch>
    </div>
  );
}

// ---------- preview ----------

const CODE = (
  <pre class="ix-code">
    <span class="kw">fn</span> main() {"{\n"}
    {"    "}<span class="kw">let</span> name = <span class="st">"ferret"</span>;{"\n"}
    {"    "}println!(<span class="st">"hello, {"{name}"}"</span>);{"\n"}
    {"}"}
  </pre>
);
const PICTURE = <div class="ix-pic"><img src="/ferret-256.png" alt="" width={120} height={120} /><span>256 × 256</span></div>;
const NOTES = (
  <div class="ix-md">
    <h4>Trip notes</h4>
    <p>Things to sort before Friday:</p>
    <ul><li>Book the train</li><li>Scan the passports</li></ul>
  </div>
);
/** Word documents show as their pages, on white whatever the theme, as the real preview does. */
const WORD = (
  <div class="ix-page">
    <h5>Quarterly report</h5>
    <p>Sales grew in all three regions, led by the north. The new warehouse opened on time.</p>
    <table>
      <thead><tr><th>Region</th><th>Q2</th><th>Q3</th></tr></thead>
      <tbody>
        <tr><td>North</td><td>1,240</td><td>1,510</td></tr>
        <tr><td>Central</td><td>980</td><td>1,040</td></tr>
        <tr><td>South</td><td>760</td><td>815</td></tr>
      </tbody>
    </table>
    <p>Next quarter the focus moves to delivery times and to the two stores opening in spring.</p>
  </div>
);
/** PDFs show as numbered pages, one under the other, with the real preview's own styles. */
const PDF = (
  <div class="pv-pdfjs ix-pdf">
    <div class="pdf-page">
      <h5>Field guide</h5>
      <p>Ferrets sleep for up to eighteen hours a day, and spend the rest of it looking for things to hide.</p>
      <p>Keep their water fresh, and check behind the sofa before moving it.</p>
      <span class="pdf-num">1</span>
    </div>
    <div class="pdf-page">
      <h6>Chapter 2</h6>
      <p>A ferret will fit through any gap that its head fits through.</p>
      <span class="pdf-num">2</span>
    </div>
  </div>
);
const SHOWN = [
  { name: "ferret.png", size: "42 KB", view: PICTURE },
  { name: "main.rs", size: "2 KB", view: CODE },
  { name: "manual.pdf", size: "1.4 MB", view: PDF },
  { name: "notes.md", size: "1 KB", view: NOTES },
  { name: "report.docx", size: "86 KB", view: WORD },
];

function PreviewDemo() {
  const [frame] = useFrames(SHOWN.length, (i) => (i === SHOWN.length - 1 ? 3200 : 2100));
  return (
    <Win class="ix-split">
      <div class="ix-pane ix-list">
        <Row name="assets" dir />
        <Row name="src" dir />
        <For each={SHOWN}>{(f, i) => <Row name={f.name} size={f.size} on={i() === frame()} />}</For>
        <Row name="todo.txt" size="1 KB" />
      </div>
      <div class="ix-pane ix-preview">
        <div class="ix-preview-head">{SHOWN[frame()].name}</div>
        <div class="ix-preview-body" classList={{ flush: SHOWN[frame()].view === PDF }}>{SHOWN[frame()].view}</div>
      </div>
    </Win>
  );
}

// ---------- archives ----------

const ZIP = {
  menu: 0, // right-clicked in its folder
  inside: 1, // opened: the bar offers to extract it
  picked: 2, // a file picked and previewed: the bar offers that too
  choosing: 3, // Extract to…
  running: 4,
  done: 5,
};

function ZipDemo() {
  const [frame] = useFrames(6, (i) => [1900, 2300, 2500, 2600, 1500, 3400][i]);
  const inside = () => frame() >= ZIP.inside && frame() <= ZIP.running;
  const picked = () => frame() >= ZIP.picked;
  return (
    <Win>
      <Crumbs parts={frame() === ZIP.done ? ["projects"] : inside() ? ["Downloads", "brand.zip"] : ["Downloads"]} />
      <Show when={inside()}>
        {/* The real bar over an archive's items (ArchiveBar in FileView.tsx). */}
        <div class="bin-bar archive-bar ix-bar-row">
          <FileArchive size={15} />
          <span class="bin-count">ZIP archive</span>
          <Show when={picked()} fallback={<span class="bin-hint">Read-only: extract it to make changes</span>}>
            <span class="bin-hint" />
            <span class="btn"><PackageOpen size={14} />Extract 1 selected item…</span>
          </Show>
          <span class="btn" classList={{ lit: frame() === ZIP.choosing }}><FolderOpen size={14} />Extract to…</span>
          <span class="btn accent"><PackageOpen size={14} />Extract all</span>
        </div>
      </Show>
      <div class="ix-grow ix-row-of-panes">
        <Switch>
          <Match when={inside()}>
            <div class="ix-pane ix-list ix-swap ix-half">
              <Row name="fonts" dir />
              <Row name="ferret.png" size="42 KB" on={picked()} />
              <Row name="guidelines.docx" size="310 KB" />
              <Row name="logo-dark.svg" size="6 KB" />
            </div>
            <div class="ix-pane ix-preview">
              <Show when={picked()} fallback={<div class="ix-preview-empty">Nothing unpacked: it's all still in the zip</div>}>
                <div class="ix-preview-head">ferret.png</div>
                <div class="ix-preview-body">{PICTURE}</div>
              </Show>
            </div>
          </Match>
          <Match when={frame() === ZIP.done}>
            <div class="ix-pane ix-list ix-swap">
              <Row name="brand" dir class="ix-arrives" on />
              <Row name="ferret" dir />
              <Row name="website" dir />
              <Row name="notes.md" size="1 KB" />
            </div>
          </Match>
          <Match when={frame() === ZIP.menu}>
            <div class="ix-pane ix-list ix-swap">
              <Row name="drivers" dir />
              <Row name="brand.zip" size="1.2 MB" on />
              <Row name="setup.exe" size="88 MB" />
              <Row name="statement.pdf" size="212 KB" />
              <Row name="todo.txt" size="1 KB" />
            </div>
          </Match>
        </Switch>
      </div>
      <Show when={frame() === ZIP.menu}>
        <div class="ix-menu">
          <div class="on">Browse archive</div>
          <div>Extract here</div>
          <div>Extract to “brand\”</div>
          <div>Extract to…</div>
        </div>
      </Show>
      <Show when={frame() === ZIP.choosing}>
        <div class="ix-form ix-pop">
          <div class="ix-form-title">Extract brand.zip to…</div>
          <Row name="Documents" dir />
          <Row name="Downloads" dir />
          <Row name="projects" dir on />
          <div class="ix-form-actions">
            <span class="btn">Cancel</span>
            <span class="btn accent">Select folder</span>
          </div>
        </div>
      </Show>
      <Show when={frame() === ZIP.running}>
        <div class="job running ix-job">
          <div class="job-top">
            <span class="job-icon"><PackageOpen size={14} /></span>
            <span class="job-label">Extracting brand.zip</span>
          </div>
          <div class="job-bar"><div style={{ width: "64%" }} /></div>
          <div class="job-sub"><span>64%</span></div>
        </div>
      </Show>
      <Show when={frame() === ZIP.done}>
        <div class="toast ix-toast">
          <span>Extracted to projects</span>
          <span class="toast-action">Undo</span>
        </div>
      </Show>
    </Win>
  );
}

// ---------- tabs and split view ----------

function TabsDemo() {
  // One pane, it splits, a file gets picked, the copy lands in the other pane.
  const [frame] = useFrames(4, (i) => [1400, 1200, 1000, 3000][i]);
  return (
    <Win>
      <div class="ix-tabs">
        <div class="ix-tab on"><Folder size={13} /><span>projects</span></div>
        <div class="ix-tab"><Folder size={13} /><span>Downloads</span></div>
        <Plus size={13} class="ix-dim" />
      </div>
      <div class="ix-panes" classList={{ split: frame() > 0 }}>
        <div class="ix-pane ix-list">
          <div class="ix-pane-head on">projects</div>
          <Row name="ferret" dir />
          <Row name="website" dir />
          <Row name="notes.md" />
          <Row name="logo.png" on={frame() >= 2} />
          <Row name="build.log" />
          <Row name="todo.txt" />
        </div>
        <div class="ix-pane ix-list">
          <div class="ix-pane-head">backup</div>
          <Row name="old-site" dir />
          <Row name="2025.zip" />
          <Show when={frame() === 3}><Row name="logo.png" class="ix-arrives" soft /></Show>
        </div>
      </div>
    </Win>
  );
}

// ---------- background jobs ----------

const PROGRESS = [0, 8, 19, 31, 44, 52, 67, 78, 91, 100];
const JOB_FILES = ["brand", "exports", "invoices", "photos", "raw", "site", "videos"];

function JobsDemo() {
  // The list stays usable while the bar runs, then the job ends with its Undo.
  const [frame] = useFrames(PROGRESS.length + 1, (i) => (i < PROGRESS.length - 1 ? 420 : i === PROGRESS.length - 1 ? 500 : 3000));
  const done = () => frame() >= PROGRESS.length;
  const pct = () => PROGRESS[Math.min(frame(), PROGRESS.length - 1)];
  const cursor = () => Math.min(Math.floor(frame() / 2), JOB_FILES.length - 1);
  return (
    <Win>
      <div class="ix-list ix-grow">
        <For each={JOB_FILES}>{(f, i) => <Row name={f} dir on={i() === cursor()} />}</For>
      </div>
      <Show when={!done()} fallback={
        <div class="toast ix-toast">
          <span>Copied 1,204 items to backup</span>
          <span class="toast-action">Undo</span>
        </div>
      }>
        <div class="job running ix-job">
          <div class="job-top">
            <span class="job-icon"><Copy size={14} /></span>
            <span class="job-label">Copying 1,204 items to backup</span>
            <X size={12} class="ix-dim" />
          </div>
          <div class="job-bar"><div style={{ width: `${pct()}%` }} /></div>
          <div class="job-sub"><span>{pct()}%</span></div>
        </div>
      </Show>
    </Win>
  );
}

// ---------- dropzone ----------

const PARKED = [{ name: "logo.png", size: "42 KB" }, { name: "notes.md", size: "1 KB" }];
const POINTER = [
  { x: 66, y: 100 }, // on the two selected files
  { x: 15, y: 112 }, // dragged to the Dropzone
  { x: 15, y: 112 }, // dropped there
  { x: 14, y: 20 }, // another folder
  { x: 14, y: 20 },
  { x: 29, y: 86 }, // "Drag all"
  { x: 60, y: 150 }, // dragged over the list
  { x: 60, y: 150 }, // dropped
];

function DropzoneDemo() {
  const [frame] = useFrames(POINTER.length, (i) => [1200, 900, 900, 800, 900, 800, 900, 3200][i]);
  const parked = () => frame() >= 2;
  const inProjects = () => frame() >= 4;
  return (
    <Win class="ix-split ix-dropzone">
      <div class="ix-side">
        <div class="ix-side-item" classList={{ on: inProjects() }}><Folder size={14} /><span class="ix-name">projects</span></div>
        <div class="ix-side-item" classList={{ on: !inProjects() }}><Folder size={14} /><span class="ix-name">Downloads</span></div>
        <div class="shelf" classList={{ filled: parked(), "drop-hover": frame() === 1 }}>
          <div class="shelf-head">
            <Inbox size={14} />
            <span>Dropzone</span>
            <Show when={parked()}>
              <span class="shelf-count">{PARKED.length}</span>
              <span class="shelf-spacer" />
              <span class="shelf-all">Drag all</span>
            </Show>
          </div>
          <Show when={parked()} fallback={<div class="shelf-hint">Drop files here and browse freely</div>}>
            <div class="shelf-list">
              <For each={PARKED}>
                {(f) => <div class="shelf-item ix-arrives"><Icon name={f.name} /><span class="shelf-name">{f.name}</span></div>}
              </For>
            </div>
          </Show>
        </div>
      </div>
      <div class="ix-pane">
        <Crumbs parts={inProjects() ? ["projects"] : ["Downloads"]} />
        <div class="ix-list ix-swap">
          <Show when={inProjects()} fallback={
            <>
              <Row name="drivers" dir />
              <For each={PARKED}>{(f) => <Row name={f.name} size={f.size} on={frame() < 2} />}</For>
              <Row name="statement.pdf" size="212 KB" />
              <Row name="todo.txt" size="1 KB" />
            </>
          }>
            <Row name="ferret" dir />
            <Row name="website" dir />
            <Show when={frame() === POINTER.length - 1}>
              <For each={PARKED}>{(f) => <Row name={f.name} size={f.size} class="ix-arrives" soft />}</For>
            </Show>
            <Row name="README.md" size="3 KB" />
          </Show>
        </div>
      </div>
      <Pointer {...POINTER[frame()]} count={PARKED.length}
        ghost={frame() === 1 ? "Add 2 items to the shelf" : frame() === 6 ? "Copy 2 items to projects" : undefined} />
    </Win>
  );
}

// ---------- WSL ----------

type Item = [name: string, dir: boolean, size?: string];
const Items = (p: { items: Item[]; first?: boolean }) => (
  <div class="ix-list ix-swap"><For each={p.items}>{([n, dir, size], i) => <Row name={n} dir={dir} size={size} on={p.first && i() === 0} />}</For></div>
);

const WIN_HOME: Item[] = [["Documents", true], ["Downloads", true], ["Pictures", true]];
const LINUX_HOME: Item[] = [["code", true], [".config", true], ["venv", true], [".bashrc", false, "4 KB"], ["deploy.sh", false, "2 KB"]];
const LINUX_CODE: Item[] = [["ferret", true], ["website", true], ["Makefile", false, "1 KB"], ["notes.md", false, "2 KB"]];

function WslDemo() {
  // The distro is already in the sidebar; a click opens it; Ctrl+` opens a terminal right there.
  const [frame] = useFrames(4, (i) => [1500, 2000, 1500, 3400][i], 2);
  const opened = () => frame() >= 1;
  const inCode = () => frame() >= 2;
  return (
    <Win class="ix-split ix-places">
      <div class="ix-side">
        <div class="ix-side-title"><span>Linux</span></div>
        <div class="ix-side-item" classList={{ on: opened() }}><Terminal size={14} /><span class="ix-name">Ubuntu-26.04</span><span class="side-badge ix-badge">default</span></div>
        <div class="ix-side-item"><Terminal size={14} /><span class="ix-name">Debian</span></div>
        <div class="ix-side-title"><span>Drives</span></div>
        <div class="ix-side-item" classList={{ on: !opened() }}><HardDrive size={14} /><span class="ix-name">Local Disk (C:)</span></div>
      </div>
      <div class="ix-pane">
        <Crumbs parts={!opened() ? ["Local Disk (C:)", "Users", "you"] : inCode() ? ["Ubuntu-26.04", "home", "you", "code"] : ["Ubuntu-26.04", "home", "you"]} marked={opened()} />
        <Switch fallback={<Items items={WIN_HOME} />}>
          <Match when={inCode()}><Items items={LINUX_CODE} /></Match>
          <Match when={opened()}><Items items={LINUX_HOME} first /></Match>
        </Switch>
      </div>
      <Show when={frame() === 0}><Pointer x={13} y={46} /></Show>
      <Show when={frame() === 3}>
        <div class="ix-term ix-pop">
          <div class="ix-term-title"><Terminal size={13} />Ubuntu-26.04</div>
          <pre><b>you@pc</b>:<i>~/code</i>$ ls{"\n"}ferret  Makefile  notes.md  website{"\n"}<b>you@pc</b>:<i>~/code</i>$ <span class="ix-caret" /></pre>
        </div>
      </Show>
    </Win>
  );
}

// ---------- NAS and network folders ----------

const NET = {
  found: 0, // it's in the sidebar: a click
  login: 1, // it wants an account
  connecting: 2,
  shares: 3, // what it shares
  share: 4, // one of them
  adding: 5, // not in the list: + and its address
  added: 6,
};

function NetworkDemo() {
  const [frame] = useFrames(7, (i) => [1500, 2800, 1000, 1700, 1800, 3000, 3000][i], NET.share);
  const signedIn = () => frame() >= NET.shares && frame() < NET.adding;
  const added = () => frame() === NET.added;
  const crumbs = () => (added() ? ["\\\\192.168.0.10\\backup"] : frame() === NET.share ? ["\\\\NAS\\media"] : frame() === NET.shares ? ["\\\\NAS"] : ["Local Disk (C:)", "Users", "you"]);
  return (
    <Win class="ix-split ix-places">
      <div class="ix-side">
        <div class="ix-side-title"><span>Network</span><span class="ix-title-acts"><RotateCw size={12} /><Plus size={13} classList={{ lit: frame() === NET.adding }} /></span></div>
        <Show when={added()}><div class="ix-side-item on ix-arrives"><Network size={14} /><span class="ix-name">backup</span></div></Show>
        <div class="ix-side-item" classList={{ on: signedIn() }}><Monitor size={14} /><span class="ix-name">NAS</span></div>
        <div class="ix-side-item"><Monitor size={14} /><span class="ix-name">OFFICE-PC</span></div>
        <div class="ix-side-title"><span>Drives</span></div>
        <div class="ix-side-item" classList={{ on: !signedIn() && !added() }}><HardDrive size={14} /><span class="ix-name">Local Disk (C:)</span></div>
      </div>
      <div class="ix-pane">
        <Crumbs parts={crumbs()} />
        <Switch fallback={<div class="ix-faded"><Items items={WIN_HOME} /></div>}>
          <Match when={frame() === NET.shares}><Items items={[["backup", true], ["media", true], ["photos", true]]} /></Match>
          <Match when={frame() === NET.share}><Items items={[["Movies", true], ["Music", true], ["holiday.mp4", false, "1.8 GB"], ["playlist.m3u", false, "2 KB"]]} first /></Match>
          <Match when={added()}><Items items={[["2025", true], ["2026", true], ["laptop.zip", false, "48 GB"], ["phone.zip", false, "12 GB"]]} /></Match>
        </Switch>
      </div>
      <Show when={frame() === NET.login || frame() === NET.connecting}>
        <div class="ix-form ix-pop">
          <div class="ix-form-title">Sign in to NAS</div>
          <div class="ix-form-row"><span>User name</span><span class="ix-form-value">you</span></div>
          <div class="ix-form-row"><span>Password</span><span class="ix-form-value">••••••••••</span></div>
          <div class="ix-form-check"><span class="ix-box on" />Remember (in Windows Credential Manager)</div>
          <div class="ix-form-actions">
            <Show when={frame() === NET.connecting}><span class="ix-connecting"><LoaderCircle size={14} class="spin" />Connecting…</span></Show>
            <span class="btn">Cancel</span>
            <span class="btn accent">Connect</span>
          </div>
        </div>
      </Show>
      <Show when={frame() === NET.adding}>
        <div class="ix-form ix-pop">
          <div class="ix-form-title">Add a network folder</div>
          <div class="ix-form-row"><span>Address</span><span class="ix-form-value">\\192.168.0.10\backup</span></div>
          <div class="ix-form-row"><span>Name</span><span class="ix-form-value">backup</span></div>
          <div class="ix-form-actions">
            <span class="btn">Cancel</span>
            <span class="btn accent">Add and connect</span>
          </div>
        </div>
      </Show>
      <Show when={frame() === NET.found}><Pointer x={13} y={46} /></Show>
      <Show when={frame() === NET.login}><Pointer x={72} y="calc(50% + 66px)" /></Show>
      <Show when={frame() === NET.shares}><Pointer x={44} y={74} /></Show>
    </Win>
  );
}

// ---------- servers ----------

const FORM = [
  ["From ~/.ssh/config", "my-server"],
  ["Name", "my-server"],
  ["Server", "192.168.0.10"],
  ["User", "you"],
  ["Sign in with", "Keys, then password"],
];
const SERVER_FILES: [string, boolean, string?][] = [
  ["projects", true], [".config", true], ["backup.tar.gz", false, "1.2 GB"], ["deploy.sh", false, "2 KB"], ["nginx.conf", false, "3 KB"], ["access.log", false, "48 MB"],
];

function ServersDemo() {
  // The + under Servers, the form, connecting, and then it's a folder like any other.
  const [frame] = useFrames(4, (i) => [1300, 2600, 1300, 3400][i]);
  const added = () => frame() === 3;
  return (
    <Win class="ix-split ix-places">
      <div class="ix-side">
        <div class="ix-side-title"><span>Servers</span><Plus size={13} classList={{ lit: frame() === 0 }} /></div>
        <Show when={added()} fallback={<div class="ix-side-empty">Add a server to browse its files over SSH</div>}>
          <div class="ix-side-item on ix-arrives"><Server size={14} /><span class="ix-name">my-server</span></div>
        </Show>
        <div class="ix-side-title"><span>Drives</span></div>
        <div class="ix-side-item"><HardDrive size={14} /><span class="ix-name">Local Disk (C:)</span></div>
      </div>
      <div class="ix-pane">
        <Crumbs parts={added() ? ["my-server", "home", "you"] : ["Local Disk (C:)", "Users", "you"]} marked={added()} />
        <Show when={added()} keyed fallback={
          <div class="ix-list ix-faded"><Row name="Documents" dir /><Row name="Downloads" dir /><Row name="projects" dir /></div>
        }>
          <div class="ix-list ix-swap"><For each={SERVER_FILES}>{([n, dir, size]) => <Row name={n} dir={dir} size={size} />}</For></div>
        </Show>
      </div>
      <Show when={frame() === 1 || frame() === 2}>
        <div class="ix-form ix-pop">
          <div class="ix-form-title">Add a server</div>
          <For each={FORM}>
            {([label, value]) => <div class="ix-form-row"><span>{label}</span><span class="ix-form-value">{value}</span></div>}
          </For>
          <div class="ix-form-actions">
            <Show when={frame() === 2}><span class="ix-connecting"><LoaderCircle size={14} class="spin" />Connecting…</span></Show>
            <span class="btn">Cancel</span>
            <span class="btn accent">Add and connect</span>
          </div>
        </div>
      </Show>
      <Show when={frame() < 2}>
        {/* The form is centered, so its button follows the window's middle. */}
        <Pointer {...(frame() === 0 ? { x: 29, y: 18 } : { x: 72, y: "calc(50% + 88px)" })} />
      </Show>
    </Win>
  );
}

// ---------- command palette ----------

const COMMANDS = [
  { label: "Split view (two folders side by side)", keys: "Ctrl+\\" },
  { label: "Show hidden items", keys: "Ctrl+H" },
  { label: "Reopen closed tab", keys: "Ctrl+Shift+T" },
  { label: "Toggle preview pane", keys: "Alt+P" },
  { label: "Undo", keys: "Ctrl+Z" },
  { label: "Open in Terminal", keys: "Ctrl+`" },
  { label: "New window", keys: "Ctrl+N" },
];
/** What gets typed: each entry is one frame. */
const TYPED = ["split", "hidden", "reopen"].flatMap((q) => [...Array(q.length + 1).keys()].map((n) => q.slice(0, n)).concat(q));

function CommandsDemo() {
  const [frame] = useFrames(TYPED.length, (i) => (TYPED[i] === TYPED[i + 1] ? 1900 : TYPED[i] === "" ? 700 : 90));
  const query = () => TYPED[frame()];
  const shown = createMemo(() => COMMANDS.filter((c) => c.label.toLowerCase().includes(query())));
  const parts = (label: string) => {
    const at = label.toLowerCase().indexOf(query());
    return [label.slice(0, at), label.slice(at, at + query().length), label.slice(at + query().length)];
  };
  return (
    <Win class="ix-palette">
      <div class="palette-input">
        <Command size={18} />
        <span class="ix-typed">
          <Show when={query()} fallback={<span class="ix-placeholder">Type a command…</span>}>{query()}</Show>
          <span class="ix-caret" />
        </span>
      </div>
      <div class="palette-list">
        <For each={shown()}>
          {(c, i) => (
            <div class="palette-item cmd" classList={{ active: i() === 0 }}>
              <Command size={14} class="muted" />
              <span class="palette-name">{parts(c.label)[0]}<mark>{parts(c.label)[1]}</mark>{parts(c.label)[2]}</span>
              <span class="palette-path" />
              <kbd>{c.keys}</kbd>
            </div>
          )}
        </For>
      </div>
      <div class="palette-foot"><span>↑↓ choose · Enter run · Esc close</span></div>
    </Win>
  );
}

// ---------- setup ----------

/** Not moving: this one answers to the color you pick below it. */
function SetupDemo() {
  return (
    <Win class="ix-split ix-places">
      <div class="ix-side">
        <div class="ix-side-item on"><Folder size={14} /><span class="ix-name">projects</span></div>
        <div class="ix-side-item"><HardDrive size={14} /><span class="ix-name">Local Disk (C:)</span></div>
      </div>
      <div class="ix-pane ix-list">
        <Row name="ferret" dir />
        <Row name="notes.md" size="1 KB" on />
        <Row name="logo.png" size="42 KB" />
        <div class="job-bar ix-bar"><div style={{ width: "62%" }} /></div>
      </div>
    </Win>
  );
}

export const DEMOS: Record<IntroDemo, Component> = {
  search: SearchDemo,
  preview: PreviewDemo,
  zip: ZipDemo,
  tabs: TabsDemo,
  jobs: JobsDemo,
  dropzone: DropzoneDemo,
  wsl: WslDemo,
  network: NetworkDemo,
  servers: ServersDemo,
  commands: CommandsDemo,
  setup: SetupDemo,
};
