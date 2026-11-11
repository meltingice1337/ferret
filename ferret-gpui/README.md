# ferret-gpui: Ferret

Ferret drawn with [GPUI](https://gpui.rs), Zed's Rust UI framework. It works on `ferret-core`
directly: no WebView2, no IPC, no JSON. It started as a prototype to judge the look, the speed and
the hard parts; it's now the app that's released (`scripts/release.sh`). The Tauri app it replaces
(`src`, `src-tauri`) stays as an archive, for comparison.

```bash
cargo run --release   # a development copy: its own settings (%TEMP%\ferret-gpui), DEV badge and icon
cargo test
```

It has its own cargo workspace, so it never builds as part of `src-tauri`.

## Releases

- `cargo build --release` makes `ferret-gpui.exe` (installed as `ferret.exe`) and
  `ferret-indexer.exe` (the search service, `src/bin/`), with their icons and version details
  (`build.rs`; `FERRET_RELEASE=1` gives a release build Ferret's icon instead of the DEV one).
- `installer/ferret.nsi` is the installer (NSIS): the Tauri installer's folder, registry entries and
  updater arguments, so an installed Tauri copy updates into this app with its settings. `/DQA`
  builds a "Ferret QA" variant for trying it next to an installed Ferret.
- `src/update.rs` and `src/updateview.rs` are the updater: the same `latest.json` feed and release
  key as the Tauri app's, checked shortly after start and every few hours by an installed copy;
  "What's new", then the installer runs in passive mode and Ferret starts again.
- `scripts/build-ocr-pack.sh` builds the text recognition add-on (signed, from pinned sources),
  published on a release of its own (`ocr-pack-1`); `src/addons.rs` downloads it when asked
  (Settings → Add-ons). `FERRET_OCR_PACK_URL` points it elsewhere (tests).
- `src/appdata.rs`: installed, Ferret keeps its settings where the Tauri app did
  (%APPDATA%\com.ferret.app) and logs to %LOCALAPPDATA%\com.ferret.app\logs; `FERRET_DATA_DIR`
  points it elsewhere (tests).

## What it has

Nearly all of the app: tabs and split view, the list and grid with sorting, grouping and folder
sizes, file operations with undo, context menus with other apps' entries, previews of every kind
the app shows (and a few more), Quick Look, archives, Ctrl+K search and commands, the sidebar with
servers and network folders, the video and picture compressors, settings, the intro tour, and
keyboard areas (F6), and more besides: text in scanned PDFs (OCR), password-protected PDFs, Ctrl+F
in previews.

The feature-by-feature comparison with the Tauri app, the side-by-side review, the benchmark and the
probe programs behind them are working notes, kept outside the repository (`research/` at the repo
root, git-ignored).

## Video without CPU copies

GPUI's Windows renderer has no external surfaces (its `draw_surfaces` is empty; only macOS gets
them). So `vendor/gpui-pre-windows` is a copy of GPUI's Windows backend with a small patch (marked
"Ferret patch", wired in with `[patch.crates-io]`). The patch:
- creates GPUI's Direct3D 11 device with video support and makes it safe across threads,
- hands that device to the app (`gpui_windows::ferret::shared_device`),
- copies a texture over an image's atlas tile on the GPU (`gpui_windows::ferret::copy_to_image`),
- wakes a window at a display refresh only while GPUI wants a frame for it (`FrameDemand` in
  `vsync.rs`). Upstream wakes every window at every refresh, which kept an idle window at about 2 %
  of a core; now an idle window costs nothing. `FERRET_GPUI_VSYNC_ALWAYS` restores the upstream
  behaviour.

With it, Media Foundation's media engine decodes in hardware on GPUI's own device. Each new frame
is scaled by the GPU's video processor and copied GPU-to-GPU into the image GPUI draws, with no
pixel through the CPU.

While a video plays, the frame check runs at every display refresh, but the window only redraws
when a frame arrived. It stops when the video is paused or has ended.
Updating GPUI means applying the patch to the new snapshot (or getting it upstream).

## Environment variables

- `FERRET_GPUI_OPEN`: the folder to start in (default: your home folder).
- `FERRET_GPUI_SELECT`: an item to select once that folder is listed.
- `FERRET_GPUI_BOUNDS`: window position and size, `x,y,w,h` in logical pixels.
- `FERRET_GPUI_MUTE`: videos play without sound. `FERRET_GPUI_AUTOPLAY`: a selected video plays.
- `FERRET_GPUI_PALETTE`: start with Ctrl+K open on this search. `FERRET_GPUI_SCOPE`: search only
  under this folder.
- `FERRET_GPUI_EDITOR`: open the compress editor for the selected video.
  `FERRET_GPUI_EDITOR_PRESET` picks a preset (0–3). `FERRET_GPUI_EDITOR_COMPRESS` compresses as
  soon as the sample is in.
- `FERRET_GPUI_BACKGROUND`: the window opens without taking the keyboard or the foreground (tests).
  `FERRET_GPUI_DISPLAY`: `left`, `right` or `primary` monitor; `FERRET_GPUI_BOUNDS` then counts
  from its corner.
- `FERRET_GPUI_SCRIPT`: a test script run through GPUI's own input (`script.rs`: keys, typing,
  clicks, drags, the wheel, `state` as JSON, `shot`); `FERRET_GPUI_SCRIPT_OUT` gets its output.
- `FERRET_GPUI_NO_INTRO`: no intro tour on first start.
- `FERRET_GPUI_BENCH_OUT`: turns on the benchmark hooks and names their output file (`bench.rs`).
  - `FERRET_GPUI_BENCH_DIR`: the folder to open a second after the first frame and scroll.
  - `FERRET_GPUI_VIDEO_SECS`: count the video frames that reach the screen over that many seconds
    (or until the video ends, which writes `video_ended`: a preview plays once, it doesn't loop).
  - `FERRET_GPUI_VIDEO_TRACE`: with `FERRET_GPUI_VIDEO_SECS`, write every frame check to this file
    (ms since the first frame, ms the check took, the frame's time stamp, whether it was new).
- `FERRET_GPUI_VSYNC_ALWAYS`: wake the window at every display refresh, as upstream GPUI does.

`cargo run --release --example list_timing -- <folder>` times `ferret-core`'s listing on its own,
outside any UI.

The prototype's own settings database (`ferret-core` wants one for jobs) lives in
`%TEMP%\ferret-gpui`, never the app's.

## GPUI notes

- It uses `gpui-pre` and `gpui-pre-platform`, weekly snapshots of Zed's GPUI on crates.io, pinned
  exactly. The official `gpui` crate hasn't been released since 0.2.2 (October 2025).
- SVG icons don't inherit the text color: every icon names its color, and hover colors use
  `group_hover`.
- List rows need `w_full()`, or they only take their content's width.
- An unfocused window draws at about 30 fps by default (`inactive_frame_interval`).
- There's no radial gradient, so the accent glow is drawn once into a small PNG and stretched.
- One text can't mix font sizes, so a Word paragraph takes the size most of its text has.
- Windows' `DoDragDrop` runs its own message loop. The drag starts from a GPUI task, outside event
  handling, and GPUI keeps working through it.
- Text fixes in the vendored Windows backend (`direct_write.rs`, marked "Ferret"):
  - a font's glyph box was subtracted in `i16` and wrapped negative for Segoe UI Emoji, so emoji
    were never painted in clipped text;
  - glyphs with COLR v1 colour (most Windows 11 emoji) are drawn in colour through Direct2D's
    `DrawGlyphRunWithColorSupport` onto a WIC bitmap, since GPUI only draws v0 layers;
  - right-to-left runs (Hebrew, Arabic) are placed in visual order where DirectWrite puts them,
    instead of backwards.
- GPUI sets the window's dark mode from Windows' theme; the app sets it from its own, so Mica
  follows Ferret's light theme.
- `ListState` only knows the heights of items it has drawn, so scrolling far (End in a big grid)
  goes by the pane's own item positions.
- GPUI can't rotate text. Slide shapes that are turned, flipped or set in vertical text are
  drawn with Direct2D and DirectWrite into a picture first (`slidedraw.rs`).
- Accessibility: GPUI builds a UI Automation tree through AccessKit from elements that have an
  id and a `role`. It rebuilds the tree each frame, and a cached view reused as is leaves its
  nodes out, so the sidebar and file lists are drawn uncached while a screen reader listens
  (`embed` in `view.rs`). The item under the keyboard is the list's active descendant; Ctrl+K's
  focus stays in its box, so its current result is read out as a live region.
