# NSIS's 64-bit stubs and plug-ins

Ferret's installer (`../ferret.nsi`) and its uninstaller are 64-bit programs (`Target
amd64-unicode`), as Ferret is. NSIS's releases only ship 32-bit (x86) stubs and plug-ins, so the
64-bit ones are here, built from NSIS's own source:

- `Stubs/`: the installer stubs (`zlib`, `bzip2`, `lzma`, each plain and solid). Ferret's installer
  uses `lzma_solid`; makensis loads `zlib` first when the target changes.
- `Plugins/amd64-unicode/`: `nsExec` (ferret.nsi), `nsDialogs` and `System` (Modern UI 2's pages).
- `SHA256SUMS`: the NSIS version and tag they come from, and each file's hash.
  `scripts/make-installer.sh` checks them before every build.
- `COPYING`: NSIS's licence (zlib/libpng; bzip2's licence for the bzip2 stubs).

They must match makensis's version (NSIS 3.11 now, Tauri's copy in `%LOCALAPPDATA%\tauri\NSIS`):
`scripts/make-installer.sh` refuses a different one.

## How they're used

`scripts/release.sh` makes the installer with `scripts/make-installer.sh`. makensis takes stubs and
plug-ins from its own NSIS folder, which an environment variable (`NSISDIR`) can name. So
make-installer.sh copies NSIS's `Include`, `Contrib`, `Stubs` and `Plugins` into a temporary
folder, adds these, and runs makensis with `NSISDIR` pointing there. Nothing in the NSIS install
changes. It then checks that the setup is a 64-bit program.

To try an installer: `scripts/make-installer.sh 1.2.3 <folder with the exes and .sig> <setup.exe> --qa`
(the "Ferret QA" build, see ferret.nsi).

## Rebuilding them

`scripts/build-nsis-amd64.sh` (Git Bash) downloads NSIS's source (the release tag on
github.com/nsis-dev/nsis, the official mirror of NSIS's SourceForge repository, checked against
its SHA-256 and its commit), builds it with SCons and Visual Studio's x64 C++ tools, and puts the
results here. Needs Visual Studio's "Desktop development with C++" (MSVC x64 and a Windows SDK),
and Python with SCons: `uv`, or `PYTHON=<python.exe that can import SCons>`. The build is
reproducible: the same source gives the same bytes (it uses 3.11's own `SOURCE_DATE_EPOCH`).

Two things in it aren't NSIS's own build as is:
- NSIS's SConstruct configures makensis (not built here) every time, and makensis wants zlib: an
  empty zlib (a header and an import library) gets past that check.
- SCons's `masm` tool always runs `ml`, MASM's x86 assembler. The System plug-in's x64 call code
  (`Call-amd64.S`) needs `ml64`, so a tool ahead of SCons's (`--site-dir`) runs ml64 for x64.
  Without it, System.dll doesn't link.

For a new NSIS version: change the version, tag, commit and SHA-256 at the top of the script,
update Tauri's NSIS (or makensis on PATH) to the same version, and run it.
