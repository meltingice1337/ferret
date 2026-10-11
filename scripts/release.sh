#!/usr/bin/env bash
# Builds a signed Ferret release and publishes it on GitHub, where installed copies
# find it through the updater feed (releases/latest/download/latest.json).
# Run from Git Bash:
#
#   scripts/release.sh                     interactive: pick patch / minor / major, review, confirm
#   scripts/release.sh --bump patch --yes  no questions
#   scripts/release.sh --version 1.0.0     exact version
#   options: --notes "text" (replaces the commit list), --draft, --dry-run (show the plan, change nothing),
#            --skip-checks (emergencies only)
#
# Before anything else it type-checks the frontend and runs the Rust tests; a failure stops it.
# Release notes are the commit subjects since the previous tag, plus a compare link.
# Needs: a clean working tree, `gh` logged in, and the updater signing key at
# ~/.tauri/ferret.key (or $TAURI_SIGNING_PRIVATE_KEY).
set -euo pipefail

repo="meltingice1337/ferret"
cd "$(dirname "$0")/.."

bump="" version="" notes="" draft=0 yes=0 dry=0 skip=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-checks) skip=1; shift ;;
    --bump) bump="${2:-}"; shift 2 ;;
    --version) version="${2:-}"; shift 2 ;;
    --notes) notes="${2:-}"; shift 2 ;;
    --draft) draft=1; shift ;;
    -y|--yes) yes=1; shift ;;
    --dry-run) dry=1; shift ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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

# ---- checks: nothing else happens unless these pass ----
if [[ $skip -eq 1 ]]; then
  echo "${dim}Skipping checks (--skip-checks).${reset}"
else
  step "Type-checking the frontend (tsc)"
  npx tsc --noEmit -p . || fail "type check failed (fix it, or use --skip-checks)"
  step "Running the Rust tests (cargo test)"
  (cd src-tauri && cargo test --release --quiet) || fail "tests failed (fix them, or use --skip-checks)"
  echo "${green}Checks passed.${reset}"
fi

# ---- pick the version ----
current=$(node -p 'require("./src-tauri/tauri.conf.json").version')
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

if [[ -n "$prev" ]]; then
  compare="https://github.com/$repo/compare/$prev...$tag"
else
  compare="https://github.com/$repo/commits/$tag"
fi
[[ -n "$notes" ]] || notes=${commits:-"- No changes listed"}
notes+=$'\n\n'"**Full changelog:** $compare"

echo
echo "${green}Releasing Ferret $current -> $version ($tag)$([[ $draft -eq 1 ]] && echo ' as a draft')${reset}"
echo "${dim}Notes:${reset}"
echo "$notes"
echo
if [[ $dry -eq 1 ]]; then echo "Dry run: nothing was changed."; exit 0; fi
if [[ $yes -eq 0 ]]; then
  read -r -p "Build, tag, push and publish? [Y/n]: " ok </dev/tty || ok=""
  case "${ok,,}" in ""|y|yes) ;; *) echo "Cancelled."; exit 0 ;; esac
fi

if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  key="$HOME/.tauri/ferret.key"
  [[ -f "$key" ]] || fail "signing key not found at $key"
  export TAURI_SIGNING_PRIVATE_KEY="$key"
fi
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

# ---- version bump (tauri.conf.json, Cargo.toml, package.json) ----
step "Setting version $version"
bumped=(src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock package.json package-lock.json)
revert() { git checkout -- "${bumped[@]}" 2>/dev/null || true; }
VERSION="$version" node -e '
  const fs = require("fs");
  for (const f of ["src-tauri/tauri.conf.json", "package.json"]) {
    const s = fs.readFileSync(f, "utf8");
    if (!/"version":\s*"[^"]+"/.test(s)) { console.error("no version in " + f); process.exit(1); }
    fs.writeFileSync(f, s.replace(/"version":\s*"[^"]+"/, `"version": "${process.env.VERSION}"`));
  }'
sed -i "0,/^version = \"[^\"]*\"/s//version = \"$version\"/" src-tauri/Cargo.toml
npm version "$version" --no-git-tag-version --allow-same-version >/dev/null

# ---- build + sign ----
step "Building (npm run tauri build)"
if ! npm run tauri build; then
  revert
  fail "build failed (version bump reverted)"
fi

bundle="src-tauri/target/release/bundle/nsis"
setup=$(ls "$bundle"/*_"${version}"_x64-setup.exe 2>/dev/null | head -n1 || true)
[[ -n "$setup" ]] || { revert; fail "installer for $version not found in $bundle"; }
[[ -f "$setup.sig" ]] || { revert; fail "signature $setup.sig is missing (is createUpdaterArtifacts on?)"; }

# ---- updater feed ----
asset=$(basename "$setup" | tr ' ' '.')  # GitHub stores spaces in asset names as dots
latest="$bundle/latest.json"
VERSION="$version" NOTES="$notes" SIG="$setup.sig" URL="https://github.com/$repo/releases/download/$tag/$asset" OUT="$latest" node -e '
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
