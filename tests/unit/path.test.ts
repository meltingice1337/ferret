import { describe, expect, it } from "vitest";
import {
  basename, isRecycle, isSearch, isVirtual, isWithin, isWsl, parent, pathKey, RECYCLE, samePath, searchPath, searchQuery, segments,
} from "../../src/lib/path";

describe("segments (breadcrumbs)", () => {
  it("splits a drive path, the drive keeping its backslash", () => {
    expect(segments("C:\\Users\\me")).toEqual([
      { label: "C:", path: "C:\\" },
      { label: "Users", path: "C:\\Users" },
      { label: "me", path: "C:\\Users\\me" },
    ]);
  });
  it("treats a share root as one crumb", () => {
    expect(segments("\\\\server\\share\\docs").map((s) => s.label)).toEqual(["\\\\server\\share", "docs"]);
  });
  it("names a WSL distro by itself", () => {
    expect(segments("\\\\wsl.localhost\\Ubuntu\\home").map((s) => s.label)).toEqual(["Ubuntu", "home"]);
  });
  it("shows the Recycle Bin and search tabs as single crumbs", () => {
    expect(segments(RECYCLE)).toEqual([{ label: "Recycle Bin", path: RECYCLE }]);
    expect(segments("search:ext:pdf")).toEqual([{ label: "“ext:pdf”", path: "search:ext:pdf" }]);
  });
});

describe("parent / basename", () => {
  it("walks up to the drive and stops there", () => {
    expect(parent("C:\\a\\b")).toBe("C:\\a");
    expect(parent("C:\\a")).toBe("C:\\");
    expect(parent("C:\\")).toBeNull();
  });
  it("has no parent for virtual places", () => {
    expect(parent(RECYCLE)).toBeNull();
    expect(parent("search:report")).toBeNull();
  });
  it("names the last segment", () => {
    expect(basename("C:\\Users\\me\\notes.txt")).toBe("notes.txt");
  });
});

describe("comparing paths", () => {
  it("ignores case on Windows paths but not inside WSL", () => {
    expect(samePath("C:\\Users", "c:\\users")).toBe(true);
    expect(isWsl("\\\\wsl.localhost\\Ubuntu\\home")).toBe(true);
    expect(pathKey("\\\\wsl.localhost\\Ubuntu\\Home")).not.toBe(pathKey("\\\\wsl.localhost\\Ubuntu\\home"));
  });
  it("knows what's inside a folder (and that a sibling with a longer name isn't)", () => {
    expect(isWithin("C:\\a\\b\\c", "C:\\a")).toBe(true);
    expect(isWithin("C:\\a", "C:\\a")).toBe(true);
    expect(isWithin("C:\\ab", "C:\\a")).toBe(false);
  });
});

describe("virtual places", () => {
  it("recognizes search tabs and round-trips their query", () => {
    const p = searchPath("  ext:pdf modified:<30d ");
    expect(p).toBe("search:ext:pdf modified:<30d");
    expect(isSearch(p)).toBe(true);
    expect(isSearch("Search:x")).toBe(true);
    expect(searchQuery(p)).toBe("ext:pdf modified:<30d");
    expect(isSearch("C:\\search")).toBe(false);
  });
  it("counts the Recycle Bin and search tabs as not real folders", () => {
    expect(isRecycle("shell:recyclebinfolder")).toBe(true);
    expect(isVirtual(RECYCLE) && isVirtual("search:x")).toBe(true);
    expect(isVirtual("C:\\Users")).toBe(false);
  });
});

describe("archiveRoot", () => {
  it("finds the archive a path inside one is in", async () => {
    const { archiveRoot } = await import("../../src/lib/path");
    expect(archiveRoot("C:\\a\\b.zip\\docs\\x")).toBe("C:\\a\\b.zip");
    expect(archiveRoot("C:\\a\\b.zip")).toBe("C:\\a\\b.zip");
    expect(archiveRoot("C:\\a\\b")).toBeNull();
    // A drive's own name never counts, nor a server's path.
    expect(archiveRoot("\\\\ssh:\\home.local\\x.zip")).toBeNull();
  });

  it("draws an archive's top as the archive, its folders as folders", async () => {
    const { folderLook } = await import("../../src/lib/path");
    expect(folderLook("C:\\a\\Photos (23).ZIP")).toEqual({ isDir: false, ext: "zip", isSymlink: false });
    expect(folderLook("C:\\a\\b.tar.gz")).toEqual({ isDir: false, ext: "gz", isSymlink: false });
    expect(folderLook("C:\\a\\b.zip\\2024").isDir).toBe(true);
    expect(folderLook("C:\\a\\b").isDir).toBe(true);
  });
});

describe("server paths", () => {
  it("names the connection, keeps case and walks up to the server's root", async () => {
    const { displayPath, isRemote, remoteParts, setRemoteNames } = await import("../../src/lib/path");
    setRemoteNames((id) => (id === "home.local" ? "Home server" : id));
    const p = "\\\\ssh:\\home.local\\home\\Darius";
    expect(isRemote(p)).toBe(true);
    expect(isRemote("\\\\server\\share")).toBe(false);
    expect(remoteParts(p)).toEqual({ id: "home.local", linux: "/home/Darius" });
    expect(remoteParts("\\\\ssh:\\home.local")).toEqual({ id: "home.local", linux: "/" });
    expect(segments(p).map((s) => s.label)).toEqual(["Home server", "home", "Darius"]);
    expect(parent(p)).toBe("\\\\ssh:\\home.local\\home");
    expect(parent("\\\\ssh:\\home.local")).toBeNull();
    expect(pathKey(p)).toBe(p);
    expect(samePath(p, p.toLowerCase())).toBe(false);
    expect(displayPath(p)).toBe("Home server:/home/Darius");
    expect(displayPath("C:\\Users")).toBe("C:\\Users");
    setRemoteNames((id) => id);
  });
});
