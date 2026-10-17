import { ArrowLeft, ArrowRight, ArrowUp, ChevronRight, Folder, FolderOpen, ListFilter, PanelLeft, RotateCw, Search, Shapes, Star, X } from "../lib/icons";
import { createEffect, createMemo, createSignal, For, on, Show } from "solid-js";
import { api } from "../lib/api";
import { isRecycle, isWsl, parent, segments } from "../lib/path";
import {
  fileList, activeTab, goBack, goForward, goUp, isFavorite, navigate, openMenu, refresh, setUi, sidebarDocked, toast,
  toggleFavorite, ui, winWidth,
} from "../lib/state";
import FileIcon from "./FileIcon";
import { typeIcon } from "./typeIcons";
import { FILE_TYPES, findType, FOLDERS, typeTokens, withoutTypes, type FileType } from "../lib/filetypes";

export default function Toolbar() {
  const tab = () => activeTab();
  const canBack = () => tab().hIndex[0]() > 0;
  const canFwd = () => tab().hIndex[0]() < tab().history[0]().length - 1;

  return (
    <div class="toolbar">
      <div class="nav-btns">
        <Show when={!sidebarDocked()}>
          <button class="icon-btn sidebar-toggle" title="Show sidebar (Ctrl+B)"
            onClick={() => (winWidth() < 860 ? setUi("sidebarOverlay", !ui.sidebarOverlay) : setUi("sidebarHidden", false))}>
            <PanelLeft size={16} />
          </button>
        </Show>
        <button class="icon-btn" disabled={!canBack()} title="Back (Alt+←)" onClick={() => goBack(tab())}><ArrowLeft size={16} /></button>
        <button class="icon-btn" disabled={!canFwd()} title="Forward (Alt+→)" onClick={() => goForward(tab())}><ArrowRight size={16} /></button>
        <button class="icon-btn" disabled={!parent(tab().p)} title="Up (Alt+↑)" onClick={() => goUp(tab())}><ArrowUp size={16} /></button>
        <button class="icon-btn" title="Refresh (F5)" onClick={() => refresh(tab())}>
          <RotateCw size={15} classList={{ spin: tab().loading[0]() }} />
        </button>
        <button class="icon-btn star" classList={{ on: isFavorite(tab().p) }}
          title={isFavorite(tab().p) ? "Remove from favorites (Ctrl+D)" : "Add to favorites (Ctrl+D)"}
          onClick={() => toggleFavorite(tab().p, true)}>
          <Star size={15} fill={isFavorite(tab().p) ? "currentColor" : "none"} />
        </button>
      </div>
      <Show when={ui.editingPath} fallback={<Breadcrumbs />}>
        <PathEditor />
      </Show>
      <FilterBox />
      <button class="icon-btn" title="Search everywhere (Ctrl+K)" onClick={() => setUi("palette", "files")}><Search size={16} /></button>
    </div>
  );
}

/** Width of the "…" button that holds folded folders (incl. spacing). */
const FOLD_W = 30;

function Breadcrumbs() {
  const tab = () => activeTab();
  const segs = createMemo(() => segments(tab().p));
  let bar!: HTMLDivElement;
  // Leading folders fold into "…" so every name shown fits whole; the current folder's
  // name only gets an ellipsis when it can't fit even on its own.
  const [folded, setFolded] = createSignal(0);
  const [squeeze, setSqueeze] = createSignal<number | null>(null);
  let segW: number[] = [];
  let crumbW: number[] = [];

  const fit = () => {
    if (!bar || !segW.length) return;
    const css = getComputedStyle(bar);
    const inner = bar.clientWidth - parseFloat(css.paddingLeft) - parseFloat(css.paddingRight);
    const tail = bar.querySelector<HTMLElement>(".crumb-tail")?.offsetWidth ?? 0;
    const n = segW.length;
    let used = tail;
    let keep = 0;
    for (let i = n - 1; i >= 0; i--) {
      // Folding anything needs room for the "…" button.
      if (keep > 0 && used + segW[i] + (i > 0 ? FOLD_W : 0) > inner) break;
      used += segW[i];
      keep++;
    }
    const fold = n - keep;
    setFolded(fold);
    const room = inner - tail - (fold > 0 ? FOLD_W : 0) - (segW[n - 1] - crumbW[n - 1]);
    setSqueeze(keep === 1 && crumbW[n - 1] > room ? Math.max(40, room) : null);
  };

  // A new path: show everything once to measure each folder's natural width, then fit.
  createEffect(on(() => tab().p, () => {
    setFolded(0);
    setSqueeze(null);
    requestAnimationFrame(() => {
      const els = [...bar.querySelectorAll<HTMLElement>(".crumb-seg")];
      segW = els.map((el) => el.getBoundingClientRect().width);
      crumbW = els.map((el) => el.querySelector<HTMLElement>(".crumb")?.getBoundingClientRect().width ?? 0);
      fit();
    });
  }));

  const siblingMenu = async (e: MouseEvent, dir: string) => {
    e.stopPropagation();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    try {
      const res = await api.listDir(dir);
      const dirs = res.entries.filter((x) => x.isDir && !x.hidden)
        .sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
      const here = tab().p.toLowerCase();
      openMenu(r.left, r.bottom + 4, dirs.length
        ? dirs.slice(0, 80).map((d) => {
            const onPath = here === d.path.toLowerCase() || here.startsWith(d.path.toLowerCase() + "\\");
            return { label: d.name, icon: onPath ? FolderOpen : Folder, checked: onPath ? true : undefined, action: () => navigate(tab(), d.path) };
          })
        : [{ label: "No subfolders", disabled: true }]);
    } catch (err) {
      toast(String(err), "error");
    }
  };

  return (
    <div class="crumbs" ref={(el) => {
      bar = el;
      new ResizeObserver(() => fit()).observe(el);
    }} onClick={() => setUi("editingPath", true)} title="Click to type a path (Ctrl+L)">
      <Show when={folded() > 0}>
        <button class="crumb crumb-fold" title="Folders above" onClick={(e) => {
          e.stopPropagation();
          const r = e.currentTarget.getBoundingClientRect();
          openMenu(r.left, r.bottom + 4, segs().slice(0, folded()).reverse().map((s) => ({
            label: s.label, icon: Folder, action: () => navigate(tab(), s.path),
          })));
        }}>…</button>
      </Show>
      <For each={segs()}>
        {(seg, i) => (
          <span class="crumb-seg" style={i() < folded() ? { display: "none" } : undefined}>
            <Show when={i() > 0}>
              <button class="crumb-sep" title="Subfolders" onClick={(e) => siblingMenu(e, segs()[i() - 1].path)}>
                <ChevronRight size={14} />
              </button>
            </Show>
            <button class="crumb" classList={{ last: i() === segs().length - 1, wsl: i() === 0 && isWsl(seg.path), squeezed: i() === segs().length - 1 && squeeze() !== null }}
              style={i() === segs().length - 1 && squeeze() !== null ? { "max-width": `${squeeze()}px` } : undefined}
              title={seg.path}
              data-drop={seg.path}
              onClick={(e) => { e.stopPropagation(); navigate(tab(), seg.path); }}>
              {/* A drive shows as its letter badge (as in the sidebar), not "C:". */}
              <Show when={i() === 0 && /^[a-z]:$/i.test(seg.label)} fallback={seg.label}>
                <span class="drive-letter" aria-label={seg.label}>{seg.label[0].toUpperCase()}</span>
              </Show>
            </button>
          </span>
        )}
      </For>
      <Show when={segs().length > 0 && !isRecycle(tab().p)}>
        <button class="crumb-sep crumb-tail" title="Subfolders" onClick={(e) => siblingMenu(e, tab().p)}><ChevronRight size={14} /></button>
      </Show>
    </div>
  );
}

function PathEditor() {
  const tab = () => activeTab();
  const [text, setText] = createSignal(tab().p);
  const [suggestions, setSuggestions] = createSignal<{ name: string; path: string }[]>([]);
  const [pick, setPick] = createSignal(-1);
  const cache = new Map<string, { name: string; path: string }[]>();
  let input!: HTMLInputElement;
  let timer: number | undefined;

  queueMicrotask(() => { input.focus(); input.select(); });

  const close = () => {
    setUi("editingPath", false);
    requestAnimationFrame(() => { if (!ui.editingPath) fileList()?.focus({ preventScroll: true }); });
  };

  const suggest = (value: string) => {
    clearTimeout(timer);
    timer = window.setTimeout(async () => {
      const cut = value.lastIndexOf("\\");
      if (cut < 2) return setSuggestions([]);
      const dir = value.slice(0, cut + 1);
      const prefix = value.slice(cut + 1).toLowerCase();
      let dirs = cache.get(dir.toLowerCase());
      if (!dirs) {
        try {
          const res = await api.listDir(dir);
          dirs = res.entries.filter((e) => e.isDir).map((e) => ({ name: e.name, path: e.path }))
            .sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
          cache.set(dir.toLowerCase(), dirs);
        } catch {
          return setSuggestions([]);
        }
      }
      setSuggestions(dirs.filter((d) => d.name.toLowerCase().startsWith(prefix)).slice(0, 8));
      setPick(-1);
    }, 90);
  };

  const submit = async (value: string) => {
    const res = await api.resolveInput(value, tab().p).catch((e) => { toast(String(e), "error"); return null; });
    if (!res) return;
    if (res.kind === "missing") return toast(`Path not found: ${res.path}`, "error");
    close();
    if (res.kind === "dir") navigate(tab(), res.path);
    else {
      const dir = parent(res.path);
      if (dir) await navigate(tab(), dir, { select: res.path });
      api.openPath(res.path).catch((e) => toast(String(e), "error"));
    }
  };

  const accept = (s: { path: string }) => {
    setText(s.path + "\\");
    suggest(s.path + "\\");
    input.focus();
  };

  return (
    <div class="path-edit">
      <input
        ref={input}
        value={text()}
        spellcheck={false}
        onInput={(e) => { setText(e.currentTarget.value); suggest(e.currentTarget.value); }}
        onBlur={() => setTimeout(close, 120)}
        onKeyDown={(e) => {
          const list = suggestions();
          if (e.key === "Escape") { e.preventDefault(); close(); }
          // Ctrl+L again selects the whole path, so the next one replaces it.
          else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "l") { e.preventDefault(); e.currentTarget.select(); }
          else if (e.key === "ArrowDown" && list.length) { e.preventDefault(); setPick((p) => (p + 1) % list.length); }
          else if (e.key === "ArrowUp" && list.length) { e.preventDefault(); setPick((p) => (p <= 0 ? list.length - 1 : p - 1)); }
          else if (e.key === "Tab" && list.length) { e.preventDefault(); accept(list[Math.max(0, pick())]); }
          else if (e.key === "Enter") {
            e.preventDefault();
            const s = list[pick()];
            submit(s ? s.path : text());
          }
          e.stopPropagation();
        }}
      />
      <Show when={suggestions().length}>
        <div class="suggest">
          <For each={suggestions()}>
            {(s, i) => (
              <button class="suggest-item" classList={{ active: i() === pick() }}
                onMouseDown={(e) => { e.preventDefault(); submit(s.path); }}>
                <FileIcon entry={{ isDir: true, ext: "", isSymlink: false }} size={14} />
                <span>{s.name}</span>
              </button>
            )}
          </For>
          <div class="suggest-hint">↑↓ choose · Tab complete · Enter go · ~ /linux %VARS% work</div>
        </div>
      </Show>
    </div>
  );
}

function FilterBox() {
  const tab = () => activeTab();
  let input!: HTMLInputElement;
  createEffect(() => { if (ui.filterOpen) queueMicrotask(() => input?.focus()); });
  const close = () => { tab().filter[1](""); setUi("filterOpen", false); };

  /** The types of file in this folder, with how many of each: pick one to show only those. */
  const typesMenu = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const t = tab();
    const showHidden = t.view[0]().showHidden;
    const entries = t.entries[0]().filter((x) => showHidden || !x.hidden);
    const current = typeTokens(t.filter[0]()).flatMap((x) => x.values.map((v) => findType(v)?.id));
    const set = (id: string | null) => {
      const rest = withoutTypes(t.filter[0]());
      t.filter[1](id ? `${rest} type:${id}`.trim() : rest);
      setUi("filterOpen", true);
      queueMicrotask(() => input?.focus());
    };
    const count = (ft: FileType) => {
      if (ft.id === "folder") return entries.filter((x) => x.isDir).length;
      const exts = new Set(ft.exts);
      return entries.filter((x) => !x.isDir && exts.has(x.ext.toLowerCase())).length;
    };
    const rows = [FOLDERS, ...FILE_TYPES].map((ft) => ({ ft, n: count(ft) })).filter((x) => x.n > 0);
    openMenu(r.left, r.bottom + 4, [
      { heading: true, label: "Show only" },
      { label: "All types", checked: !current.some(Boolean), action: () => set(null) },
      { separator: true },
      ...rows.map(({ ft, n }) => ({
        label: ft.label, icon: typeIcon(ft.id), shortcut: n.toLocaleString(), checked: current.includes(ft.id) || undefined, action: () => set(ft.id),
      })),
      ...(rows.length ? [] : [{ label: "No files here", disabled: true }]),
    ]);
  };

  return (
    <Show when={ui.filterOpen || tab().filter[0]()} fallback={
      <button class="icon-btn" title="Filter this folder (Ctrl+F)" onClick={() => setUi("filterOpen", true)}><ListFilter size={15} /></button>
    }>
      <div class="filter">
        <ListFilter size={14} />
        <input ref={input} placeholder="Filter folder…" value={tab().filter[0]()} spellcheck={false}
          title={"Shows names with these words. Also understands type:image, ext:pdf,docx and !word (leave out)"}
          onInput={(e) => tab().filter[1](e.currentTarget.value)}
          onBlur={() => !tab().filter[0]() && setUi("filterOpen", false)}
          onKeyDown={(e) => {
            if (e.key === "Escape") { e.preventDefault(); close(); fileList()?.focus(); }
            if (e.key === "Enter" || e.key === "ArrowDown") { e.preventDefault(); fileList()?.focus(); }
            e.stopPropagation();
          }} />
        <button class="icon-btn tiny" classList={{ on: typeTokens(tab().filter[0]()).length > 0 }} title="Show only a type of file"
          onMouseDown={(e) => e.preventDefault()} onClick={typesMenu}><Shapes size={13} /></button>
        <button class="icon-btn tiny" title="Clear filter" onClick={close}><X size={13} /></button>
      </div>
    </Show>
  );
}
