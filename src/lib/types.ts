export type ViewMode = "list" | "grid";
export type SortBy = "name" | "modified" | "created" | "size" | "type";
export type SortDir = "asc" | "desc";
export type GroupBy = "none" | "date" | "type" | "size" | "letter";
/** List view columns besides Name. */
export type ListColumn = "modified" | "type" | "size";

/** Total size of everything inside a folder (counted in the background). */
export interface FolderSize {
  path: string;
  bytes: number;
  /** Not known for WSL folders. */
  files: number | null;
  /** Some folders inside couldn't be read. */
  partial: boolean;
  /** False while still counting: `bytes` is the total so far. */
  done: boolean;
  /** An earlier count (possibly from an earlier session), being counted again right now. */
  refreshing: boolean;
  /** When it was counted (Unix ms); 0 while counting. */
  countedAt: number;
}

export interface ViewSettings {
  mode: ViewMode;
  sortBy: SortBy;
  sortDir: SortDir;
  groupBy: GroupBy;
  foldersFirst: boolean;
  showHidden: boolean;
  iconSize: number;
  previewOpen: boolean;
}

export type ViewSource = "folder" | "inherited" | "profile" | "global";
export type ProfileName = "media" | "code" | "downloads";

export interface ResolvedView {
  view: ViewSettings;
  source: ViewSource;
  sourcePath: string | null;
  profile: ProfileName | null;
}

export interface Entry {
  name: string;
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  hidden: boolean;
  size: number;
  modified: number | null;
  created: number | null;
  ext: string;
  inArchive: boolean;
  /** Recycle Bin items: the folder the item was deleted from. */
  origin?: string;
}

export type Policy = "replace" | "skip" | "keepBoth";

export interface Conflict {
  name: string;
  source: string;
  target: string;
  sourceSize: number;
  targetSize: number;
  sourceModified: number | null;
  targetModified: number | null;
  isDir: boolean;
}

export interface JobUpdate {
  id: number;
  kind: "copy" | "move" | "delete" | "recycle" | "extract" | "compress" | string;
  label: string;
  status: "running" | "done" | "error" | "cancelled";
  bytesDone: number;
  bytesTotal: number;
  filesDone: number;
  filesTotal: number;
  current: string;
  error: string | null;
  created: string[];
  /** Pre-existing items recycled because they were replaced. */
  replaced: string[];
  /** [source, destination] for everything the job produced. */
  pairs: [string, string][];
  skipped: number;
}

export interface ItemInfo {
  path: string;
  name: string;
  isDir: boolean;
  size: number;
  created: number | null;
  modified: number | null;
  accessed: number | null;
  readonly: boolean;
  hidden: boolean;
  system: boolean;
  isSymlink: boolean;
  linkTarget: string | null;
}

export interface Stats {
  files: number;
  dirs: number;
  bytes: number;
  partial: boolean;
}

export interface TextPreview {
  text: string;
  truncated: boolean;
  binary: boolean;
  encoding: string;
  size: number;
}

export interface OpenWithApp {
  id: string;
  name: string;
  isDefault: boolean;
  icon: string | null;
}

export interface ClipboardFiles {
  paths: string[];
  cut: boolean;
}

export interface ArchiveInfo {
  format: string;
  entries: number;
  files: number;
  dirs: number;
  totalSize: number;
  compressedSize: number;
  encrypted: boolean;
  topLevel: string[];
  comment: string | null;
}

export interface DirListing {
  path: string;
  entries: Entry[];
  resolved: ResolvedView;
}

export interface Settings {
  theme: "dark" | "light" | "auto";
  accent: string;
  backdrop: "mica" | "acrylic" | "solid";
  density: "comfortable" | "compact";
  doubleClickOpen: boolean;
  rememberPerFolder: boolean;
  inheritToSubfolders: boolean;
  restoreSession: boolean;
  /** Sidebar places the user hid (ids like "videos", "recycle"). */
  hiddenPlaces: string[];
  /** Sidebar sections the user hid ("linux", "drives", "recents", "dropzone"). */
  hiddenSections: string[];
  /** List view column widths in px; Name takes the rest. */
  columnWidths: Record<ListColumn, number>;
  /** List view columns the user hid (Name always shows). */
  hiddenColumns: ListColumn[];
  /** Count folder sizes in the background and show them in the Size column. */
  folderSizes: boolean;
  globalView: ViewSettings;
  profiles: Record<ProfileName, { enabled: boolean; view: Partial<ViewSettings> }>;
}

export interface Favorite {
  path: string;
  name: string;
  isDir: boolean;
  color: string | null;
}

export interface Recent {
  path: string;
  lastOpened: number;
  count: number;
}

export interface Drive {
  path: string;
  label: string;
  total: number;
  free: number;
  removable: boolean;
  fs: string;
}

export interface Places {
  home: string;
  known: { id: string; name: string; path: string }[];
  drives: Drive[];
  wsl: { name: string; path: string; isDefault: boolean }[];
  /** Filled in by the frontend (recycle_info), not by get_places. */
  recycle: RecycleInfo;
}

export interface RecycleInfo {
  items: number;
  bytes: number;
}

/** A folder to open from the command line (or a second launch). Empty path = default location. */
export interface LaunchTarget {
  path: string;
  select: string | null;
  /** The tab to show first (a window restarted as administrator). */
  active: boolean;
}

/** An entry another app added to Explorer's right-click menu. */
export interface ShellMenuItem {
  id: string;
  label: string;
  icon: string | null;
  disabled: boolean;
  separator: boolean;
  children: ShellMenuItem[];
  source: "classic" | "modern";
}

/** File operations run through the Windows shell (can ask for administrator permission). */
export type ShellOp =
  | { kind: "transfer"; op: "copy" | "move"; sources: string[]; dest: string; policy: Policy }
  | { kind: "delete"; items: string[]; permanent: boolean }
  | { kind: "rename"; path: string; newName: string }
  | { kind: "newFolder"; dir: string; name: string }
  | { kind: "newFile"; dir: string; name: string }
  | { kind: "moveBack"; pairs: [string, string][] }
  | { kind: "restore"; items: string[] }
  | { kind: "purge"; items: string[] };

export interface DefaultManagerStatus {
  enabled: boolean;
  target: string | null;
  devBuild: boolean;
}

export interface FolderView {
  path: string;
  view: Partial<ViewSettings>;
  recursive: boolean;
  updatedAt: number;
}

export interface Session {
  tabs: { path: string }[];
  active: number;
  sidebarWidth?: number;
  collapsedSections?: string[];
  previewOpen?: boolean;
  previewWidth?: number;
  shelf?: string[];
  sidebarHidden?: boolean;
}

export interface Bootstrap {
  settings: Settings;
  session: Session | null;
  places: Places;
  favorites: Favorite[];
  recents: Recent[];
}
