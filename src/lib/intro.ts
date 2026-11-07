// The intro tour: what Ferret can do. It shows once on first launch (Skip is always there), and
// Settings and the command palette bring it back.
//
// A new feature worth announcing: add a step here with an id of its own. Anyone who has already
// been through the tour then sees just that step, once, on their next start. Never rename or
// reuse an id: a renamed step shows up as new for everyone. A live demo is optional (a step
// without one shows its keys), and they live in components/IntroDemos.tsx.
//
// Only say what's true of every install: no comparisons nobody measured, and nothing that needs
// an optional part without saying so.

export type IntroDemo = "search" | "preview" | "zip" | "tabs" | "jobs" | "dropzone" | "wsl" | "network" | "servers" | "commands" | "setup";

export interface IntroStep {
  /** What's remembered once it has been shown. */
  id: string;
  /** Its name in the list, and its heading. */
  title: string;
  /** One or two sentences. `Backticks` mark something you'd type. */
  text: string;
  keys?: { key: string; does: string }[];
  demo?: IntroDemo;
  /** Opens the real thing over the tour, which is still there afterwards: search, the command
   *  palette, or the form for adding a network folder. */
  tryIt?: "files" | "commands" | "network";
}

export const INTRO_STEPS: IntroStep[] = [
  {
    id: "search",
    title: "Search every drive",
    text: "Find any file by name, narrow it down with filters, or look for text inside files. Turn on the drive index in the search window for instant results on NTFS drives.",
    keys: [{ key: "Ctrl+K", does: "Search" }, { key: "?", does: "Every filter" }, { key: "Alt+Enter", does: "Show in folder" }],
    demo: "search",
    tryIt: "files",
  },
  {
    id: "preview",
    title: "Look inside files",
    text: "Word documents, PDF, pictures, video, audio, Markdown, fonts and highlighted code, without opening another app.",
    keys: [{ key: "Space", does: "Quick Look" }, { key: "Alt+P", does: "Preview pane" }],
    demo: "preview",
  },
  {
    id: "archives",
    title: "Zips open like folders",
    text: "Browse and preview what's inside zip, 7z, rar and tar files without unpacking them. Then extract all of it, choose where with Extract to, or take only what you've selected.",
    keys: [{ key: "Enter", does: "Open it" }, { key: "Backspace", does: "Back out" }],
    demo: "zip",
  },
  {
    id: "tabs",
    title: "Tabs and split view",
    text: "Keep your folders open in tabs. Split a tab to see two folders side by side, and copy or move files across.",
    keys: [{ key: "Ctrl+T", does: "New tab" }, { key: "Ctrl+\\", does: "Split view" }, { key: "Tab", does: "Other pane" }],
    demo: "tabs",
  },
  {
    id: "jobs",
    title: "Work while files move",
    text: "Copies and moves run in the background with live progress, and undo takes them back. Where there's no Recycle Bin, as in WSL and on SSH servers, Ferret asks before deleting for good.",
    keys: [{ key: "Ctrl+Z", does: "Undo" }, { key: "Ctrl+Y", does: "Redo" }],
    demo: "jobs",
  },
  {
    id: "dropzone",
    title: "Dropzone",
    text: "A parking spot at the bottom of the sidebar: drag files in, browse to where they belong, then drag them out to copy or move them. Nothing moves until then.",
    demo: "dropzone",
  },
  {
    id: "wsl",
    title: "Your WSL distros",
    text: "They're listed under Linux in the sidebar, with nothing to set up. Click one and its files open like any other folder.",
    keys: [{ key: "Ctrl+`", does: "Terminal here" }],
    demo: "wsl",
  },
  {
    id: "network",
    title: "NAS and network folders",
    text: "Computers Windows finds show up under Network in the sidebar. If yours isn't there, press + and type its address, like `\\\\nas\\media` or `192.168.0.10`. Ferret asks for a user name and password when it needs them.",
    keys: [{ key: "Ctrl+L", does: "Type a path" }],
    demo: "network",
    tryIt: "network",
  },
  {
    id: "servers",
    title: "Connect to SSH servers",
    text: "Add one under Servers in the sidebar, from your `~/.ssh/config` or by its address. Its folders browse like any other, a file you edit uploads when you save, and it only connects when you click it.",
    demo: "servers",
  },
  {
    id: "commands",
    title: "Every command, one list",
    text: "Forgot a shortcut? Type what you want to do. The palette runs it and shows you the key for next time.",
    keys: [{ key: "Ctrl+Shift+P", does: "Command palette" }],
    demo: "commands",
    tryIt: "commands",
  },
  {
    id: "setup",
    title: "Make it yours",
    text: "Pick a color and a theme, and the window follows as you choose.",
    demo: "setup",
  },
];

export interface IntroPlan {
  /** Ids of the steps to show, in order. */
  steps: string[];
  /** Starts on the welcome screen. */
  welcome: boolean;
  /** Brought back on purpose: nothing new, and it says Close rather than Skip. */
  replay: boolean;
}

/** What a plain start should show, given the steps already seen: the whole tour the first time,
 *  afterwards only what's been added since, and nothing once it's all been seen. */
export function dueOnStart(seen: readonly string[], all: readonly IntroStep[] = INTRO_STEPS): IntroPlan | null {
  const unseen = all.filter((s) => !seen.includes(s.id)).map((s) => s.id);
  if (!unseen.length) return null;
  return { steps: unseen, welcome: seen.length === 0, replay: false };
}

/** The whole tour, on request. */
export const replayPlan = (all: readonly IntroStep[] = INTRO_STEPS): IntroPlan => ({ steps: all.map((s) => s.id), welcome: false, replay: true });

/** `seen` plus every step: skipping counts too, so a skipped step never comes back by itself. */
export function allSeen(seen: readonly string[], all: readonly IntroStep[] = INTRO_STEPS): string[] {
  return [...new Set([...seen, ...all.map((s) => s.id)])];
}
