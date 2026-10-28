import { describe, expect, it } from "vitest";
import { entryFilter } from "../../src/lib/filter";
import { entry } from "./helpers";

const list = [
  entry("Holiday photo.jpg"),
  entry("holiday notes.txt"),
  entry("Budget 2026.xlsx"),
  entry("report.pdf"),
  entry("Holiday", { isDir: true }),
  entry("node_modules", { isDir: true }),
];
const names = (text: string) => {
  const f = entryFilter(text);
  return f ? list.filter(f).map((e) => e.name) : "no filter";
};

describe("folder filter (Ctrl+F)", () => {
  it("filters nothing for empty text", () => {
    expect(names("")).toBe("no filter");
    expect(names("   ")).toBe("no filter");
  });
  it("matches all words, ignoring case", () => {
    expect(names("holiday")).toEqual(["Holiday photo.jpg", "holiday notes.txt", "Holiday"]);
    expect(names("holiday notes")).toEqual(["holiday notes.txt"]);
  });
  it("ignores accents, either way", () => {
    const f = (text: string, name: string) => entryFilter(text)!(entry(name));
    expect(f("cafe", "Café ñ 日本語 файл.txt")).toBe(true);
    expect(f("café", "cafe menu.txt")).toBe(true);
    expect(f("munchen", "München")).toBe(true);
    expect(f("strasse krakow", "Straße Kraków.jpg")).toBe(true);
    expect(f("файл", "Café ñ 日本語 файл.txt")).toBe(true);
    expect(f("cafe", "Coffee.txt")).toBe(false);
  });
  it("keeps quoted words together", () => {
    expect(names('"holiday photo"')).toEqual(["Holiday photo.jpg"]);
  });
  it("leaves out !words", () => {
    expect(names("holiday !photo")).toEqual(["holiday notes.txt", "Holiday"]);
  });
  it("understands types, extensions and folders", () => {
    expect(names("type:image")).toEqual(["Holiday photo.jpg"]);
    expect(names("ext:pdf,xlsx")).toEqual(["Budget 2026.xlsx", "report.pdf"]);
    expect(names("type:folder")).toEqual(["Holiday", "node_modules"]);
    expect(names("type:folder holiday")).toEqual(["Holiday"]);
  });
  it("adds types up (images or PDFs)", () => {
    expect(names("type:image ext:pdf")).toEqual(["Holiday photo.jpg", "report.pdf"]);
  });
  it("ignores a type it doesn't know yet (while typing)", () => {
    expect(names("type:imag")).toBe("no filter");
  });
});
