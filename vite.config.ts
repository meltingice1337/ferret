import { defineConfig, type Plugin } from "vite";
import solid from "vite-plugin-solid";
// @ts-expect-error type error without @types/node package
import process from "node:process";
// @ts-expect-error type error without @types/node package
import { readdirSync, readFileSync } from "node:fs";
const host = process.env.TAURI_DEV_HOST;

/** pdf.js data it fetches at runtime (PdfPages.tsx): WebAssembly image decoders, color
 *  profiles, standard fonts, character maps. Served at /pdfjs/<dir>/ in dev, copied into
 *  the build. */
const PDFJS_DIRS = ["wasm", "iccs", "standard_fonts", "cmaps"];
const PDFJS_ROOT = "node_modules/pdfjs-dist";
const TYPES: Record<string, string> = { wasm: "application/wasm", js: "text/javascript", pfb: "application/octet-stream", ttf: "font/ttf", icc: "application/octet-stream", bcmap: "application/octet-stream" };

function pdfjsAssets(): Plugin {
  const files = () => PDFJS_DIRS.flatMap((dir) => readdirSync(`${PDFJS_ROOT}/${dir}`).map((name: string) => `${dir}/${name}`));
  return {
    name: "ferret-pdfjs-assets",
    configureServer(server) {
      const known = new Set(files());
      server.middlewares.use("/pdfjs/", (req, res, next) => {
        // Only the listed files: nothing else under node_modules is reachable this way.
        const rel = decodeURIComponent((req.url ?? "").split("?")[0]).replace(/^\/+/, "");
        if (!known.has(rel)) return next();
        res.setHeader("Content-Type", TYPES[rel.split(".").pop() ?? ""] ?? "application/octet-stream");
        res.end(readFileSync(`${PDFJS_ROOT}/${rel}`));
      });
    },
    generateBundle() {
      for (const rel of files()) {
        if (/LICENSE/.test(rel)) continue;
        this.emitFile({ type: "asset", fileName: `pdfjs/${rel}`, source: readFileSync(`${PDFJS_ROOT}/${rel}`) });
      }
    },
  };
}

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [solid(), pdfjsAssets()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
