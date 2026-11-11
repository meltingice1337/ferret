#!/usr/bin/env bash
# Makes Ferret's installer (ferret-gpui/installer/ferret.nsi), a 64-bit program, with NSIS's
# makensis. scripts/release.sh runs it; run it yourself to try an installer. From Git Bash:
#
#   scripts/make-installer.sh VERSION SRC OUTFILE [--qa]   SRC: ferret-gpui.exe, ferret-indexer.exe
#                                                          and its .sig; --qa: the "Ferret QA" build
#   scripts/make-installer.sh --check                      is everything there? (changes nothing)
#
# NSIS is Tauri's copy (%LOCALAPPDATA%\tauri\NSIS, fetched by Tauri builds), makensis on PATH or
# the usual install, and must be the version of the 64-bit stubs and plug-ins NSIS doesn't ship,
# built from its source into ferret-gpui/installer/nsis-amd64 (scripts/build-nsis-amd64.sh).
# makensis takes those from its NSIS folder, so it's given one (NSISDIR): NSIS's own files with
# them added, in a temporary folder. Nothing in the NSIS install changes.
set -euo pipefail

cd "$(dirname "$0")/.."
root=$PWD
amd64="$root/ferret-gpui/installer/nsis-amd64"
script="$root/ferret-gpui/installer/ferret.nsi"

fail() { echo "error: $*" >&2; exit 1; }

check=0 qa=0 args=()
for a in "$@"; do
  case "$a" in
    --check) check=1 ;;
    --qa) qa=1 ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) fail "unknown option: $a (see --help)" ;;
    *) args+=("$a") ;;
  esac
done
[[ $check -eq 1 || ${#args[@]} -eq 3 ]] || fail "usage: scripts/make-installer.sh VERSION SRC OUTFILE [--qa] (see --help)"

# ---- makensis, and the NSIS folder it comes with ----
makensis=""
for m in "${LOCALAPPDATA:-}/tauri/NSIS/Bin/makensis.exe" "$(command -v makensis 2>/dev/null || true)" "/c/Program Files (x86)/NSIS/makensis.exe"; do
  [[ -n "$m" && -x "$m" ]] && { makensis=$m; break; }
done
[[ -n "$makensis" ]] || fail "NSIS (makensis) wasn't found: install NSIS, or let a Tauri build fetch it"
nsis=""
for d in "$(dirname "$makensis")" "$(dirname "$(dirname "$makensis")")"; do
  [[ -f "$d/Include/MUI2.nsh" && -d "$d/Contrib" && -d "$d/Stubs" ]] && { nsis=$d; break; }
done
[[ -n "$nsis" ]] || fail "NSIS's files (Include, Contrib, Stubs) aren't next to $makensis"

# ---- the 64-bit stubs and plug-ins: there, intact, and for this makensis ----
[[ -f "$amd64/SHA256SUMS" ]] || fail "$amd64 is missing (scripts/build-nsis-amd64.sh builds it)"
want=$(sed -n '1s/^# NSIS \([0-9][0-9.]*\) .*/\1/p' "$amd64/SHA256SUMS")
have=$("$makensis" -VERSION | tr -d '\r')
[[ "$have" == "v$want" ]] ||
  fail "makensis is NSIS $have, the 64-bit stubs are NSIS $want's: use NSIS $want, or rebuild them (scripts/build-nsis-amd64.sh)"
(cd "$amd64" && grep -v '^#' SHA256SUMS | sha256sum -c --quiet -) || fail "$amd64 doesn't match its SHA256SUMS"
[[ $check -eq 1 ]] && exit 0

version=${args[0]} src=${args[1]} outfile=${args[2]}
[[ -f "$src/ferret-gpui.exe" && -f "$src/ferret-indexer.exe" && -f "$src/ferret-indexer.exe.sig" ]] ||
  fail "$src must hold ferret-gpui.exe, ferret-indexer.exe and ferret-indexer.exe.sig"

# ---- NSIS's folder, with the 64-bit stubs and plug-ins ----
tree=$(mktemp -d)
trap 'rm -rf "$tree"' EXIT
cp -r "$nsis/Include" "$nsis/Contrib" "$nsis/Stubs" "$nsis/Plugins" "$tree/"
cp "$amd64"/Stubs/* "$tree/Stubs/"
mkdir -p "$tree/Plugins/amd64-unicode"
cp "$amd64"/Plugins/amd64-unicode/* "$tree/Plugins/amd64-unicode/"

defs=(-DVERSION="$version" -DSRC="$(cygpath -w "$src")" -DOUTFILE="$(cygpath -w "$outfile")")
[[ $qa -eq 1 ]] && defs+=(-DQA)
NSISDIR="$(cygpath -w "$tree")" "$makensis" -V2 "${defs[@]}" "$(cygpath -w "$script")"

# ---- a 64-bit program? (the PE header's machine: 0x8664) ----
pe=$(od -An -tu4 -j60 -N4 "$outfile" | tr -d ' ')
machine=$(od -An -tx2 -j$((pe + 4)) -N2 "$outfile" | tr -d ' ')
[[ "$machine" == "8664" ]] || fail "$outfile isn't a 64-bit program (machine $machine)"
