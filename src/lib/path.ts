// Paths arrive from the backend already normalized: `C:\a\b`, `\\wsl.localhost\Arch\home`.

export const isWsl = (p: string) => /^\\\\wsl(\.localhost|\$)\\/i.test(p);

export const pathKey = (p: string) => (isWsl(p) ? p : p.toLowerCase());

/** Virtual path of the Recycle Bin (same name Explorer's address bar accepts). */
export const RECYCLE = "shell:RecycleBinFolder";
export const isRecycle = (p?: string | null) => !!p && p.toLowerCase() === RECYCLE.toLowerCase();
/** Real file inside the Recycle Bin (C:\$Recycle.Bin\<SID>\$R…). */
export const inRecycleBin = (p: string) => /^[a-z]:\\\$recycle\.bin\\/i.test(p);

export const samePath = (a?: string | null, b?: string | null) =>
  !!a && !!b && pathKey(a) === pathKey(b);

export interface Segment {
  label: string;
  path: string;
}

export function segments(p: string): Segment[] {
  const out: Segment[] = [];
  if (isRecycle(p)) return [{ label: "Recycle Bin", path: RECYCLE }];
  if (p.startsWith("\\\\")) {
    const parts = p.slice(2).split("\\").filter(Boolean);
    if (parts.length < 2) return [{ label: p, path: p }];
    let acc = `\\\\${parts[0]}\\${parts[1]}`;
    out.push({ label: isWsl(p) ? parts[1] : acc, path: acc });
    for (const part of parts.slice(2)) {
      acc += `\\${part}`;
      out.push({ label: part, path: acc });
    }
    return out;
  }
  const parts = p.split("\\").filter(Boolean);
  if (!parts.length) return [];
  let acc = `${parts[0]}\\`;
  out.push({ label: parts[0], path: acc });
  for (const part of parts.slice(1)) {
    acc = acc.endsWith("\\") ? acc + part : `${acc}\\${part}`;
    out.push({ label: part, path: acc });
  }
  return out;
}

export function parent(p: string): string | null {
  const s = segments(p);
  return s.length > 1 ? s[s.length - 2].path : null;
}

export function basename(p: string): string {
  const s = segments(p);
  return s.length ? s[s.length - 1].label : p;
}

/** True if `child` is `dir` or lives under it. */
export function isWithin(child: string, dir: string): boolean {
  const c = pathKey(child);
  const d = pathKey(dir).replace(/\\$/, "");
  return c === d || c.startsWith(d + "\\");
}

const ARCHIVE_RE = /\.(zip|7z|rar|tar|tgz|txz|tzst|tbz2?|gz|xz|zst|bz2|jar|apk|cbz|cbr|xpi|nupkg|vsix|whl|epub)$/i;
/** File names Ferret can browse like folders. */
export const isArchiveName = (p: string) => ARCHIVE_RE.test(p);
