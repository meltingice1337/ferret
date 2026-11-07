import { describe, expect, it } from "vitest";
import { allSeen, dueOnStart, INTRO_STEPS, replayPlan, type IntroStep } from "../../src/lib/intro";

const ids = INTRO_STEPS.map((s) => s.id);
const step = (id: string): IntroStep => ({ id, title: id, text: id });

describe("the steps", () => {
  it("have ids of their own", () => {
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("all say something", () => {
    for (const s of INTRO_STEPS) {
      expect(s.title, s.id).not.toBe("");
      expect(s.text.length, s.id).toBeGreaterThan(20);
    }
  });

  it("stay short enough to go through in about two minutes, as the welcome screen says", () => {
    expect(INTRO_STEPS.length).toBeLessThanOrEqual(12);
  });
});

describe("dueOnStart", () => {
  it("shows the whole tour the first time, from the welcome screen", () => {
    expect(dueOnStart([])).toEqual({ steps: ids, welcome: true, replay: false });
  });

  it("shows nothing once everything has been seen", () => {
    expect(dueOnStart(ids)).toBeNull();
  });

  it("shows only what was added since, without the welcome screen", () => {
    const all = [step("old"), step("new-one"), step("older"), step("new-two")];
    expect(dueOnStart(["old", "older"], all)).toEqual({ steps: ["new-one", "new-two"], welcome: false, replay: false });
  });

  it("isn't fooled by a step that no longer exists", () => {
    expect(dueOnStart(["gone", ...ids])).toBeNull();
  });
});

describe("allSeen", () => {
  it("counts every step, so a skipped one doesn't come back", () => {
    expect(dueOnStart(allSeen([]))).toBeNull();
  });

  it("keeps what was seen before, without repeats", () => {
    const seen = allSeen(["gone", ids[0]]);
    expect(seen).toContain("gone");
    expect(new Set(seen).size).toBe(seen.length);
  });
});

describe("replayPlan", () => {
  it("is the whole tour, even when it's all been seen", () => {
    expect(replayPlan().steps).toEqual(ids);
    expect(replayPlan().replay).toBe(true);
    expect(replayPlan().welcome).toBe(false);
  });
});
