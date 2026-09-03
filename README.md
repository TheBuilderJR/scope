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
  pages, video frames, app icons…), generated lazily and cached on disk.
- A **preview / info pane** with live previews of images, video, audio, PDFs,
  and text/source files, plus full metadata (size, kind, created / modified /
  accessed dates, permissions, path).
- Sidebar of favorites and mounted volumes with custom icons.
- Clickable breadcrumb path, plus Back / Forward / Up / Home navigation.
- Live name filter; toggle hidden files with the Finder hotkey **⌘⇧.**.
- Double-click folders to open, files to launch in their default app.
- **Open** / **Reveal in Finder** actions in the preview pane.

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
opening multiple Finder windows.

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
