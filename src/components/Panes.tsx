// Split view: a tab's folder and, once split (Ctrl+\), a second pane next to it. Each pane is a
// Tab of its own; the one in use gets the toolbar, preview and keys (state.ts: activeTab).

import { Show } from "solid-js";
import { X } from "../lib/icons";
import { basename, folderLook, isRecycle, isSearch } from "../lib/path";
import { closeSplit, fileList, focusPane, paneOf, searchTitle, type Tab } from "../lib/state";
import FileIcon from "./FileIcon";
import FileView from "./FileView";

const label = (p: string) => (isSearch(p) ? searchTitle(p) : isRecycle(p) ? "Recycle Bin" : basename(p) || p);

function Pane(props: { tab: Tab; owner: Tab }) {
  const split = () => !!props.owner.split[0]();
  const inUse = () => paneOf(props.owner) === props.tab;
  const second = props.tab !== props.owner;
  // Clicking a pane's header gives it the keyboard, like clicking into its list.
  const use = (e: MouseEvent) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
    e.preventDefault();
    focusPane(props.tab);
    requestAnimationFrame(() => fileList()?.focus({ preventScroll: true }));
  };
  return (
    <div class="pane" classList={{ "in-use": split() && inUse(), idle: split() && !inUse() }}>
      <Show when={split()}>
        <div class="pane-head" title={props.tab.p} onMouseDown={use}>
          <FileIcon entry={folderLook(props.tab.p)} size={14} />
          <span class="pane-name">{label(props.tab.p)}</span>
          {/* Left-to-right marks keep the path reading normally in the end-truncating box. */}
          <span class="pane-path">{`‎${props.tab.p}‎`}</span>
          <Show when={second}>
            <button class="icon-btn tiny pane-close" title="Close this pane (Ctrl+W)" onClick={() => closeSplit(props.owner)}><X size={13} /></button>
          </Show>
        </div>
      </Show>
      <FileView tab={props.tab} />
    </div>
  );
}

export default function Panes(props: { tab: Tab }) {
  return (
    <div class="panes" classList={{ split: !!props.tab.split[0]() }}>
      <Pane tab={props.tab} owner={props.tab} />
      <Show when={props.tab.split[0]()} keyed>{(s) => <Pane tab={s} owner={props.tab} />}</Show>
    </div>
  );
}
