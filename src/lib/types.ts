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
  /** "waiting": paused on an item it couldn't handle until the user answers `question`. */
  status: "running" | "waiting" | "done" | "error" | "cancelled";
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
  /** [path, reason] for items left in place (skipped after an error). */
  failed: [string, string][];
  question: string | null;
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
  /** Lines in the whole file, when only its start was read (and it isn't huge). */
  lines?: number | null;
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
  /** Short transitions between previews, folders and tabs (also off when Windows' animation effects are). */
  animations: boolean;
  /** Ctrl+K queries that led somewhere, newest first. */
  searchHistory: string[];
  /** Searches pinned to the sidebar; each opens as a tab of its current matches. */
  savedSearches: SavedSearch[];
  globalView: ViewSettings;
  profiles: Record<ProfileName, { enabled: boolean; view: Partial<ViewSettings> }>;
}

export interface SavedSearch {
  name: string;
  query: string;
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
  /** `split`: the folder of the tab's second pane (split view); `splitFocused`: it had the keyboard. */
  tabs: { path: string; split?: string; splitFocused?: boolean }[];
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
  connections: Connection[];
}

/** A saved server connection (remote/config.rs). */
export interface Connection {
  /** Stable and path-safe: its folders are `\\ssh:\<id>\…`. Empty for a new one. */
  id: string;
  name: string;
  kind: "ssh";
  host: string;
  port: number;
  user: string;
  /** The ~/.ssh/config Host it was made from (read again at each connect). */
  sshAlias: string | null;
  /** "auto": SSH agent, key files, then a password; "key": only `identityFile`; "password". */
  auth: "auto" | "key" | "password";
  identityFile: string | null;
  /** Where it opens (a Linux path); the home folder when empty. */
  startDir: string | null;
  /** Ctrl+K searches it while it's connected. */
  search: boolean;
}

/** What ~/.ssh/config says about a host. */
export interface HostConfig {
  alias: string;
  hostName: string | null;
  user: string | null;
  port: number | null;
  identityFiles: string[];
  identitiesOnly: boolean;
  proxyJump: string | null;
}

/** A question a connection asks (remote/prompt.rs). */
export interface RemotePrompt {
  id: number;
  connection: string;
  kind: "secret" | "hostKey" | "confirm";
  title: string;
  message: string;
  fields: { label: string; secret: boolean }[];
  canRemember: boolean;
  fingerprint: string | null;
  confirmLabel: string | null;
  danger: boolean;
}

/** A song's tags as Windows reads them (empty / 0 when not set). */
export interface AudioInfo {
  title: string;
  artist: string;
  album: string;
  albumArtist: string;
  year: number;
  track: number;
  /** Bits per second. */
  bitrate: number;
  durationMs: number;
}

// ---------- media compression (media.rs) ----------

export interface VideoInfo {
  path: string;
  size: number;
  /** Stored frame size; `rotation` (0/90/180/270) says how players turn it. */
  width: number;
  height: number;
  rotation: number;
  durationMs: number;
  fps: number;
  videoBitrate: number;
  hasAudio: boolean;
  audioBitrate: number;
  codec: string;
}

export interface VideoJob {
  source: string;
  dest: string;
  /** Output size in stored orientation (Windows turns rotated videos upright). */
  width: number;
  height: number;
  /** 0 keeps the source's frame rate. */
  fps: number;
  videoBitrate: number;
  /** 0 drops the sound. */
  audioBitrate: number;
  hevc: boolean;
  trimStartMs?: number;
  trimEndMs?: number;
}

export interface VideoSample {
  path: string;
  bytes: number;
  durationMs: number;
}

export interface ImageInfo {
  path: string;
  size: number;
  /** Upright size. */
  width: number;
  height: number;
  alpha: boolean;
  format: string;
  /** data: URL of the original (upright, ≤ 2048 px). */
  preview: string;
}

export interface ImageOpts {
  png: boolean;
  /** PNG for pictures with transparency, JPEG otherwise (overrides `png`). */
  auto: boolean;
  /** JPEG quality 1–100. */
  quality: number;
  /** Longest side in pixels; 0 keeps the size. */
  maxSide: number;
  /** PNG: down to 256 colours (dithered, transparency kept) when the picture has more. */
  palette?: boolean;
}

export interface ImagePreview {
  bytes: number;
  width: number;
  height: number;
  dataUrl: string;
}
