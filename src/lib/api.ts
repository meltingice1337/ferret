import { convertFileSrc, invoke as rawInvoke, type InvokeArgs } from "@tauri-apps/api/core";
import { createStore } from "solid-js/store";
import { logCommandFailure } from "./log";
import type {
  ArchiveInfo, Bootstrap, ClipboardFiles, Conflict, DefaultManagerStatus, DirListing, Favorite, FolderSize, FolderView, ImageInfo,
  ImageOpts, ImagePreview, ItemInfo, LaunchTarget, OpenWithApp, Places, Policy, Recent, RecycleInfo, Session, ShellMenuItem, ShellOp,
  Settings, Stats, TextPreview, VideoInfo, VideoJob, VideoSample, ViewSettings,
} from "./types";

/** `invoke` that records failures in the log file before passing them on. */
const invoke = <T>(cmd: string, args?: InvokeArgs) =>
  rawInvoke<T>(cmd, args).catch((e) => {
    logCommandFailure(cmd, args, e);
    throw e;
  });

export const api = {
  bootstrap: () => invoke<Bootstrap>("bootstrap"),
  getPlaces: () => invoke<Places>("get_places"),
  listDir: (path: string) => invoke<DirListing>("list_dir", { path }),
  resolveInput: (input: string, current?: string) =>
    invoke<{ path: string; kind: "dir" | "file" | "missing" }>("resolve_input", { input, current }),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  watchDirs: (paths: string[]) => invoke<void>("watch_dirs", { paths }),

  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  saveSession: (session: Session) => invoke<void>("save_session", { session }),
  setFolderView: (path: string, view: ViewSettings, recursive: boolean) =>
    invoke<void>("set_folder_view", { path, view, recursive }),
  clearFolderView: (path: string) => invoke<void>("clear_folder_view", { path }),
  listFolderViews: () => invoke<FolderView[]>("list_folder_views"),
  clearAllFolderViews: () => invoke<void>("clear_all_folder_views"),

  addFavorite: (path: string, name: string, isDir: boolean) =>
    invoke<Favorite[]>("add_favorite", { path, name, isDir }),
  removeFavorite: (path: string) => invoke<Favorite[]>("remove_favorite", { path }),
  updateFavorite: (path: string, name: string, color: string | null) =>
    invoke<Favorite[]>("update_favorite", { path, name, color }),
  reorderFavorites: (paths: string[]) => invoke<Favorite[]>("reorder_favorites", { paths }),


  pushRecent: (path: string) => invoke<Recent[]>("push_recent", { path }),
  removeRecent: (path: string) => invoke<Recent[]>("remove_recent", { path }),
  clearRecents: () => invoke<Recent[]>("clear_recents"),

  setBackdrop: (mode: string, dark: boolean) => invoke<boolean>("set_backdrop", { mode, dark }),

  // ---- file operations ----
  createFolder: (dir: string, name?: string) => invoke<string>("create_folder", { dir, name }),
  createFile: (dir: string, name: string, content?: string) => invoke<string>("create_file", { dir, name, content }),
  renameItem: (path: string, newName: string) => invoke<string>("rename_item", { path, newName }),
  checkConflicts: (sources: string[], dest: string) => invoke<Conflict[]>("check_conflicts", { sources, dest }),
  transfer: (op: "copy" | "move", sources: string[], dest: string, policy: Policy) =>
    invoke<number>("transfer", { op, sources, dest, policy }),
  undoMoves: (pairs: [string, string][]) => invoke<number>("undo_moves", { pairs }),
  deleteItems: (items: string[], permanent: boolean) => invoke<number>("delete_items", { items, permanent }),
  restoreFromTrash: (items: string[]) => invoke<string[]>("restore_from_trash", { items }),
  findInTrash: (items: string[]) => invoke<string[]>("find_in_trash", { items }),
  itemInfo: (path: string) => invoke<ItemInfo>("item_info", { path }),
  folderStats: (items: string[]) => invoke<Stats>("folder_stats", { items }),
  folderSizes: (requester: number, paths: string[], refresh = false) =>
    invoke<FolderSize[]>("folder_sizes", { requester, paths, refresh }),
  folderSizesInvalidate: (paths: string[], deep = false) => invoke<void>("folder_sizes_invalidate", { paths, deep }),
  setAttributes: (path: string, readonly: boolean, hidden: boolean) =>
    invoke<void>("set_attributes", { path, readonly, hidden }),
  readText: (path: string, maxBytes?: number) => invoke<TextPreview>("read_text", { path, maxBytes }),
  readBytes: (path: string, count?: number) => invoke<number[]>("read_bytes", { path, count }),
  /** Version resource and CPU architecture of executables / libraries. */
  fileDetails: (path: string) => invoke<{ version: [string, string][]; arch: string | null }>("file_details", { path }),
  cancelJob: (id: number) => invoke<void>("cancel_job", { id }),
  dragIcon: (path?: string) => invoke<string>("drag_icon", { path }),

  // ---- windows shell ----
  showNativeMenu: (paths: string[], x: number, y: number) => invoke<string | null>("show_native_menu", { paths, x, y }),
  showNativeBackgroundMenu: (folder: string, x: number, y: number) =>
    invoke<string | null>("show_native_background_menu", { folder, x, y }),
  showProperties: (paths: string[]) => invoke<void>("show_properties", { paths }),
  /** Entries other apps add to Explorer's menu (shown inside Ferret's own menu). */
  shellMenuItems: (paths: string[], background: boolean) =>
    invoke<{ token: number; items: ShellMenuItem[] }>("shell_menu_items", { paths, background }),
  shellMenuInvoke: (token: number, id: string) => invoke<string | null>("shell_menu_invoke", { token, id }),
  openWithDialog: (path: string) => invoke<void>("open_with_dialog", { path }),
  listOpenWith: (path: string) => invoke<OpenWithApp[]>("list_open_with", { path }),
  openWith: (path: string, appId: string) => invoke<void>("open_with", { path, appId }),
  clipboardWriteFiles: (paths: string[], cut: boolean) => invoke<void>("clipboard_write_files", { paths, cut }),
  clipboardReadFiles: () => invoke<ClipboardFiles | null>("clipboard_read_files"),
  openTerminal: (path: string) => invoke<void>("open_terminal", { path }),
  openInVscode: (path: string) => invoke<void>("open_in_vscode", { path }),
  revealInExplorer: (path: string) => invoke<void>("reveal_in_explorer", { path }),

  // ---- archives ----
  archiveMaterialize: (path: string, password?: string) => invoke<string>("archive_materialize", { path, password }),
  archiveExtract: (archive: string, dest: string, entries?: string[], password?: string) =>
    invoke<number>("archive_extract", { archive, dest, entries, password }),
  archiveExtractSmart: (archive: string, password?: string) => invoke<number>("archive_extract_smart", { archive, password }),
  compressZip: (sources: string[], dest: string) => invoke<number>("compress_zip", { sources, dest }),
  archiveInfo: (path: string) => invoke<ArchiveInfo>("archive_info", { path }),
  archiveUnlock: (path: string, password: string) => invoke<void>("archive_unlock", { path, password }),

  // ---- video / picture compression ----
  videoInfo: (path: string) => invoke<VideoInfo>("video_info", { path }),
  videoSample: (job: VideoJob, atMs: number, lengthMs: number) =>
    invoke<VideoSample | null>("video_sample", { job, atMs, lengthMs }),
  compressVideos: (items: VideoJob[]) => invoke<number>("compress_videos", { items }),
  imageInfo: (path: string) => invoke<ImageInfo>("image_info", { path }),
  imagePreview: (path: string, opts: ImageOpts) => invoke<ImagePreview>("image_preview", { path, opts }),
  compressImages: (items: { source: string; dest: string }[], opts: ImageOpts) =>
    invoke<number>("compress_images", { items, opts }),
  mediaRelease: () => invoke<void>("media_release"),

  // ---- Recycle Bin & Windows integration ----
  recycleInfo: () => invoke<RecycleInfo>("recycle_info"),
  emptyRecycleBin: () => invoke<void>("empty_recycle_bin"),
  launchTargets: () => invoke<LaunchTarget[]>("launch_targets"),
  defaultManagerStatus: () => invoke<DefaultManagerStatus>("default_manager_status"),
  setDefaultManager: (enable: boolean) => invoke<DefaultManagerStatus>("set_default_manager", { enable }),
  openInExplorer: (path: string) => invoke<void>("open_in_explorer", { path }),
  /** Whether this window runs as administrator, and whether it was the first one open. */
  windowInfo: () => invoke<{ elevated: boolean; firstWindow: boolean }>("window_info"),
  /** Opens another Ferret window (elevated if this one is). */
  newWindow: (path?: string) => invoke<void>("new_window", { path }),
  /** Restarts this window as administrator (UAC prompt) with these tabs; resolves once it's on screen. */
  restartElevated: (tabs: string[], active: number) => invoke<void>("restart_elevated", { tabs, active }),
  /** Settings, favorites and recents (other windows may have changed them). */
  sharedState: () => invoke<{ settings: Settings; favorites: Favorite[]; recents: Recent[] }>("shared_state"),
  /** Runs an operation through the Windows shell; `elevate` lets it ask for administrator permission. */
  shellOp: (op: ShellOp, elevate: boolean) => invoke<number>("shell_op", { op, elevate }),
  openLogFolder: () => invoke<void>("open_log_folder"),
};

/** Each shown file's version (modified time + size). File and thumbnail URLs carry it, so a
 *  file edited in place loads again everywhere it's shown instead of from the webview's
 *  cache. Reactive: a picture already on screen swaps to the new version by itself. */
const [versions, setVersions] = createStore<Record<string, string>>({});
export function noteVersion(path: string, modified: number | null | undefined, size: number) {
  const k = path.toLowerCase();
  const v = `${modified ?? 0}-${size}`;
  if (versions[k] !== v) setVersions(k, v);
}
const versionOf = (path: string) => versions[path.toLowerCase()];

/** URL that streams a local file into <img>/<video>/<iframe> (Range-capable). */
export const fileUrl = (path: string) => {
  const v = versionOf(path);
  return convertFileSrc(path, "fs") + (v ? `?v=${v}` : "");
};

/** Shell thumbnail (or icon) PNG for any path. */
export const thumbUrl = (path: string, size: number, iconOnly = false) => {
  const v = versionOf(path);
  return `${convertFileSrc(path, "thumb")}?s=${size}${iconOnly ? "&icon=1" : ""}${v ? `&v=${v}` : ""}`;
};
