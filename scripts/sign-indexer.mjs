// Signs ferret-indexer.exe for a release, with the key the updater's signatures use
// (TAURI_SIGNING_PRIVATE_KEY and its password, which release.sh sets). `tauri build` runs it
// before bundling (tauri.conf.json build.beforeBundleCommand), and the installer carries the
// signature next to the exe (bundle.resources). The installed search service updates itself only
// to a copy signed so (src-tauri/src/search/signed.rs).
//
// Without a key (a local build) the signature stays empty: that build's indexer can be installed,
// but a service can't update itself to it.
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const target = process.env.CARGO_TARGET_DIR ?? join(root, "src-tauri", "target");
const exe = join(target, "release", "ferret-indexer.exe");
const out = join(root, "src-tauri", "target", "signed", "ferret-indexer.exe.sig");
mkdirSync(dirname(out), { recursive: true });

const key = process.env.TAURI_SIGNING_PRIVATE_KEY;
if (!key) {
  writeFileSync(out, "");
  console.log("sign-indexer: no signing key (TAURI_SIGNING_PRIVATE_KEY), so ferret-indexer.exe isn't signed");
  process.exit(0);
}
if (!existsSync(exe)) {
  console.error(`sign-indexer: ${exe} wasn't built`);
  process.exit(1);
}
// The key comes as a file (release.sh) or as its contents. The password stays in the environment.
const env = { ...process.env };
const args = [join(root, "node_modules", "@tauri-apps", "cli", "tauri.js"), "signer", "sign"];
if (existsSync(key)) {
  delete env.TAURI_SIGNING_PRIVATE_KEY;
  args.push("--private-key-path", key);
}
args.push(exe);
rmSync(`${exe}.sig`, { force: true });
writeFileSync(out, "");
execFileSync(process.execPath, args, { cwd: root, env, stdio: ["ignore", "ignore", "inherit"] });
renameSync(`${exe}.sig`, out);
console.log(`sign-indexer: signed ${exe}`);
