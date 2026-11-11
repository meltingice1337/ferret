<p align="center">
  <img src="public/ferret-256.png" width="128" alt="Ferret logo">
</p>

<h1 align="center">Ferret</h1>

<p align="center">
  <b>A fast, modern file manager for Windows.</b><br>
  Instant whole-disk search, rich previews and real Explorer integration, in a Mica window.
</p>

<p align="center">
  <img alt="Windows 10/11" src="https://img.shields.io/badge/Windows-10%20%7C%2011-0078D4?logo=windows&logoColor=white">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-all%20the%20way-B7410E?logo=rust&logoColor=white">
  <img alt="GPUI" src="https://img.shields.io/badge/GPUI-native%20UI-6E56CF">
</p>

<p align="center">
  <img src="docs/hero.png" alt="Ferret in split view: a project folder next to its srccomponents folder, with Panes.tsx previewed with syntax highlighting">
</p>

---

## Features

**⚡ Instant search — `Ctrl+K`**
Fuzzy search over every file on every drive, ranked by name match and how often you open things.
Filters: `ext:pdf,docx` · `type:folder` · `size:>100mb` · `modified:<7d` · `dm:2026-03-04` · `in:"C:\Program Files"` · `!node_modules\` · globs like `*.rs`.
Filters are suggested as you type (`Tab` completes them). Running WSL distros are searchable too: each is listed from inside Linux and refreshed every 15 minutes, and a stopped distro is never started just to be indexed.
Search inside files with `content:"some text"`: it reads the files in the open folder (or the one named with `in:`), and each result shows the line with the text. The other filters narrow which files are read; binary files and `node_modules` are skipped.
Recent searches come back when you open `Ctrl+K`, and any search can be saved to the sidebar (☆). `Ctrl+Shift+Enter` opens every match as a tab that lists them like a folder and stays current.

**👁 Previews — `Alt+P`, or `Space` for Quick Look**
Images, video, audio, PDF, Word (`.docx`), Markdown, CSV, fonts, SVG and syntax-highlighted code. A hex view for everything else, plus version and CPU architecture details for `.exe` and `.dll` files.
A file changed while it's shown (saved from an editor, say) is shown again as it is now. Click the path in the details to copy it.

**🗜 Archives as folders**
Browse, preview and extract `zip`, `7z`, `rar` and `tar` (`.gz`, `.xz`, `.zst`, `.bz2`) archives as if they were folders, or compress files into one.

**🎬 Shrink videos and pictures**
Right-click → **Compress video…** / **Compress picture…** opens an editor that shows the result's size before you commit (for example, a 132 MB phone screen recording came out at about 9 MB with the Small preset).
- Video: presets (Small, Balanced, High quality, Fit 10 MB for Discord and chats), quality or target file size, resolution, frame rate, H.264 or HEVC, keep or remove the sound, and trimming on a timeline with frame previews (`I` / `O` set the ends, `Space` plays)
- **Preview quality** encodes a few seconds with your settings: compare it with the original side by side with a slider while both play, and the size estimate becomes a measured one
- Pictures: a before/after slider with zoom to actual pixels, a **Differences** view that lights up what compression changed, JPEG quality, longest side, and Auto format (PNG for pictures with transparency, JPEG for the rest). The size shown is exact. Photos are turned upright, and location and camera details aren't copied
- Several files at once with the same settings; the originals are never touched and the result can be undone
- Uses Windows' own encoders (the GPU for video), so there is nothing extra to install or download. iPhone videos (HEVC) and photos (HEIC) need Microsoft's free HEVC / HEIF extensions, which Ferret points to when they're missing

**🪟 Feels like Windows, only faster**
- The real Explorer context menu, including third-party entries (7-Zip, Git, VS Code, PowerToys…)
- Properties and Open with dialogs, and a clipboard that works with Explorer
- Copy, move and delete in the background with progress and conflict handling, plus undo/redo. Deleting thousands of items keeps the window responsive, a file that's in use asks **Try again / Skip / Skip all** instead of stopping everything, and emptying the Recycle Bin shows its progress and can be canceled
- Asks before a rename changes a file's extension
- Recycle Bin you can browse and restore from
- Asks for admin rights only when a protected folder needs it, and any window can restart as administrator with its tabs intact
- Drag files to and from other apps, including switching apps with `Alt+Tab` mid-drag. Files dropped in follow Explorer's rule: on the same drive they're moved (`Ctrl` copies, `Shift` moves), but an app that only lets its files be copied (mail, browsers, editors) always keeps its own, and the banner says which will happen before you let go
- Middle-click autoscroll, like Explorer: press the mouse wheel and move away from that point to scroll (hold it, or click once to follow the mouse until the next click)
- Windows 11-style menus, and each window is named after its folder in the taskbar and `Alt+Tab`, like Explorer
- Optional: make Ferret the default file manager (Win+E, folder double-clicks), with a one-click switch back. Apps' "Show in folder" opens Ferret too, with the file selected when the app passes it to Explorer: a small background helper swaps in Ferret before Explorer's window appears, and Control Panel and other non-folder windows stay in Explorer. Uninstalling Ferret gives folders back to Explorer

**◫ Split view — `Ctrl+\`**
Two folders side by side in one tab, each with its own history and selection. `Tab` switches between them, and the toolbar, preview and shortcuts follow the one you're in. Copy or move to the other pane from **Copy to ▸** / **Move to ▸**, the command palette, or by dragging across. Split tabs come back after a restart.

**🗂 Built for keyboards and many tabs**
Tabs (`Ctrl+T`, `Ctrl+Shift+T`), a command palette (`Ctrl+Shift+P`), folder filter (`Ctrl+F`), editable path bar (`Ctrl+L`), favorites (`Ctrl+D`), and open in terminal (`` Ctrl+` ``) or VS Code.
List or grid view with grouping by date, type, size or letter. Each folder remembers its own view, and folder types have presets.
Resize list columns by dragging, hide them by right-clicking the headings, and show only one type of file (pictures, documents, code…) from the toolbar.
Folder sizes appear in the Size column, counted in the background at low priority (can be turned off in Settings).
Recent folders are ranked by how often and how recently you use them. A folder counts once you stay in it for a moment, not when you pass through it.

**🐧 Plays well with others**
WSL distros, network (UNC) paths and all drives in the sidebar. Folders refresh live when their contents change.

**🎨 Make it yours**
Dark, light or system theme, with a Mica, Acrylic or solid backdrop. Pick any accent color and a comfortable or compact layout. Short, Fluent-style animations, which you can turn off in Settings (and which stay off when Windows' animation effects are off).

**🔄 Stays up to date**
Updates come from GitHub Releases: Ferret shows what's new, then updates and restarts in one click. If something goes wrong, Settings → **Open logs** has the app's log files.

---

## Demo

Code previews, filtering a folder, a Word document, `Ctrl+K` (names, then text inside files) and split view, in 30 seconds:

<p align="center">
  <img src="docs/demo.gif" alt="A 30-second tour of Ferret: code previews with syntax highlighting, filtering a folder as you type, a Word document rendered in the preview, Ctrl+K finding a file by name and then text inside files, and split view with a photo dragged from one pane to the other">
</p>

---

## <img src="src-tauri/icons/source/indexer-32.png" width="24" align="top" alt=""> Ferret Indexer (optional)

Without setup, search scans your drives in the background and watches for changes.
For **instant results on every NTFS drive**, enable the **FerretIndexer** service from inside Ferret (one UAC prompt).
It reads the NTFS file table (MFT) and follows the change journal (USN), so the index stays current without rescanning.

- Runs as its own small process, `ferret-indexer.exe`, and talks to Ferret over a named pipe
- Only returns results from folders the requesting user is allowed to list
- Also gives the Size column every folder's size at once, as WizTree does: it reads the file table once (about 5 s for a drive with a million files, where walking it took 36 s) and follows the change journal from there. Folders the user can't open are left out, as Ferret's own count does
- Saves index snapshots to `%PROGRAMDATA%\Ferret`, readable only by SYSTEM and Administrators
- Runs from its own copy in `%PROGRAMDATA%\Ferret\bin`. When Ferret updates, it has the service update itself to the same release, without a click or a UAC prompt: the service installs only a newer `ferret-indexer.exe` signed with Ferret's release key (so a copy swapped into Ferret's own folder can't become the service). Services installed by Ferret before 0.8.0 ask once (one UAC prompt)

```text
ferret-indexer --install | --uninstall | --status | --query <text> | --sizes <folder>... | --console | --probe
```

---

## Development

Ferret is a native Rust app drawn with [GPUI](https://www.gpui.rs) (Zed's UI framework), in `ferret-gpui/`. The first versions, up to 0.9.7, were a Tauri app with a SolidJS UI (`src/`, `src-tauri/`): it's kept as an archive for comparison, and its core (`src-tauri/ferret-core/`) is shared.

**Requirements:** Windows 10/11, [Rust](https://rustup.rs) with its Linux targets (`rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl`, for the folder-size helper inside WSL and on SSH servers), and the Windows SDK (`rc.exe`, for the exe's icon and version details). Releases also need [NSIS](https://nsis.sourceforge.io) (a Tauri build's cached copy in `%LOCALAPPDATA%\tauri\NSIS` works) and Node.js with `yarn` (the updater's signer comes from the repo's packages).

```powershell
cd ferret-gpui
cargo run --release   # run Ferret
cargo test            # unit tests
```

A build run from `target\` is a development copy: its settings live in `%TEMP%\ferret-gpui` (never the installed app's), it shows a DEV badge and the DEV icon, and it never takes over as the default file manager or updates itself. Releases are published with `scripts/release.sh`: tests, a release build of `ferret.exe` and `ferret-indexer.exe`, a smoke test, the installer (`ferret-gpui/installer/ferret.nsi`), signatures and the updater feed. The installer and feed are the ones the Tauri app used, so installed Tauri copies update into the GPUI app with their settings.

The archived Tauri app still builds: `yarn` then `yarn app:dev` (development) or `yarn tauri build`. Its release script is in `scripts/archive/`.

| Path | What's there |
| --- | --- |
| `ferret-gpui/` | The app: its UI in GPUI on ferret-core (`src/`), the installer (`installer/`), the updater (`src/update.rs`) |
| `src/` | Archived Tauri app's UI (SolidJS + TypeScript) |
| `src-tauri/ferret-core/` | Backend without a UI (no Tauri): file operations, shell integration, archives, previews, SSH |
| `src-tauri/ferret-core/src/search/` | Search engine, MFT/USN indexer, named-pipe protocol |
| `src-tauri/ferret-core/src/redirect.rs` | Background helper that opens Explorer's folder windows in Ferret |
| `src-tauri/ferret-core/src/media.rs` | Video (Media Foundation transcoder) and picture (WIC) compression |
| `src-tauri/src/` | Archived Tauri shell: the window, the commands the UI calls (`cmd/`), the `fs://` and `thumb://` schemes, the drop target for files from other apps (`filedrop.rs`) |
| `ferret-gpui/src/bin/ferret-indexer.rs` | Entry point of the indexer service (the archived app's is `src-tauri/src/bin/`) |
| `src-tauri/wsl-size` | Linux program that counts folder sizes inside WSL (static, built into the app) |
| `src-tauri/vendor/drag` | Patched copy of the `drag` crate (dragging files out of Ferret; tells the drop target a drag is Ferret's own) |
