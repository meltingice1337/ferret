import type { Entry, ViewSettings } from "../../src/lib/types";

/** A list entry with sensible defaults: `entry("a.txt")`, `entry("src", { isDir: true })`. */
export function entry(name: string, over: Partial<Entry> = {}): Entry {
  const isDir = over.isDir ?? false;
  const dot = name.lastIndexOf(".");
  return {
    name,
    path: `C:\\t\\${name}`,
    isDir,
    isSymlink: false,
    hidden: false,
    size: 0,
    modified: 0,
    created: 0,
    ext: isDir || dot <= 0 ? "" : name.slice(dot + 1).toLowerCase(),
    inArchive: false,
    ...over,
  };
}

export const view = (over: Partial<ViewSettings> = {}): ViewSettings => ({
  mode: "list", sortBy: "name", sortDir: "asc", groupBy: "none", foldersFirst: true, showHidden: false, iconSize: 96, previewOpen: false,
  ...over,
});
