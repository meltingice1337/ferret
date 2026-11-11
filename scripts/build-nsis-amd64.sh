#!/usr/bin/env bash
# Builds NSIS's 64-bit installer stubs (amd64-unicode) and the plug-ins Ferret's installer uses,
# from NSIS's own source, into ferret-gpui/installer/nsis-amd64 (checked in: scripts/release.sh
# makes the installer with them). NSIS's releases ship 32-bit (x86) stubs and plug-ins only.
# Run from Git Bash when NSIS (the makensis release.sh finds) moves to a new version:
#
#   scripts/build-nsis-amd64.sh
#
# Needs: Visual Studio's C++ build tools for x64 (MSVC and a Windows SDK; SCons finds them), curl,
# and Python 3 with SCons: uv (it puts both in a throwaway environment), or $PYTHON naming a
# Python that can import SCons. Nothing is installed; the work happens in a temporary folder.
set -euo pipefail

# NSIS 3.11, the version of the makensis release.sh uses (it checks they match). The source is
# the v311 tag ("Tagging for release 3.11") of github.com/nsis-dev/nsis, the official mirror of
# NSIS's SourceForge repository (github.com/kichik/nsis redirects there).
nsis_version=3.11
nsis_tag=v311
nsis_commit=7359413009afd4f0fff472d841fc2f2cc0e0a5f8
nsis_sha256=6ba0e34576f95282e4c7ab4347c016eaaa929fb5f20c90c6a28fb84ca1e2724e
nsis_url="https://github.com/nsis-dev/nsis/archive/refs/tags/$nsis_tag.tar.gz"
# 3.11's own build time (makensis -HDRINFO shows it), so the same source gives the same bytes.
source_date_epoch=1741475120
scons_version=4.9.1
# The plug-ins ferret.nsi uses: nsExec itself; nsDialogs and System through Modern UI 2's pages.
plugins=(nsExec nsDialogs System)
# All the stubs: makensis loads zlib's when the target changes, then the compressor's.
stubs=(zlib zlib_solid bzip2 bzip2_solid lzma lzma_solid)

cd "$(dirname "$0")/.."
root=$PWD
dest="$root/ferret-gpui/installer/nsis-amd64"

fail() { echo "error: $*" >&2; exit 1; }
step() { echo "==> $*"; }

# ---- Python with SCons ----
if [[ -n "${PYTHON:-}" ]]; then
  python=("$PYTHON" -I)
elif command -v uv >/dev/null 2>&1; then
  python=(uv run --no-project --quiet --with "scons==$scons_version" python -I)
else
  fail "no Python with SCons: install uv, or set PYTHON to a python.exe that can import SCons"
fi
"${python[@]}" -c "import SCons" 2>/dev/null || fail "SCons can't be imported by: ${python[*]}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# ---- the source, checked ----
step "Downloading NSIS $nsis_version's source ($nsis_tag)"
mkdir -p "$work/download" "$work/src"
curl -fsSL -o "$work/download/nsis.tar.gz" "$nsis_url"
got=$(sha256sum "$work/download/nsis.tar.gz" | cut -d' ' -f1)
[[ "$got" == "$nsis_sha256" ]] || fail "the source archive's SHA-256 is $got, not $nsis_sha256"
# (git reads the archive's first block only: gzip may end on a broken pipe.)
got=$({ gzip -dc "$work/download/nsis.tar.gz" 2>/dev/null || true; } | git get-tar-commit-id)
[[ "$got" == "$nsis_commit" ]] || fail "the source archive is commit $got, not $nsis_tag's $nsis_commit"
tar -xzf "$work/download/nsis.tar.gz" -C "$work/src"
src="$work/src/nsis-${nsis_tag#v}"
[[ -f "$src/SConstruct" ]] || fail "no SConstruct in $src"

# ---- zlib for SCons's check ----
# NSIS's SConstruct always configures makensis, which links zlib, so it wants a zlib (ZLIB_W32)
# even though makensis isn't built here. The stubs carry their own decompressors (Source/zlib,
# bzip2, 7zip): a header and an empty import library get past the check.
vswhere="$(cygpath -u "$(printenv 'ProgramFiles(x86)' || echo 'C:\Program Files (x86)')")/Microsoft Visual Studio/Installer/vswhere.exe"
[[ -x "$vswhere" ]] || fail "Visual Studio isn't installed (no vswhere.exe)"
lib=$("$vswhere" -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 \
  -find 'VC/Tools/MSVC/**/bin/Hostx64/x64/lib.exe' | tr -d '\r' | head -n1)
[[ -n "$lib" ]] || fail "Visual Studio's x64 C++ build tools aren't installed"
mkdir -p "$work/zlib"
echo "/* placeholder: makensis isn't built here */" >"$work/zlib/zlib.h"
printf 'LIBRARY zlib1\nEXPORTS\n  zlibVersion\n' >"$work/zlib/z.def"
MSYS2_ARG_CONV_EXCL='*' "$lib" /nologo /machine:x64 /def:"$(cygpath -w "$work/zlib/z.def")" \
  /out:"$(cygpath -w "$work/zlib/zdll.lib")" >/dev/null

# ---- MASM for x64 ----
# The System plug-in has an assembly file (Call-amd64.S). SCons's masm tool always runs ml,
# MASM's x86 assembler, and without it the plug-in is built without its Call (an unresolved
# CallProc2). This tool, ahead of SCons's own (--site-dir), runs ml64 for an x64 build.
mkdir -p "$work/site/site_tools"
cat >"$work/site/site_tools/masm.py" <<'EOF'
import SCons.Tool.masm as masm

def generate(env):
    masm.generate(env)
    if env.get('TARGET_ARCH') in ('amd64', 'x86_64', 'x64', 'em64t'):
        env['AS'] = 'ml64'

def exists(env):
    return masm.exists(env) or env.Detect('ml64')
EOF

# ---- build ----
skip=()  # every other plug-in (ExDLL, the plug-in API they link, is built)
for p in AdvSplash Banner BgImage Dialer InstallOptions LangDLL Library/TypeLib Math nsDialogs nsExec \
  NSISdl Splash StartMenu System UserInfo VPatch/Source/Plugin; do
  [[ " ${plugins[*]} " == *" $p "* ]] || skip+=("$p")
done
step "Building the amd64-unicode stubs and ${plugins[*]} (SCons, MSVC)"
(
  cd "$src"
  "${python[@]}" -c "import sys; from SCons.Script.Main import main; sys.argv[0] = 'scons'; main()" \
    -Q "--site-dir=$(cygpath -w "$work/site")" TARGET_ARCH=amd64 UNICODE=yes \
    VER_MAJOR=${nsis_version%%.*} VER_MINOR=${nsis_version#*.} VER_REVISION=0 VER_BUILD=0 SOURCE_DATE_EPOCH=$source_date_epoch \
    SKIPUTILS=all SKIPMISC=all SKIPDOC=all "SKIPPLUGINS=$(IFS=,; echo "${skip[*]}")" \
    "ZLIB_W32=$(cygpath -w "$work/zlib")" "PREFIX=$(cygpath -w "$work/out")" \
    install-stubs install-plugins
)

# ---- into the repository ----
rm -rf "$dest/Stubs" "$dest/Plugins"
mkdir -p "$dest/Stubs" "$dest/Plugins/amd64-unicode"
for s in "${stubs[@]}"; do
  cp "$work/out/Stubs/$s-amd64-unicode" "$dest/Stubs/"
done
for p in "${plugins[@]}"; do
  cp "$work/out/Plugins/amd64-unicode/$p.dll" "$dest/Plugins/amd64-unicode/"
done
cp "$src/COPYING" "$dest/COPYING"
(
  cd "$dest"
  echo "# NSIS $nsis_version ($nsis_tag, $nsis_commit), built by scripts/build-nsis-amd64.sh"
  find Stubs Plugins -type f | sort | xargs sha256sum
) >"$dest/SHA256SUMS"
for f in "$dest"/Stubs/* "$dest"/Plugins/amd64-unicode/*; do
  file "$f" | grep -q 'PE32+' || fail "$f isn't a 64-bit program"
done
step "Done: $dest"
cat "$dest/SHA256SUMS"
