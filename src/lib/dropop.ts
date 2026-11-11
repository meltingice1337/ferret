// Copy or move, for files dropped on a folder: Explorer's rule. Pure, so unit tests reach it.

/** DROPEFFECT bits: what the app files are dragged from allows done with them. */
export const DROPEFFECT_COPY = 1;
export const DROPEFFECT_MOVE = 2;

/** The drive (`c:`) or share (`server\share`) a path is on. */
const volume = (p: string) => (p.startsWith("\\\\") ? p.split("\\").slice(2, 4).join("\\") : p.slice(0, 2)).toLowerCase();

export const sameVolume = (a: string, b: string) => volume(a) === volume(b);

export interface DropKeys {
  ctrlKey: boolean;
  shiftKey: boolean;
}

/**
 * Explorer's rule: Ctrl copies, Shift moves, otherwise the same drive moves.
 *
 * `allowed` is what the app the files come from allows (DROPEFFECT bits), for drops from other
 * apps; leave it out for Ferret's own drags. Many apps (mail, browsers, editors) only offer a
 * copy, and moving their file would take it away from them: those are always copied, whatever
 * keys are held. A source that allows neither gets a copy too.
 */
export function dropOp(sources: string[], target: string, keys: DropKeys, allowed?: number): "copy" | "move" {
  if (allowed !== undefined) {
    if (!(allowed & DROPEFFECT_MOVE)) return "copy";
    if (!(allowed & DROPEFFECT_COPY)) return "move";
  }
  if (keys.ctrlKey) return "copy";
  if (keys.shiftKey) return "move";
  return sources.length > 0 && sources.every((s) => sameVolume(s, target)) ? "move" : "copy";
}

/** The source lets its files be taken away (moved, or recycled): see `dropOp`. */
export const mayTakeAway = (allowed?: number) => allowed === undefined || (allowed & DROPEFFECT_MOVE) !== 0;
