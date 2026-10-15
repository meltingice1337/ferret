import { describe, expect, it } from "vitest";
import { parseNotes } from "../../src/lib/notes";

// What scripts/release.sh writes into latest.json: every release, newest first.
const feed = [
  "## v0.6.0",
  "- release: v0.6.0",
  "- feat: saved searches",
  "- fix: stuck delete",
  "",
  "## v0.5.9",
  "- feat(ui): window titles",
  "",
  "## v0.5.8",
  "- fix: drag with Alt+Tab",
  "- Add something without a prefix",
  "",
  "**Full changelog:** https://github.com/meltingice1337/ferret/compare/v0.5.9...v0.6.0",
].join("\n");

const flat = (r: ReturnType<typeof parseNotes>) => Object.fromEntries(r.sections.map((s) => [s.title, s.items]));

describe("release notes", () => {
  it("covers every release newer than the installed one", () => {
    const r = parseNotes(feed, "0.5.7");
    expect(flat(r)).toEqual({
      New: ["Saved searches", "Window titles"],
      Fixes: ["Stuck delete", "Drag with Alt+Tab"],
      "Other changes": ["Add something without a prefix"],
    });
    expect(r.link).toBe("https://github.com/meltingice1337/ferret/compare/v0.5.7...v0.6.0");
  });
  it("shows only what's new for a copy one version behind", () => {
    expect(flat(parseNotes(feed, "0.5.9"))).toEqual({ New: ["Saved searches"], Fixes: ["Stuck delete"] });
  });
  it("never turns version headings into sections", () => {
    const titles = parseNotes(feed).sections.map((s) => s.title);
    expect(titles.some((t) => /^v?\d/.test(t))).toBe(false);
  });
  it("skips release commits and keeps old single-release notes working", () => {
    const r = parseNotes("- release: v0.5.0\n- feat: one\n- perf: two\n\n**Full changelog:** https://x/compare/v0.4.0...v0.5.0", "0.4.0");
    expect(flat(r)).toEqual({ New: ["One"], Improvements: ["Two"] });
  });
  it("compares versions numerically (0.10 is newer than 0.9)", () => {
    const r = parseNotes("## v0.10.0\n- feat: ten\n\n## v0.9.0\n- feat: nine", "0.9.0");
    expect(flat(r)).toEqual({ New: ["Ten"] });
  });
});
