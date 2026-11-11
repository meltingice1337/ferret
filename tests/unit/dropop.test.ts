import { describe, expect, it } from "vitest";
import { DROPEFFECT_COPY, DROPEFFECT_MOVE, dropOp, mayTakeAway, sameVolume } from "../../src/lib/dropop";

const none = { ctrlKey: false, shiftKey: false };
const ctrl = { ctrlKey: true, shiftKey: false };
const shift = { ctrlKey: false, shiftKey: true };
// What Explorer allows: copy, move and link.
const EXPLORER = DROPEFFECT_COPY | DROPEFFECT_MOVE | 4;

describe("dropOp (copy or move, Explorer's rule)", () => {
  it("moves on the same drive and copies across drives", () => {
    expect(dropOp(["C:\\a\\x.png"], "C:\\b", none)).toBe("move");
    expect(dropOp(["C:\\a\\x.png"], "c:\\B", none)).toBe("move");
    expect(dropOp(["C:\\a\\x.png"], "D:\\b", none)).toBe("copy");
    expect(dropOp(["\\\\nas\\share\\x.png"], "\\\\NAS\\Share\\b", none)).toBe("move");
    expect(dropOp(["\\\\nas\\share\\x.png"], "\\\\nas\\other", none)).toBe("copy");
  });

  it("copies when any item is on another drive", () => {
    expect(dropOp(["C:\\a\\x.png", "D:\\y.png"], "C:\\b", none)).toBe("copy");
  });

  it("copies with Ctrl and moves with Shift", () => {
    expect(dropOp(["C:\\a\\x.png"], "C:\\b", ctrl)).toBe("copy");
    expect(dropOp(["C:\\a\\x.png"], "D:\\b", shift)).toBe("move");
    expect(dropOp(["C:\\a\\x.png"], "C:\\b", { ctrlKey: true, shiftKey: true })).toBe("copy");
  });

  it("follows the same rule for an app that allows a move (Explorer)", () => {
    expect(dropOp(["C:\\a\\x.png"], "C:\\b", none, EXPLORER)).toBe("move");
    expect(dropOp(["C:\\a\\x.png"], "D:\\b", none, EXPLORER)).toBe("copy");
    expect(dropOp(["C:\\a\\x.png"], "C:\\b", ctrl, EXPLORER)).toBe("copy");
    expect(dropOp(["C:\\a\\x.png"], "D:\\b", shift, EXPLORER)).toBe("move");
  });

  it("never moves a file away from an app that only allows a copy, whatever the keys", () => {
    // The bug: a picture dropped from a copy-only app on the same drive left its folder.
    expect(dropOp(["C:\\Users\\me\\FerretQA\\media\\ferret.png"], "C:\\Users\\me\\FerretQA\\b", none, DROPEFFECT_COPY)).toBe("copy");
    expect(dropOp(["C:\\a\\x.png"], "C:\\b", shift, DROPEFFECT_COPY)).toBe("copy");
    expect(dropOp(["C:\\a\\x.png"], "C:\\b", none, DROPEFFECT_COPY | 4)).toBe("copy");
  });

  it("copies when the app allows neither, and moves when it allows only a move", () => {
    expect(dropOp(["C:\\a\\x.png"], "C:\\b", none, 0)).toBe("copy");
    expect(dropOp(["C:\\a\\x.png"], "C:\\b", none, 4)).toBe("copy");
    expect(dropOp(["C:\\a\\x.png"], "D:\\b", ctrl, DROPEFFECT_MOVE)).toBe("move");
  });

  it("copies an empty drop rather than guessing", () => {
    expect(dropOp([], "C:\\b", none)).toBe("copy");
  });
});

describe("mayTakeAway (recycling a dropped item)", () => {
  it("is allowed for Ferret's own drags and apps that allow a move", () => {
    expect(mayTakeAway(undefined)).toBe(true);
    expect(mayTakeAway(EXPLORER)).toBe(true);
    expect(mayTakeAway(DROPEFFECT_MOVE)).toBe(true);
  });
  it("is refused for apps that only allow a copy", () => {
    expect(mayTakeAway(DROPEFFECT_COPY)).toBe(false);
    expect(mayTakeAway(0)).toBe(false);
  });
});

describe("sameVolume", () => {
  it("compares drive letters and shares, ignoring case", () => {
    expect(sameVolume("C:\\x", "c:\\y")).toBe(true);
    expect(sameVolume("C:\\x", "D:\\x")).toBe(false);
    expect(sameVolume("\\\\srv\\a\\x", "\\\\SRV\\A")).toBe(true);
  });
});
