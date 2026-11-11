#!/usr/bin/env bash
# Builds a signed Ferret release (the GPUI app, ferret-gpui) and publishes it on GitHub, where
# installed copies find it through the updater feed (releases/latest/download/latest.json).
# Copies of the Tauri app (archived in src-tauri and src) take the same feed, so they update
# into this one. Run from Git Bash:
#
#   scripts/release.sh                     asks patch / minor / major up front, then runs unattended
#   scripts/release.sh --bump patch        no questions
#   scripts/release.sh --version 1.0.0     exact version
#   options: --notes "text" (replaces the commit list), --draft, --dry-run (show the plan, change nothing),
#            --skip-smoke (skip starting the built app once), --skip-checks (emergencies only)
#
# Once the version is picked it refuses to go on with any compiler warning (even with
# --skip-checks), runs the Rust tests (the app's and ferret-core's), builds the app
# and the search service, starts the built app once in the background (a smoke test), then makes
# the installer (NSIS, a 64-bit program: scripts/make-installer.sh) and signs it and the search
# service with the updater key. A failure stops it. The build's exes and symbols (PDBs) are kept in
# ferret-gpui/target/symbols/<version>, to read addresses logged by installed copies.
# Release notes are the commit subjects since the previous tag, plus a compare link. The updater
# feed (latest.json) also lists every previous release's notes, for copies that skipped versions.
# Needs: a clean working tree, `gh` logged in, NSIS 3.11 (Tauri's copy in %LOCALAPPDATA%\tauri\NSIS,
# or makensis on PATH; its 64-bit stubs are in ferret-gpui/installer/nsis-amd64), node with the
# repo's packages (the signer is @tauri-apps/cli's), and the updater signing key at
# ~/.tauri/ferret.key (or $TAURI_SIGNING_PRIVATE_KEY).
set -euo pipefail

repo="meltingice1337/ferret"
cd "$(dirname "$0")/.."
root=$PWD
app=ferret-gpui

bump="" version="" notes="" draft=0 dry=0 skip=0 skip_smoke=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-checks) skip=1; shift ;;
    --skip-smoke|--skip-e2e) skip_smoke=1; shift ;;
    --bump) bump="${2:-}"; shift 2 ;;
    --version) version="${2:-}"; shift 2 ;;
    --notes) notes="${2:-}"; shift 2 ;;
    --draft) draft=1; shift ;;
    -y|--yes) shift ;;  # no-op, kept so old invocations still work
    --dry-run) dry=1; shift ;;
    -h|--help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 1 ;;
  esac
done

if [[ -t 1 ]]; then
  bold=$'\e[1m' dim=$'\e[2m' red=$'\e[31m' green=$'\e[32m' cyan=$'\e[36m' reset=$'\e[0m'
else
  bold="" dim="" red="" green="" cyan="" reset=""
fi
fail() { echo "${red}error:${reset} $*" >&2; exit 1; }
step() { echo "${cyan}==>${reset} $*"; }

[[ -z "$bump" || "$bump" =~ ^(patch|minor|major)$ ]] || fail "--bump must be patch, minor or major"
[[ -z "$version" || "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "--version must look like 1.2.3"
if [[ $dry -eq 0 && -n "$(git status --porcelain)" ]]; then fail "the working tree has uncommitted changes"; fi

# NSIS: Tauri's copy (it downloads one for its own builds), or one on PATH, or the usual install,
# the version of the 64-bit stubs in ferret-gpui/installer/nsis-amd64 (make-installer.sh says).
scripts/make-installer.sh --check || fail "the installer can't be made (see above)"
signer="$root/node_modules/@tauri-apps/cli/tauri.js"
[[ -f "$signer" ]] || fail "the signer isn't installed (run yarn): $signer"

# ---- pick the version (the only question; everything after runs unattended) ----
current=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$app/Cargo.toml" | head -n1)
[[ "$current" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || fail "can't read the current version ($current)"
maj=${BASH_REMATCH[1]} min=${BASH_REMATCH[2]} pat=${BASH_REMATCH[3]}
next_patch="$maj.$min.$((pat + 1))"
next_minor="$maj.$((min + 1)).0"
next_major="$((maj + 1)).0.0"

prev=$(git tag --list 'v*' --sort=-v:refname | head -n1)
commits=$(git log ${prev:+"$prev..HEAD"} --pretty='- %s' --no-merges)
count=$(grep -c . <<<"$commits" || true)

if [[ -z "$version" ]]; then
  if [[ -z "$bump" ]]; then
    echo
    if [[ -n "$prev" ]]; then
      echo "${bold}Ferret $current${reset}  ${dim}(last release $prev, $count commits since)${reset}"
    else
      echo "${bold}Ferret $current${reset}  ${dim}(first release)${reset}"
    fi
    echo
    echo "  1) patch  $next_patch   fixes only"
    echo "  2) minor  $next_minor   new features  ${dim}[default]${reset}"
    echo "  3) major  $next_major   big or breaking changes"
    echo
    read -r -p "Release type [2]: " choice </dev/tty || choice=""
    case "${choice,,}" in
      ""|2|minor) bump=minor ;;
      1|patch) bump=patch ;;
      3|major) bump=major ;;
      *) fail "unknown choice '$choice'" ;;
    esac
  fi
  case "$bump" in
    patch) version=$next_patch ;;
    minor) version=$next_minor ;;
    major) version=$next_major ;;
  esac
fi
tag="v$version"
[[ -z "$(git tag --list "$tag")" ]] || fail "tag $tag already exists"
# The folder-size helper (WSL and SSH servers) is built for x86_64 and ARM64 Linux with the
# app (ferret-core's build.rs).
for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  rustup target list --installed | grep -qx "$t" ||
    fail "Rust's Linux target is missing: rustup target add $t"
done

if [[ -n "$prev" ]]; then
  compare="https://github.com/$repo/compare/$prev...$tag"
else
  compare="https://github.com/$repo/commits/$tag"
fi
[[ -n "$notes" ]] || notes=${commits:-"- No changes listed"}
# The updater feed also carries every previous release's notes, each under "## vX.Y.Z", so a copy
# however far behind can show everything it's about to get (it keeps the ones newer than itself).
# Commit subjects only: a few KB even after many releases.
feed_notes="## $tag"$'\n'"$notes"
mapfile -t older < <(git tag --list 'v*' --sort=-v:refname)
for i in "${!older[@]}"; do
  t=${older[$i]} before=${older[$((i + 1))]:-}
  [[ -n "$before" ]] || break
  feed_notes+=$'\n\n'"## $t"$'\n'"$(git log "$before..$t" --pretty='- %s' --no-merges)"
done
notes+=$'\n\n'"**Full changelog:** $compare"
feed_notes+=$'\n\n'"**Full changelog:** $compare"

echo
echo "${green}Releasing Ferret $current -> $version ($tag)$([[ $draft -eq 1 ]] && echo ' as a draft')${reset}"
echo "${dim}Notes:${reset}"
echo "$notes"
echo
if [[ $dry -eq 1 ]]; then echo "Dry run: nothing was changed."; exit 0; fi

# ---- checks: nothing else happens unless these pass ----
# No release with a compiler warning, in the app or in ferret-core (its tests included). Not
# skipped by --skip-checks: an unused function or field is fixed (or removed) before it ships.
# `cargo check --all-targets` sees both builds' code (with and without the tests); dependencies'
# own warnings are hidden by cargo, so what shows is ours.
no_warnings() {
  local dir=$1 out
  shift
  out=$(cd "$dir" && cargo check "$@" --all-targets --quiet --message-format short 2>&1) || { echo "$out" >&2; fail "cargo check failed in $dir"; }
  if grep -qE '(^|: )warning' <<<"$out"; then
    echo "$out" >&2
    fail "compiler warnings in $dir: fix them first (a release never builds with warnings)"
  fi
}
step "Checking for compiler warnings (cargo check)"
no_warnings "$app"
no_warnings src-tauri -p ferret-core
if [[ $skip -eq 1 ]]; then
  echo "${dim}Skipping checks (--skip-checks).${reset}"
else
  step "Running the app's tests (cargo test)"
  (cd "$app" && cargo test --quiet) || fail "tests failed (fix them, or use --skip-checks)"
  step "Running ferret-core's tests"
  # A debug build of ferret-core alone (not the archived Tauri shell around it).
  (cd src-tauri && cargo test -p ferret-core --quiet) || fail "ferret-core's tests failed (fix them, or use --skip-checks)"
  echo "${green}Checks passed.${reset}"
fi

if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  key="$HOME/.tauri/ferret.key"
  [[ -f "$key" ]] || fail "signing key not found at $key"
  export TAURI_SIGNING_PRIVATE_KEY="$key"
fi
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
# `tauri signer sign <file>`: writes <file>.sig (the key comes as a file or as its contents).
sign() {
  local args=(signer sign)
  if [[ -f "$TAURI_SIGNING_PRIVATE_KEY" ]]; then
    args+=(--private-key-path "$TAURI_SIGNING_PRIVATE_KEY")
    env -u TAURI_SIGNING_PRIVATE_KEY node "$signer" "${args[@]}" "$1" >/dev/null
  else
    node "$signer" "${args[@]}" "$1" >/dev/null
  fi
  [[ -s "$1.sig" ]] || fail "signing $1 failed"
}

# ---- version bump: the app, and ferret-core's workspace (the search service compares it) ----
step "Setting version $version"
bumped=("$app/Cargo.toml" "$app/Cargo.lock" src-tauri/Cargo.toml src-tauri/Cargo.lock)
revert() { git checkout -- "${bumped[@]}" 2>/dev/null || true; }
sed -i "0,/^version = \"[^\"]*\"/s//version = \"$version\"/" "$app/Cargo.toml"
sed -i "0,/^version = \"[^\"]*\"/s//version = \"$version\"/" src-tauri/Cargo.toml
(cd src-tauri && cargo update --workspace --offline --quiet) || { revert; fail "couldn't update src-tauri/Cargo.lock"; }

# ---- build ----
step "Building ferret.exe and ferret-indexer.exe (cargo build --release)"
# FERRET_RELEASE=1: the exe gets Ferret's icon (other builds show the development one).
if ! (cd "$app" && FERRET_RELEASE=1 cargo build --release); then
  revert
  fail "build failed (version bump reverted)"
fi
built="$app/target/release"
said=$(powershell.exe -NoProfile -Command "(Get-Item '$(cygpath -w "$built/ferret-gpui.exe")').VersionInfo.ProductVersion" | tr -d '\r')
[[ "$said" == "$version" ]] || { revert; fail "the built exe says it's ${said:-no version}, not $version"; }
# This build's symbols, with its exes under their installed names: addresses logged by installed
# copies (the UI-stall watchdog's ferret.exe+0x…) are read with them (scripts/symbolize-log.ps1).
# target/ is git-ignored: a local archive.
symbols="$app/target/symbols/$version"
rm -rf "$symbols" && mkdir -p "$symbols"
{ cp "$built/ferret-gpui.exe" "$symbols/ferret.exe" &&
  cp "$built/ferret_gpui.pdb" "$built/ferret-indexer.exe" "$built/ferret_indexer.pdb" "$symbols/"; } ||
  { revert; fail "the build's exes and symbols (PDBs) couldn't be kept in $symbols"; }
echo "${dim}Symbols kept in $symbols${reset}"

# ---- smoke test: the built app starts, lists a folder and quits (a background window) ----
if [[ $skip_smoke -eq 1 ]]; then
  echo "${dim}Skipping the smoke test (--skip-smoke).${reset}"
else
  step "Starting the built app once (smoke test)"
  smoke=$(mktemp -d)
  printf 'wait 2500\nstate smoke\nquit\n' >"$smoke/script.txt"
  if ! FERRET_DATA_DIR="$(cygpath -w "$smoke/data")" FERRET_GPUI_BACKGROUND=1 FERRET_GPUI_MUTE=1 FERRET_GPUI_NO_INTRO=1 \
      FERRET_GPUI_OPEN="$(cygpath -w "$root")" FERRET_GPUI_SCRIPT="$(cygpath -w "$smoke/script.txt")" \
      FERRET_GPUI_SCRIPT_OUT="$(cygpath -w "$smoke/out.txt")" timeout 60 "$built/ferret-gpui.exe"; then
    revert
    fail "the built app didn't start and quit cleanly (smoke test; --skip-smoke to skip)"
  fi
  grep -q '"items":[1-9]' "$smoke/out.txt" 2>/dev/null || { revert; fail "the built app didn't list a folder (smoke test: $smoke/out.txt)"; }
  rm -rf "$smoke"
fi

# ---- package + sign ----
out="$app/target/installer"
stage="$out/stage-$version"
rm -rf "$stage" && mkdir -p "$stage"
cp "$built/ferret-gpui.exe" "$built/ferret-indexer.exe" "$stage/"
step "Signing the search service"
sign "$stage/ferret-indexer.exe"
step "Making the installer (NSIS)"
setup="$out/Ferret_${version}_x64-setup.exe"
rm -f "$setup" "$setup.sig"
scripts/make-installer.sh "$version" "$root/$stage" "$root/$setup" || { revert; fail "makensis failed"; }
step "Signing the installer"
sign "$setup"

# ---- updater feed ----
asset=$(basename "$setup" | tr ' ' '.')  # GitHub stores spaces in asset names as dots
latest="$out/latest.json"
VERSION="$version" NOTES="$feed_notes" SIG="$setup.sig" URL="https://github.com/$repo/releases/download/$tag/$asset" OUT="$latest" node -e '
  const fs = require("fs"), e = process.env;
  const feed = {
    version: e.VERSION,
    notes: e.NOTES,
    pub_date: new Date().toISOString().replace(/\.\d+Z$/, "Z"),
    platforms: { "windows-x86_64": { signature: fs.readFileSync(e.SIG, "utf8").trim(), url: e.URL } },
  };
  fs.writeFileSync(e.OUT, JSON.stringify(feed, null, 2));'

# ---- commit, tag, publish ----
step "Committing and tagging $tag"
git add "${bumped[@]}"
git commit -m "release: $tag"
git tag -a "$tag" -m "Ferret $version"  # annotated: works with tag.gpgsign, no editor
git push origin HEAD "$tag"

step "Creating GitHub release $tag"
gh_args=(release create "$tag" "$setup" "$latest" -R "$repo" --title "Ferret $version" --notes "$notes")
[[ $draft -eq 1 ]] && gh_args+=(--draft)
gh "${gh_args[@]}"

echo
echo "${green}Released Ferret $version${reset}"
echo "  installer: $setup"
echo "  feed:      https://github.com/$repo/releases/latest/download/latest.json"
