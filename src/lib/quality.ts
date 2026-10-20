// How close a compressed video looks to its original, for the compress editor's quality check.
// SSIM (structural similarity) on brightness, after averaging the pixels in f × f blocks: that's
// about the size a video is watched at in a window, so film grain the encoder smoothed away
// doesn't count against it, while blocks and smearing do. Calibrated on 1080p clips (camera-like
// grain and motion, test patterns, phone video) encoded from the smallest setting to the best.

export interface Luma { y: Float32Array; w: number; h: number }

/** Brightness of RGBA pixels, averaged over f × f blocks (a partial block at the edge is dropped). */
export function luma(rgba: ArrayLike<number>, w: number, h: number, f: number): Luma {
  const W = Math.floor(w / f), H = Math.floor(h / f);
  const y = new Float32Array(W * H);
  for (let py = 0; py < H * f; py++) {
    const row = Math.floor(py / f) * W;
    for (let px = 0, i = py * w * 4; px < W * f; px++, i += 4) {
      y[row + Math.floor(px / f)] += 0.299 * rgba[i] + 0.587 * rgba[i + 1] + 0.114 * rgba[i + 2];
    }
  }
  const k = 1 / (f * f);
  for (let i = 0; i < y.length; i++) y[i] *= k;
  return { y, w: W, h: H };
}

/** Mean SSIM over 8 × 8 windows: 1 is identical. */
export function ssim(a: Luma, b: Luma): number {
  const C1 = (0.01 * 255) ** 2, C2 = (0.03 * 255) ** 2;
  const w = Math.min(a.w, b.w), h = Math.min(a.h, b.h);
  let sum = 0, n = 0;
  for (let by = 0; by + 8 <= h; by += 8) {
    for (let bx = 0; bx + 8 <= w; bx += 8) {
      let ma = 0, mb = 0, va = 0, vb = 0, cv = 0;
      for (let y = by; y < by + 8; y++) {
        for (let x = bx; x < bx + 8; x++) {
          const pa = a.y[y * a.w + x], pb = b.y[y * b.w + x];
          ma += pa; mb += pb; va += pa * pa; vb += pb * pb; cv += pa * pb;
        }
      }
      ma /= 64; mb /= 64;
      va = va / 64 - ma * ma; vb = vb / 64 - mb * mb; cv = cv / 64 - ma * mb;
      sum += ((2 * ma * mb + C1) * (2 * cv + C2)) / ((ma * ma + mb * mb + C1) * (va + vb + C2));
      n++;
    }
  }
  return n ? sum / n : 1;
}

export type Level = "same" | "good" | "ok" | "poor";

/** What a score (of the worst frame compared) means for watching the video. */
export function verdict(score: number): { level: Level; text: string } {
  if (score >= 0.985) return { level: "same", text: "Looks the same as the original" };
  if (score >= 0.965) return { level: "good", text: "Good: a little softer, easy to watch" };
  if (score >= 0.94) return { level: "ok", text: "Watchable, but softer, with some smearing" };
  return { level: "poor", text: "Poor: visible blocks or smearing" };
}
