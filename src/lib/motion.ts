// Short entrance animations (Fluent-style: fast, decelerating, no bounce). They never delay
// anything: content is already in place and only fades or slides the last few pixels. Off when
// Windows' "Animation effects" is off (prefers-reduced-motion) or Settings → Animations is.

import { settings } from "./state";

const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
export const motionOn = () => settings.animations !== false && !reduced.matches;

/** A soft deceleration: visible as a glide, settled within the duration. */
const EASE = "cubic-bezier(0.22, 0.61, 0.36, 1)";

const FRAMES: Record<Motion, Keyframe[]> = {
  fade: [{ opacity: 0 }, { opacity: 1 }],
  // Into a subfolder: the new listing arrives from the right; going up, from the left.
  in: [{ opacity: 0, transform: "translateX(14px)" }, { opacity: 1, transform: "none" }],
  out: [{ opacity: 0, transform: "translateX(-14px)" }, { opacity: 1, transform: "none" }],
  lift: [{ opacity: 0, transform: "translateY(5px)" }, { opacity: 1, transform: "none" }],
};
export type Motion = "fade" | "in" | "out" | "lift";

/** When each kind of animation last started: a burst (an arrow key held through a folder)
 *  gets instant swaps, not a stutter of half-played fades. */
const lastRun = new Map<string, number>();
const BURST_MS = 260;

/** Plays a short entrance on `el`. `channel` groups repeats for the burst check. */
export function enter(el: Element | undefined | null, motion: Motion, duration = 160, channel: string = motion) {
  if (!el || !motionOn()) return;
  const now = performance.now();
  const prev = lastRun.get(channel) ?? -Infinity;
  lastRun.set(channel, now);
  if (now - prev < BURST_MS) return;
  el.getAnimations().forEach((a) => a.cancel());
  el.animate(FRAMES[motion], { duration, easing: EASE });
}
