# Scope

**Scope** is a macOS desktop app that combines a **Finder**-style file browser
and an **Activity Monitor** / `htop`-style live system monitor in a single GUI.
It's built with [Tauri v2](https://tauri.app) (Rust backend, zero-build static
web frontend).

![Scope](src-tauri/app-icon.png)

## Features

A clean, native-feeling **light theme** styled after macOS Finder.

### 🔍 Finder
- **List view** with sortable columns (Name · Size · Kind · Date) and a
  macOS-style **column (Miller) view** for drilling through folders.
- Real **QuickLook thumbnails** rendered inline (image content, PDF first
  pages, video frames including WMV via FFmpeg, app icons…), generated lazily
  and cached on disk.
- A **preview / info pane** with live previews of images, video, audio, PDFs,
  and text/source files, plus full metadata (size, kind, created / modified /
  accessed dates, permissions, path).
- Sidebar of favorites and mounted volumes with custom icons.
- Clickable breadcrumb path, plus Back / Forward / Up / Home navigation.
- Arrow-key row and folder navigation, including Shift-range selection.
- Live name filter; toggle hidden files with the Finder hotkey **⌘⇧.**.
- Double-click folders to open, files to launch in their default app.
- **Open** / **Reveal in Finder** actions in the preview pane.
- Right-click a file or folder to **Copy Absolute Path** in either view.
  Multiple selected items copy one absolute path per line.
- **Size on disk** uses allocated blocks (`st_blocks × 512` on macOS), so
  sparse files contribute only their allocated space to folder totals and size
  sorting. File previews also show logical size. Folder totals count hard links
  once and do not follow symlinks. Sizes use binary units (KiB, MiB, GiB).
- Drag files between Scope windows: move on the same drive, copy between
  drives, or hold Option to copy. Permission failures retry the remaining
  selection through Finder as a batch (one command per copy/move operation),
  which handles macOS authentication and displays transfer progress. Allow
  Scope to control Finder when macOS asks; if previously denied, enable it
  under System Settings → Privacy & Security → Automation. Existing files
  are never replaced by the Finder retry; Finder may report a name conflict.
- Safe eject releases media previews in all Scope windows, checks the macOS
  mount information, and uses Finder if the standard eject needs assistance.
  Active Scope transfers block eject; successful eject refreshes every window.

### 📊 Monitor (htop-like, all in one view)
- Live **time-series graphs** for CPU, memory, and network throughput
  (rolling ~3-minute history).
- Per-core **CPU** usage bars with color-coded load, plus overall usage,
  CPU model, and average clock.
- **Memory** and **Swap** bars with used / total figures.
- **Network** live up/down throughput and cumulative totals.
- **Load average**, process count, uptime, OS and kernel version.
- **Disk** usage bars for every mounted volume.
- A sortable, filterable **process table** (PID, name, CPU%, memory, disk
  writes, status, run time) with a one-click **force-quit** button.
- Refreshes live every 1.5s; pausable.

### ⌨️ CLI
```
scope <folder>
```
Opens the given folder (default: the current directory) in a new Scope window.
Repeated commands create additional windows in the existing app process, like
opening multiple Finder windows. CLI-opened windows are centered and brought
to the foreground.

## Building

Requirements: Rust (stable) and the Tauri CLI.

```sh
# one-time: install the Tauri CLI
cargo install tauri-cli --version "^2.0"

# build a release .app + .dmg into src-tauri/target/release/bundle/
cargo tauri build

# or run in development
cargo tauri dev
```

The frontend is plain HTML/CSS/JS in `src/` — there is **no npm/Node build
step**.

The signed macOS bundle includes Apple's [Apple Events entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.automation.apple-events)
and an Automation usage description for permission-assisted Finder transfers.

Run backend tests with `cargo test --manifest-path src-tauri/Cargo.toml`.
The optional Finder integration test launches Finder and uses temporary files:
`cargo test --manifest-path src-tauri/Cargo.toml finder_transfer_copies_and_moves_literal_filenames_without_overwriting -- --ignored`.
To test both eject paths using a disposable disk image:
`cargo test --manifest-path src-tauri/Cargo.toml ejects_temporary_disk_image_with_diskutil_and_finder -- --ignored`.

## Installing the CLI

After building (or after copying `Scope.app` to `/Applications`), put the
`scope` launcher on your `PATH`:

```sh
ln -s "$PWD/scope" /usr/local/bin/scope
# then, from anywhere:
scope ~/Documents
```

The launcher searches `SCOPE_BIN`, `/Applications/Scope.app`,
`~/Applications/Scope.app`, and local dev builds, in that order.

## Project layout

```
scope/
├── scope                 # CLI launcher script
├── src/                  # static web frontend (no build step)
│   ├── index.html
│   ├── styles.css
│   └── app.js
└── src-tauri/            # Rust backend + Tauri config
    ├── src/lib.rs        # commands: file browsing + system metrics
    ├── src/main.rs
    ├── tauri.conf.json
    ├── gen_icon.py       # regenerates app-icon.png
    └── icons/            # generated app icons
```

## License

MIT
