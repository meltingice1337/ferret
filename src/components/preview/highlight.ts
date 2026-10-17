// Lazy Shiki highlighter: themes load once, languages on demand.

import type { HighlighterGeneric } from "shiki";

const EXT_LANG: Record<string, string> = {
  js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "jsx", ts: "typescript", mts: "typescript",
  cts: "typescript", tsx: "tsx", rs: "rust", py: "python", pyw: "python", go: "go", java: "java", kt: "kotlin",
  kts: "kotlin", c: "c", h: "c", cpp: "cpp", cc: "cpp", cxx: "cpp", hpp: "cpp", hh: "cpp", cs: "csharp",
  rb: "ruby", php: "php", swift: "swift", lua: "lua", sh: "bash", bash: "bash", zsh: "bash", fish: "fish",
  ps1: "powershell", psm1: "powershell", bat: "bat", cmd: "bat", sql: "sql", vue: "vue", svelte: "svelte",
  html: "html", htm: "html", css: "css", scss: "scss", sass: "sass", less: "less", dart: "dart", zig: "zig",
  nim: "nim", r: "r", scala: "scala", ex: "elixir", exs: "elixir", erl: "erlang", hs: "haskell", ml: "ocaml",
  clj: "clojure", fs: "fsharp", json: "json", jsonc: "jsonc", json5: "json5", yaml: "yaml", yml: "yaml",
  toml: "toml", xml: "xml", xaml: "xml", csproj: "xml", svg: "xml", plist: "xml", ini: "ini", cfg: "ini",
  conf: "ini", md: "markdown", markdown: "markdown", mdx: "mdx", diff: "diff", patch: "diff", graphql: "graphql",
  gql: "graphql", proto: "proto", tf: "hcl", hcl: "hcl", nix: "nix", vim: "viml", sol: "solidity",
  dockerfile: "docker", makefile: "makefile", mk: "makefile", cmake: "cmake", gradle: "groovy", groovy: "groovy",
  pl: "perl", tex: "latex", log: "log", env: "dotenv", astro: "astro", prisma: "prisma", glsl: "glsl",
  wgsl: "wgsl", hlsl: "hlsl", asm: "asm", v: "v", jl: "julia", m: "objective-c", vb: "vb", csv: "csv",
  tsv: "tsv", reg: "ini", properties: "properties", gitignore: "ignore", dockerignore: "ignore",
};

const NAME_LANG: Record<string, string> = {
  dockerfile: "docker", makefile: "makefile", "cmakelists.txt": "cmake", ".gitignore": "ignore",
  ".env": "dotenv", ".bashrc": "bash", ".zshrc": "bash", ".profile": "bash", gemfile: "ruby", rakefile: "ruby",
  "cargo.lock": "toml", "go.mod": "go", justfile: "makefile",
};

export function langFor(name: string, ext: string): string {
  const n = name.toLowerCase();
  return NAME_LANG[n] ?? (n.startsWith(".env") ? "dotenv" : EXT_LANG[ext] ?? "text");
}

let highlighter: Promise<HighlighterGeneric<any, any>> | null = null;
let bundled: Record<string, unknown> | null = null;
/** Languages being loaded (or loaded): the name to highlight with ("text" if it failed). */
const languages = new Map<string, Promise<string>>();

/** Shiki (engine and themes) and `lang`, loaded once. */
async function load(lang: string) {
  const shiki = await import("shiki");
  bundled ??= shiki.bundledLanguages as Record<string, unknown>;
  highlighter ??= shiki.createHighlighter({ themes: ["github-dark-default", "github-light-default"], langs: [] });
  const h = await highlighter;
  if (lang === "text" || !(lang in bundled)) return { h, l: "text" };
  if (!languages.has(lang)) {
    languages.set(lang, h.loadLanguage(lang as any).then(() => lang, () => "text"));
  }
  return { h, l: await languages.get(lang)! };
}

/** Starts loading Shiki and `lang` without highlighting anything: for a file still being read,
 *  or ahead of the first code preview (then only its language is left to load). */
export function prepare(lang = "text") {
  load(lang).catch(() => {});
}

export async function highlight(code: string, lang: string, dark: boolean): Promise<string> {
  const { h, l } = await load(lang);
  return h.codeToHtml(code, { lang: l, theme: dark ? "github-dark-default" : "github-light-default" });
}
