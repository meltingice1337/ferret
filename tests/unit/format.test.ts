import { describe, expect, it } from "vitest";
import { formatSize, kindOf, onColor, plural, timeAgo, typeLabel } from "../../src/lib/format";
import { entry } from "./helpers";

describe("formatSize", () => {
  it("uses bytes, then one decimal under 10, whole numbers above", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(1023)).toBe("1023 B");
    expect(formatSize(1536)).toBe("1.5 KB");
    expect(formatSize(15 * 1024)).toBe("15 KB");
    expect(formatSize(3 * 1024 ** 3)).toBe("3.0 GB");
  });
});

describe("plural", () => {
  it("adds an s except for one, with thousands separators", () => {
    expect(plural(1, "item")).toBe("1 item");
    expect(plural(2, "item")).toBe("2 items");
    expect(plural(4228, "item")).toMatch(/^4.228 items$/);
  });
});

describe("timeAgo", () => {
  const now = new Date(2026, 9, 15, 12, 0).getTime();
  it("reads naturally", () => {
    expect(timeAgo(now - 20_000, now)).toBe("just now");
    expect(timeAgo(now - 5 * 60_000, now)).toBe("5 min ago");
    expect(timeAgo(now - 3 * 3_600_000, now)).toBe("3 hours ago");
    expect(timeAgo(now - 30 * 3_600_000, now)).toBe("yesterday");
    expect(timeAgo(now - 4 * 86_400_000, now)).toBe("4 days ago");
    expect(timeAgo(now - 90 * 86_400_000, now)).toMatch(/^\d{4}-\d{2}-\d{2}$/);
  });
});

describe("kinds and type labels", () => {
  it("classifies by extension, folders first", () => {
    expect(kindOf(entry("a.png"))).toBe("image");
    expect(kindOf(entry("a.pdf"))).toBe("pdf");
    expect(kindOf(entry("src", { isDir: true }))).toBe("folder");
  });
  it("names unknown types after their extension", () => {
    expect(typeLabel(entry("data.zzz"))).toBe("ZZZ file");
    expect(typeLabel(entry("shot.png"))).toBe("PNG image");
    expect(typeLabel(entry("photo.jpg"))).toBe("JPEG image");
    expect(typeLabel(entry("clip.webm"))).toBe("WEBM video");
    expect(typeLabel(entry("README"))).toBe("File");
  });
});

describe("onColor", () => {
  it("picks dark text on light colors and light text on dark ones", () => {
    expect(onColor("#ffffff")).not.toBe(onColor("#000000"));
  });
});
