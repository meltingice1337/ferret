import { describe, expect, it } from "vitest";
import {
  awaitingValue, badFilter, inFolderName, inFolderPath, isLooseMatch, mergeNear, nameColumn, splitName, validDateFilter, validSizeFilter, withoutFilters,
} from "../../src/lib/searchquery";

describe("awaitingValue", () => {
  it("sees a filter with no value yet", () => {
    for (const q of ["ext:", "report size:>", "modified:<=", 'in:"', "content:", "report type:"]) expect(awaitingValue(q), q).toBe(true);
  });
  it("leaves the rest alone", () => {
    for (const q of ["ext:pdf", "size:>1mb", "C:", "foo:", "report", 'in:"C:\\x"']) expect(awaitingValue(q), q).toBe(false);
  });
});

describe("isLooseMatch", () => {
  it("is false when every word appears in the path", () => {
    expect(isLooseMatch("C:\\trips\\vacation photos\\beach.jpg", "vacation photos")).toBe(false);
    expect(isLooseMatch("C:\\docs\\Résumé.pdf", "resume")).toBe(false);
  });
  it("is true for scattered letters", () => {
    expect(isLooseMatch("C:\\sys\\vmbus-action-hotspot.dll", "vacation photos")).toBe(true);
  });
  it("doesn't judge filters, exclusions or wildcards", () => {
    expect(isLooseMatch("C:\\a.txt", "size:>1mb")).toBe(false);
    expect(isLooseMatch("C:\\a.txt", "!old *.txt")).toBe(false);
  });
});

describe("nameColumn", () => {
  it("isn't set by one very long name", () => {
    expect(nameColumn([60, 60, 60, 60, 60, 60, 60, 900])).toBe(120);
    expect(nameColumn([200, 210, 220, 230])).toBe(220);
    expect(nameColumn([500, 600, 700, 800])).toBe(260);
    expect(nameColumn([])).toBe(120);
  });
});

describe("inFolderPath", () => {
  it("reads quoted and bare folders", () => {
    expect(inFolderPath('x in:"C:\\Program Files" y')).toBe("C:\\Program Files");
    expect(inFolderPath("in:C:\\dev x")).toBe("C:\\dev");
    expect(inFolderPath("nothing")).toBeNull();
  });
  it("stops an open quote at the next filter", () => {
    expect(inFolderPath('in:"C:\\nope ext:pdf')).toBe("C:\\nope");
    expect(inFolderName('in:"C:\\nope ext:')).toBe("nope");
  });
});

describe("filter values (as search/query.rs reads them)", () => {
  it("accepts sizes", () => {
    for (const v of ["10mb", ">100mb", "<=1.5gb", "1mb..1gb", "empty", "500", ".5k", "=2kib"]) expect(validSizeFilter(v), v).toBe(true);
  });
  it("refuses what search would ignore", () => {
    for (const v of ["abc", ">abc", "10xb", "1mb..", "1..2..3"]) expect(validSizeFilter(v), v).toBe(false);
  });
  it("accepts dates", () => {
    for (const v of ["today", "<7d", ">1y", "2026-01-01", "2026", "2026/03", "2026-01-01..2026-03-31", "<=2mo"]) expect(validDateFilter(v), v).toBe(true);
  });
  it("refuses bad dates", () => {
    for (const v of ["banana", "<7", "2026-13-01", "2026-02-30", "1500", "2026-01-01..x"]) expect(validDateFilter(v), v).toBe(false);
  });
});

describe("badFilter", () => {
  it("finds the first one search can't read", () => {
    expect(badFilter("report size:abc modified:banana")?.raw).toBe("size:abc");
    expect(badFilter("modified:banana")).toMatchObject({ kind: "date", example: "modified:<7d" });
  });
  it("leaves good filters and ones still being typed alone", () => {
    expect(badFilter("report size:>1mb dm:2026-03-04")).toBeNull();
    expect(badFilter("size:")).toBeNull();
    expect(badFilter("size:>")).toBeNull();
    expect(badFilter("resize:abc")).toBeNull();
  });
});

describe("withoutFilters", () => {
  it("keeps the words and exclusions", () => {
    expect(withoutFilters('report ext:pdf in:"C:\\Program Files" size:>1mb !old')).toBe("report !old");
  });
  it("can leave nothing", () => {
    expect(withoutFilters("type:image")).toBe("");
  });
});

describe("inFolderName", () => {
  it("names the folder", () => {
    expect(inFolderName("x in:C:\\dev\\app")).toBe("app");
    expect(inFolderName('in:"C:\\Program Files\\" y')).toBe("Program Files");
    expect(inFolderName("nothing")).toBeNull();
  });
});

describe("splitName", () => {
  it("leaves short names whole", () => {
    expect(splitName("report.txt")).toEqual(["report.txt", ""]);
  });
  it("keeps the end and extension of a long name", () => {
    const name = "a-very-long-build-output-name-3f9a2c.tar.gz";
    const [start, end] = splitName(name);
    expect(start + end).toBe(name);
    expect(end.endsWith(".gz")).toBe(true);
    expect(end.length).toBeGreaterThanOrEqual(8);
  });
  it("doesn't cut an emoji in half", () => {
    const name = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa😀bbbbbbbbb";
    const [start, end] = splitName(name);
    expect(start + end).toBe(name);
    expect(end.charCodeAt(0)).not.toBe(0xde00);
  });
});

describe("mergeNear", () => {
  it("puts near results first without repeating them", () => {
    const a = { path: "C:\\x\\a" }, b = { path: "D:\\b" };
    expect(mergeNear([a], [b, { path: "c:\\X\\A" }])).toEqual([{ hit: a, near: true }, { hit: b, near: false }]);
  });
});
