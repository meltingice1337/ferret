// Release notes for the "What's new" dialog: grouping the feed's commit subjects, and keeping
// only the releases newer than the installed copy. Pure (no Tauri), so it's unit-tested.

const SECTIONS: Record<string, string> = { feat: "New", fix: "Fixes", perf: "Improvements", refactor: "Improvements", style: "Improvements" };
const ORDER = ["New", "Improvements", "Fixes", "Other changes"];

/** "## Bug fixes" and the like map onto the same sections as commit prefixes. */
function headingSection(text: string) {
  if (/feat|new|added/i.test(text)) return "New";
  if (/fix|bug/i.test(text)) return "Fixes";
  if (/perf|improv|chang|polish/i.test(text)) return "Improvements";
  return text;
}

const VERSION_HEADING = /^#{1,6}\s+v?(\d+\.\d+\.\d+)\s*$/;

/** -1, 0 or 1, comparing "1.2.3" versions. */
function compareVersions(a: string, b: string) {
  const x = a.split(".").map(Number), y = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) if ((x[i] ?? 0) !== (y[i] ?? 0)) return (x[i] ?? 0) < (y[i] ?? 0) ? -1 : 1;
  return 0;
}

/**
 * The feed lists several releases, each under "## vX.Y.Z": keeps the ones newer than `since`,
 * so a copy that skipped versions sees everything it's getting. Notes without version headings
 * (older feeds) pass through unchanged.
 */
function newerThan(md: string, since: string) {
  if (!since || !md.split(/\r?\n/).some((l) => VERSION_HEADING.test(l.trim()))) return md;
  let keep = true;
  return md.split(/\r?\n/).filter((raw) => {
    const v = raw.trim().match(VERSION_HEADING);
    if (v) { keep = compareVersions(v[1], since) > 0; return false; }
    return keep || /^[-*]?\s*\*\*full changelog/i.test(raw.trim());
  }).join("\n");
}

/**
 * Release notes are the commit subjects since the last release ("- feat: …", "- fix(x): …")
 * plus a "Full changelog" link, possibly with "## Heading" sections written by hand.
 * Groups them for reading; items keep their inline markdown (rendered by the dialog).
 * With `since` (the installed version), covers every release after it.
 */
export function parseNotes(md: string, since = "") {
  const groups = new Map<string, string[]>();
  let link = "";
  let heading = "";
  md = newerThan(md, since);
  for (const raw of md.split(/\r?\n/)) {
    if (VERSION_HEADING.test(raw.trim())) { heading = ""; continue; }
    const h = raw.trim().match(/^#{1,6}\s+(.+)$/);
    if (h) { heading = headingSection(h[1].trim()); continue; }
    const line = raw.trim().replace(/^[-*]\s+/, "");
    if (!line || /^no changes listed$/i.test(line)) continue;
    const full = line.match(/^\*\*full changelog:?\*\*:?\s*(\S+)/i);
    if (full) {
      // The feed's link compares against the previous release; start it at the installed one.
      link = since ? full[1].replace(/\/compare\/v[\d.]+\.\.\./, `/compare/v${since}...`) : full[1];
      continue;
    }
    const m = line.match(/^(\w+)(?:\([^)]*\))?!?:\s*(.+)$/);
    if (m?.[1].toLowerCase() === "release") continue;
    const title = (m && SECTIONS[m[1].toLowerCase()]) || heading || "Other changes";
    const text = m ? m[2] : line;
    // Capitalize the first letter, even behind markdown like "**".
    groups.set(title, [...(groups.get(title) ?? []), text.replace(/\p{L}/u, (c) => c.toUpperCase())]);
  }
  const order = [...ORDER.filter((t) => groups.has(t) && t !== "Other changes"),
    ...[...groups.keys()].filter((t) => !ORDER.includes(t)),
    ...(groups.has("Other changes") ? ["Other changes"] : [])];
  return { sections: order.map((title) => ({ title, items: groups.get(title)! })), link };
}
