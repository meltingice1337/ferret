import { describe, expect, it } from "vitest";
import { luma, ssim, verdict } from "../../src/lib/quality";

/** A w × h RGBA picture from a brightness function. */
function picture(w: number, h: number, at: (x: number, y: number) => number): Uint8ClampedArray {
  const d = new Uint8ClampedArray(w * h * 4);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const i = (y * w + x) * 4, v = at(x, y);
    d[i] = d[i + 1] = d[i + 2] = v;
    d[i + 3] = 255;
  }
  return d;
}
// Detail to lose: soft gradients and stripes.
const scene = (x: number, y: number) => 128 + 60 * Math.sin(x / 7) * Math.cos(y / 11) + (x % 32 < 16 ? 20 : -20);
// Repeatable noise.
let seed = 1;
const noise = () => ((seed = (seed * 16807) % 2147483647) / 2147483647 - 0.5) * 2;

describe("luma", () => {
  it("averages f × f blocks and drops a partial block at the edge", () => {
    const d = picture(5, 4, (x, y) => (x + y) % 2 ? 200 : 0);
    const l = luma(d, 5, 4, 2);
    expect([l.w, l.h]).toEqual([2, 2]);
    for (const v of l.y) expect(v).toBeCloseTo(100, 3);
  });
});

describe("ssim", () => {
  const w = 160, h = 96;
  const clean = picture(w, h, scene);

  it("is 1 for identical pictures", () => {
    expect(ssim(luma(clean, w, h, 1), luma(clean, w, h, 1))).toBeCloseTo(1, 6);
  });

  it("barely drops for a slightly brighter picture", () => {
    expect(ssim(luma(clean, w, h, 1), luma(picture(w, h, (x, y) => scene(x, y) + 4), w, h, 1))).toBeGreaterThan(0.99);
  });

  it("drops when detail is replaced by flat blocks", () => {
    const blocky = picture(w, h, (x, y) => scene(x - (x % 8) + 4, y - (y % 8) + 4));
    expect(ssim(luma(clean, w, h, 1), luma(blocky, w, h, 1))).toBeLessThan(0.9);
  });

  it("counts film grain less once pixels are averaged, as when watching in a window", () => {
    seed = 7;
    const grainy = picture(w, h, (x, y) => scene(x, y) + 18 * noise());
    const full = ssim(luma(grainy, w, h, 1), luma(clean, w, h, 1));
    const half = ssim(luma(grainy, w, h, 2), luma(clean, w, h, 2));
    expect(half).toBeGreaterThan(full);
  });
});

describe("verdict", () => {
  it("puts scores into the four bands", () => {
    expect(verdict(0.995).level).toBe("same");
    expect(verdict(0.985).level).toBe("same");
    expect(verdict(0.97).level).toBe("good");
    expect(verdict(0.95).level).toBe("ok");
    expect(verdict(0.9).level).toBe("poor");
  });
});
