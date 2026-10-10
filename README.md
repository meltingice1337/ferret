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
  <img alt="Tauri 2" src="https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-backend-B7410E?logo=rust&logoColor=white">
  <img alt="SolidJS" src="https://img.shields.io/badge/SolidJS-UI-2C4F7C?logo=solid&logoColor=white">
</p>

---

## Features

**⚡ Instant search — `Ctrl+K`**
Fuzzy search over every file on every drive, ranked by name match and how often you open things.
Filters: `ext:pdf,docx` · `type:folder` · `size:>100mb` · `modified:<7d` · `dm:2026-03-04` · `in:"C:\Program Files"` · `!node_modules\` · globs like `*.rs`.

**👁 Previews — `Alt+P`**
Images, video, audio, PDF, Word (`.docx`), Markdown, CSV, fonts, SVG and syntax-highlighted code. A hex view for everything else, plus version and CPU architecture details for `.exe` and `.dll` files.

**🗜 Archives as folders**
Browse, preview and extract `zip`, `7z`, `rar` and `tar` (`.gz`, `.xz`, `.zst`, `.bz2`) archives as if they were folders, or compress files into one.

**🪟 Feels like Windows, only faster**
- The real Explorer context menu, including third-party entries (7-Zip, Git, VS Code, PowerToys…)
- Properties and Open with dialogs, and a clipboard that works with Explorer
- Copy and move in the background with progress and conflict handling, plus undo/redo
- Recycle Bin you can browse and restore from
- Asks for admin rights only when a protected folder needs it, and any window can restart as administrator with its tabs intact
- Optional: make Ferret the default file manager (Win+E, folder double-clicks), with a one-click switch back

**🗂 Built for keyboards and many tabs**
Tabs (`Ctrl+T`, `Ctrl+Shift+T`), a command palette (`Ctrl+Shift+P`), folder filter (`Ctrl+F`), editable path bar (`Ctrl+L`), favorites (`Ctrl+D`), and open in terminal (`` Ctrl+` ``) or VS Code.
List or grid view with grouping by date, type, size or letter. Each folder remembers its own view, and folder types have presets.

**🐧 Plays well with others**
WSL distros, network (UNC) paths and all drives in the sidebar. Folders refresh live when their contents change.

**🎨 Make it yours**
Dark, light or system theme, with a Mica, Acrylic or solid backdrop. Pick any accent color and a comfortable or compact layout.

---

## <img src="src-tauri/icons/source/indexer-32.png" width="24" align="top" alt=""> Ferret Indexer (optional)

Without setup, search scans your drives in the background and watches for changes.
For **instant results on every NTFS drive**, enable the **FerretIndexer** service from inside Ferret (one UAC prompt).
It reads the NTFS file table (MFT) and follows the change journal (USN), so the index stays current without rescanning.

- Runs as its own small process, `ferret-indexer.exe`, and talks to Ferret over a named pipe
- Only returns results from folders the requesting user is allowed to list
- Saves index snapshots to `%PROGRAMDATA%\Ferret`, readable only by SYSTEM and Administrators

```text
ferret-indexer --install | --uninstall | --status | --query <text> | --console | --probe
```

---

## Development

**Requirements:** Windows 10/11, [Rust](https://rustup.rs), Node.js 20+, and WebView2 (included with Windows 11).

```powershell
npm install
npm run tauri dev      # run the app with hot reload
npm run tauri build    # release build + installer
```

| Path | What's there |
| --- | --- |
| `src/` | UI (SolidJS + TypeScript) |
| `src-tauri/src/` | Backend: file operations, shell integration, archives, previews |
| `src-tauri/src/search/` | Search engine, MFT/USN indexer, named-pipe protocol |
| `src-tauri/src/bin/ferret-indexer.rs` | Entry point of the indexer service |
