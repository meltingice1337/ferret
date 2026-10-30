// End-to-end tests against the real app (`yarn test:e2e`). Windows only.
//
// Builds a separate test copy of Ferret (identifier com.ferret.e2e, the frontend built in
// "e2e" mode and baked in, in its own target folder so a dev build isn't rebuilt), starts it on
// a fresh fixture folder in FerretQA with an off-screen window that never takes focus, drives
// it through the DevTools protocol, then removes the fixture and the test copy's data. Your
// settings, favorites and dev data are never touched. One test recycles a file and restores it,
// so the Recycle Bin ends where it started. The SSH tests use a server of their own
// (scripts/e2e/sshd, on 127.0.0.1) and a ~/.ssh of their own: yours isn't read or changed.
//
//   yarn test:e2e                 build, then run
//   yarn test:e2e --no-build      reuse the last test build
//   yarn test:e2e --keep          keep the fixture and the window open afterwards
//   yarn test:e2e --only rename   run tests whose name contains "rename"
import { spawn, execSync, execFileSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync, copyFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { connect, sleep } from "./cdp.mjs";
import { fixture, tests } from "./tests.mjs";

const args = process.argv.slice(2);
const flag = (f) => args.includes(f);
const only = args.includes("--only") ? args[args.indexOf("--only") + 1] : null;
const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const tauriDir = join(repo, "src-tauri");
const targetDir = join(tauriDir, "target", "e2e");
const exe = join(targetDir, "debug", "ferret.exe");
const artifacts = join(repo, "e2e-artifacts");
const IDENT = "com.ferret.e2e";
const PORT = 9350;
const green = (s) => `\x1b[32m${s}\x1b[0m`, red = (s) => `\x1b[31m${s}\x1b[0m`, yellow = (s) => `\x1b[33m${s}\x1b[0m`, dim = (s) => `\x1b[2m${s}\x1b[0m`;

function sh(cmd, opts = {}) {
  console.log(dim(`$ ${cmd}`));
  execSync(cmd, { stdio: "inherit", ...opts });
}

// ---- build the test copy ----
if (!flag("--no-build")) {
  sh("yarn vite build --mode e2e --outDir dist-e2e --emptyOutDir --logLevel warn", { cwd: repo });
  sh("cargo build --features tauri/custom-protocol --quiet", {
    cwd: tauriDir,
    env: {
      ...process.env,
      CARGO_TARGET_DIR: targetDir,
      TAURI_CONFIG: JSON.stringify({ identifier: IDENT, build: { frontendDist: "../dist-e2e" } }),
    },
  });
}
if (!existsSync(exe)) throw new Error(`no test build at ${exe} (run without --no-build)`);
// The SSH tests' server (scripts/e2e/sshd): SFTP over a folder of the tests' own, on 127.0.0.1.
const sshdExe = join(tauriDir, "target", "e2e-sshd", "release", "ferret-test-sshd.exe");
if (!flag("--no-build") || !existsSync(sshdExe)) {
  sh(`cargo build --release --quiet --manifest-path "${join(repo, "scripts", "e2e", "sshd", "Cargo.toml")}" --target-dir "${join(tauriDir, "target", "e2e-sshd")}"`, { cwd: repo });
}

// ---- fresh fixture and app data ----
const stamp = Date.now().toString(36);
const root = join(homedir(), "FerretQA", `e2e-${stamp}`);
const needle = `e2e-needle-${stamp}`;
const appData = [join(process.env.APPDATA, IDENT), join(process.env.LOCALAPPDATA, IDENT)];
appData.forEach((d) => rmSync(d, { recursive: true, force: true }));
for (const [rel, content] of Object.entries(fixture(root, needle))) {
  const f = join(root, ...rel.split("/"));
  mkdirSync(dirname(f), { recursive: true });
  writeFileSync(f, content);
}
const webviewData = join(process.env.LOCALAPPDATA, IDENT, "EBWebView");
// Folder sizes are counted ahead in the background: in the test copy only inside the fixture and
// one WSL folder of the test's own (in each running distro), never anyone's own folders.
const wslDir = `/tmp/ferret-e2e-${stamp}`;
let distros = [];
try {
  distros = execFileSync("wsl.exe", ["--list", "--running", "--quiet"]).toString("utf16le")
    .split(/\r?\n/).map((l) => l.replace(/\0/g, "").replace(/^﻿/, "").trim()).filter(Boolean);
} catch {}
const sizesWithin = [root, ...distros.map((d) => `\\\\wsl.localhost\\${d}${wslDir.replace(/\//g, "\\")}`)].join(";");
rmSync(artifacts, { recursive: true, force: true });
mkdirSync(artifacts, { recursive: true });

// ---- the SSH server, with a ~/.ssh of the tests' own (FERRET_SSH_DIR): its key, a config naming
// the server, and the known_hosts its key is trusted in, never the user's ----
const sshRoot = join(homedir(), "FerretQA", `e2e-${stamp}-ssh`);
const ssh = { dir: join(sshRoot, "dot-ssh"), server: join(sshRoot, "server"), home: join(sshRoot, "server", "home", "tester"), path: "\\\\ssh:\\ferret-e2e\\home\\tester", port: 0 };
mkdirSync(ssh.dir, { recursive: true });
mkdirSync(ssh.home, { recursive: true });
writeFileSync(join(ssh.home, "readme.txt"), "on the server\n");
// A folder for its size (counted over SFTP: the test server runs no commands).
mkdirSync(join(ssh.home, "docs", "sub"), { recursive: true });
writeFileSync(join(ssh.home, "docs", "a.bin"), Buffer.alloc(3000));
writeFileSync(join(ssh.home, "docs", "sub", "b.bin"), Buffer.alloc(2000));
execFileSync(sshdExe, ["keygen", join(ssh.dir, "id_ed25519")]);
// Also lets in "pw" with the password "secret" (the password prompt's test).
const sshd = spawn(sshdExe, ["serve", ssh.server, join(ssh.dir, "id_ed25519.pub"), "pw:secret"], { stdio: ["pipe", "pipe", "ignore"] });
ssh.port = await new Promise((resolve, reject) => {
  let out = "";
  sshd.stdout.on("data", (d) => { out += d; const m = /port (\d+)/.exec(out); if (m) resolve(Number(m[1])); });
  sshd.on("exit", () => reject(new Error("the test SSH server ended")));
  setTimeout(() => reject(new Error("the test SSH server didn't start")), 15_000);
});
writeFileSync(join(ssh.dir, "config"), `Host ferret-e2e\n  HostName 127.0.0.1\n  Port ${ssh.port}\n  User tester\n  IdentityFile ${join(ssh.dir, "id_ed25519")}\n`);

// ---- start it (off-screen, no focus) ----
console.log(dim(`fixture: ${root}`));
const app = spawn(exe, [root], {
  env: {
    ...process.env,
    FERRET_E2E: "1",
    FERRET_DEVTOOLS: "1",
    FERRET_SIZES_WITHIN: sizesWithin,
    FERRET_SSH_DIR: ssh.dir,
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT}`,
    WEBVIEW2_USER_DATA_FOLDER: webviewData,
  },
  stdio: "ignore",
});
const stop = () => {
  try { execSync(`taskkill /PID ${app.pid} /T /F`, { stdio: "ignore" }); } catch {}
  // The SSH server ends when its input closes.
  try { sshd.stdin.end(); sshd.kill(); } catch {}
};

let failed = 0, warned = 0;
try {
  const c = await connect(PORT);
  // Every test starts from a running app showing the fixture (so --only works too).
  await c.waitFor(`window.__ferretTest?.state.ready() && window.__ferretTest.state.activeTab()?.p === ${JSON.stringify(root)}`, { timeoutMs: 30_000, what: "Ferret to start on the fixture folder" });
  const t = {
    c, root, needle, wslDir, ssh,
    p: (...parts) => join(root, ...parts),
    notes: [],
    note(s) { this.notes.push(s); },
  };
  for (const test of tests) {
    if (only && !test.name.includes(only)) continue;
    t.notes = [];
    const started = Date.now();
    try {
      await Promise.race([test.run(t), sleep(60_000).then(() => { throw new Error("took over a minute"); })]);
      console.log(`${green("✓")} ${test.name} ${dim(`${Date.now() - started} ms${t.notes.length ? " · " + t.notes.join(", ") : ""}`)}`);
    } catch (e) {
      const shot = join(artifacts, `${test.name.replace(/[^\w]+/g, "-").slice(0, 60)}.png`);
      await c.screenshot(shot).catch(() => {});
      if (test.soft) { warned++; console.log(`${yellow("!")} ${test.name}: ${e.message} ${dim("(warning only)")}`); }
      else { failed++; console.log(`${red("✗")} ${test.name}: ${e.message}`); }
      console.log(dim(`  screenshot: ${shot}`));
      // Leave no dialog or menu behind for the next test.
      await c.key("Escape").catch(() => {});
    }
  }
  c.close();
} catch (e) {
  failed++;
  console.log(red(`couldn't run the tests: ${e.message}`));
} finally {
  const log = join(process.env.LOCALAPPDATA, IDENT, "logs", "ferret.log");
  if ((failed || warned) && existsSync(log)) copyFileSync(log, join(artifacts, "ferret.log"));
  if (!flag("--keep")) {
    stop();
    await sleep(800);
    rmSync(root, { recursive: true, force: true });
    rmSync(sshRoot, { recursive: true, force: true });
    appData.forEach((d) => rmSync(d, { recursive: true, force: true }));
  } else {
    console.log(dim(`kept: ${root} (the test window is still running, pid ${app.pid})`));
  }
}
console.log(failed ? red(`${failed} failed`) + (warned ? `, ${warned} warning(s)` : "") : green("all passed") + (warned ? yellow(` (${warned} warning(s))`) : ""));
if (failed || warned) console.log(dim(`details: ${artifacts}`));
process.exit(failed ? 1 : 0);
