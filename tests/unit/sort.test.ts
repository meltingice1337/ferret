import { describe, expect, it } from "vitest";
import { groupEntries, sortEntries } from "../../src/lib/sortgroup";
import { entry, view } from "./helpers";

const names = (l: { name: string }[]) => l.map((e) => e.name);

describe("sorting", () => {
  it("sorts names naturally (file2 before file10), folders first", () => {
    const l = [entry("file10.txt"), entry("file2.txt"), entry("zeta", { isDir: true }), entry("File1.txt")];
    expect(names(sortEntries(l, view()))).toEqual(["zeta", "File1.txt", "file2.txt", "file10.txt"]);
  });
  it("can mix folders in", () => {
    const l = [entry("b.txt"), entry("a", { isDir: true })];
    expect(names(sortEntries(l, view({ foldersFirst: false, sortDir: "desc" })))).toEqual(["b.txt", "a"]);
  });
  it("sorts by size with counted folder sizes, ties by name", () => {
    const l = [entry("small.txt", { size: 10 }), entry("big", { isDir: true }), entry("also10.txt", { size: 10 })];
    const sizeOf = (e: { isDir: boolean; size: number }) => (e.isDir ? 1000 : e.size);
    expect(names(sortEntries(l, view({ sortBy: "size", sortDir: "desc", foldersFirst: false }), sizeOf))).toEqual(["big", "small.txt", "also10.txt"]);
  });
  it("doesn't change the list it was given", () => {
    const l = [entry("b"), entry("a")];
    sortEntries(l, view());
    expect(names(l)).toEqual(["b", "a"]);
  });
});

describe("grouping", () => {
  it("is one unnamed group when off", () => {
    const g = groupEntries([entry("a"), entry("b")], view());
    expect(g).toHaveLength(1);
    expect(g[0].label).toBe("");
  });
  it("groups by first letter, keeping the sorted order inside each group", () => {
    const sorted = sortEntries([entry("apple.txt"), entry("banana.txt"), entry("avocado.txt")], view());
    const g = groupEntries(sorted, view({ groupBy: "letter" }));
    expect(g.map((x) => [x.label, names(x.items)])).toEqual([
      ["A", ["apple.txt", "avocado.txt"]],
      ["B", ["banana.txt"]],
    ]);
  });
});
