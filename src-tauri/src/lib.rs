// Scope - a combined Finder-style file browser and Activity-Monitor / htop-style
// system monitor, built with Tauri v2.
//
// The Rust side exposes two families of commands to the webview:
//   * file browsing  (list_dir, open_path, reveal_in_finder, home_dir, ...)
//   * system metrics (system_snapshot, process_list, kill_process)
// plus initial_path() so the GUI can honour `scope <folder>` from the CLI.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::Serialize;
use sysinfo::{Disks, Networks, Pid, ProcessesToUpdate, System};
use tauri::ipc::Channel;
use tauri::Manager;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct AppState {
    sys: Mutex<System>,
    networks: Mutex<Networks>,
    // Every Finder window gets its own CLI-provided starting directory.
    initial_paths: Mutex<HashMap<String, String>>,
}

// Eject cancels scans and waits for their directory handles to be dropped.
static SIZE_SCAN_GENERATION: AtomicU64 = AtomicU64::new(0);
static SIZE_SCAN_LOCK: RwLock<()> = RwLock::new(());

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// File browser
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Entry {
    name: String,
    path: String,
    is_dir: bool,
    is_symlink: bool,
    hidden: bool,
    size: u64,
    modified: Option<i64>, // seconds since the unix epoch
    created: Option<i64>,  // seconds since the unix epoch
    kind: String,
}

#[derive(Serialize)]
struct DirListing {
    path: String,
    parent: Option<String>,
    entries: Vec<Entry>,
}

fn describe_kind(path: &Path, is_dir: bool) -> String {
    if is_dir {
        // .app bundles read as directories but are really applications.
        if path.extension().and_then(|e| e.to_str()) == Some("app") {
            return "Application".to_string();
        }
        return "Folder".to_string();
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("rs") => "Rust Source",
        Some("js") | Some("mjs") | Some("cjs") => "JavaScript",
        Some("ts") | Some("tsx") => "TypeScript",
        Some("json") => "JSON",
        Some("toml") => "TOML",
        Some("md") | Some("markdown") => "Markdown",
        Some("txt") | Some("log") => "Plain Text",
        Some("html") | Some("htm") => "HTML Document",
        Some("css") => "Stylesheet",
        Some("pdf") => "PDF Document",
        Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("webp") | Some("svg")
        | Some("heic") => "Image",
        Some("mp4") | Some("mov") | Some("mkv") | Some("avi") | Some("wmv") => "Movie",
        Some("mp3") | Some("wav") | Some("flac") | Some("aac") | Some("m4a") => "Audio",
        Some("zip") | Some("gz") | Some("tar") | Some("bz2") | Some("xz") | Some("7z") => "Archive",
        Some("app") => "Application",
        Some("sh") | Some("bash") | Some("zsh") => "Shell Script",
        Some("py") => "Python Source",
        Some("go") => "Go Source",
        Some("c") | Some("h") => "C Source",
        Some("cpp") | Some("cc") | Some("hpp") => "C++ Source",
        Some(other) => return format!("{} File", other.to_uppercase()),
        None => "Document",
    }
    .to_string()
}

#[tauri::command]
fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

#[tauri::command]
fn list_dir(path: String) -> Result<DirListing, String> {
    let dir = PathBuf::from(&path);
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    if !dir.is_dir() {
        return Err(format!("Not a directory: {}", dir.display()));
    }

    let read = std::fs::read_dir(&dir).map_err(|e| format!("{}: {}", dir.display(), e))?;

    let mut entries: Vec<Entry> = Vec::new();
    for item in read.flatten() {
        let p = item.path();
        let name = item.file_name().to_string_lossy().to_string();
        // DirEntry::metadata does not follow symlinks. Reuse it for ordinary
        // entries so the initial listing pays for one metadata syscall rather
        // than two; only symlinks need a second, following lookup.
        let sym_meta = item.metadata().ok();
        let is_symlink = sym_meta
            .as_ref()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        let meta = if is_symlink {
            std::fs::metadata(&p).ok()
        } else {
            sym_meta.clone()
        };
        let is_dir = meta
            .as_ref()
            .map(|m| m.is_dir())
            .unwrap_or_else(|| p.is_dir());
        let size = if is_dir {
            0
        } else {
            sym_meta.as_ref().map(allocated_size).unwrap_or(0)
        };
        let modified = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        let created = meta
            .as_ref()
            .and_then(|m| m.created().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);

        entries.push(Entry {
            hidden: name.starts_with('.'),
            kind: describe_kind(&p, is_dir),
            path: p.to_string_lossy().to_string(),
            name,
            is_dir,
            is_symlink,
            size,
            modified,
            created,
        });
    }

    // Folders first, then alphabetical (case-insensitive).
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    let parent = dir.parent().map(|p| p.to_string_lossy().to_string());
    Ok(DirListing {
        path: dir.to_string_lossy().to_string(),
        parent,
        entries,
    })
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    open_with(&["--", &path])
}

#[tauri::command]
fn reveal_in_finder(path: String) -> Result<(), String> {
    open_with(&["-R", "--", &path])
}

#[tauri::command]
async fn copy_absolute_paths(paths: Vec<String>) -> Result<(), String> {
    if paths.is_empty() || paths.iter().any(|path| !Path::new(path).is_absolute()) {
        return Err("Select an item with an absolute path.".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        // Send literal UTF-8 paths over stdin, preserving spaces and symlinks.
        let mut child = std::process::Command::new("/usr/bin/pbcopy")
            .env("LC_CTYPE", "en_US.UTF-8")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let write_result = child
            .stdin
            .take()
            .unwrap()
            .write_all(paths.join("\n").as_bytes());
        let status = child.wait().map_err(|error| error.to_string())?;
        write_result.map_err(|error| error.to_string())?;
        if !status.success() {
            return Err(format!("Clipboard write failed: {status}"));
        }
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Move files/folders to the system Trash (recoverable, with Finder put-back),
/// rather than deleting them outright. macOS may block this call while Trash
/// is being emptied, so keep it off Tauri's command thread to avoid freezing
/// the app while the optimistic frontend remains interactive.
#[tauri::command]
async fn move_to_trash(paths: Vec<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        trash::delete_all(&paths).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferProgress {
    phase: &'static str,
    operation: &'static str,
    transferred_bytes: u64,
    total_bytes: u64,
    transferred_items: u64,
    total_items: u64,
    current: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferSummary {
    operation: String,
    transferred_bytes: u64,
    transferred_items: u64,
    destinations: Vec<String>,
    source_directories: Vec<String>,
}

#[derive(Clone, Copy, Default)]
struct TransferTotals {
    bytes: u64,
    items: u64,
}

struct TransferState<'a, F>
where
    F: FnMut(TransferProgress),
{
    totals: TransferTotals,
    transferred_bytes: u64,
    transferred_items: u64,
    operation: &'static str,
    current: String,
    last_update: Instant,
    emit: &'a mut F,
}

impl<F> TransferState<'_, F>
where
    F: FnMut(TransferProgress),
{
    fn update(&mut self, force: bool) {
        if !force && self.last_update.elapsed() < Duration::from_millis(50) {
            return;
        }
        (self.emit)(TransferProgress {
            phase: "copying",
            operation: self.operation,
            transferred_bytes: self.transferred_bytes,
            total_bytes: self.totals.bytes,
            transferred_items: self.transferred_items,
            total_items: self.totals.items,
            current: self.current.clone(),
        });
        self.last_update = Instant::now();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransferOperation {
    Copy,
    Move,
}

impl TransferOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Move => "move",
        }
    }
}

struct PlannedTransfer {
    source: PathBuf,
    target: PathBuf,
    source_directory: PathBuf,
    operation: TransferOperation,
    totals: TransferTotals,
    needs_finder: bool,
}

#[cfg(target_os = "macos")]
fn option_key_held() -> bool {
    use objc2_app_kit::{NSEvent, NSEventModifierFlags};
    NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Option)
}

#[cfg(not(target_os = "macos"))]
fn option_key_held() -> bool {
    false
}

#[cfg(unix)]
fn same_filesystem(source: &Path, destination: &Path) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;
    let source_metadata =
        std::fs::symlink_metadata(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let destination_metadata =
        std::fs::metadata(destination).map_err(|e| format!("{}: {e}", destination.display()))?;
    Ok(source_metadata.dev() == destination_metadata.dev())
}

#[cfg(windows)]
fn same_filesystem(source: &Path, destination: &Path) -> Result<bool, String> {
    Ok(source.components().next() == destination.components().next())
}

fn operation_for_path(
    source: &Path,
    destination: &Path,
    force_copy: bool,
) -> Result<TransferOperation, String> {
    if force_copy || !same_filesystem(source, destination)? {
        Ok(TransferOperation::Copy)
    } else {
        Ok(TransferOperation::Move)
    }
}

fn combined_operation(operations: impl IntoIterator<Item = TransferOperation>) -> &'static str {
    let mut saw_copy = false;
    let mut saw_move = false;
    for operation in operations {
        saw_copy |= operation == TransferOperation::Copy;
        saw_move |= operation == TransferOperation::Move;
    }
    match (saw_copy, saw_move) {
        (true, false) => "copy",
        (false, true) => "move",
        _ => "transfer",
    }
}

/// Resolve Finder-style drop semantics without modifying anything: moves on
/// the same filesystem, copies across filesystems, and Option always copies.
#[tauri::command]
fn transfer_operation(
    paths: Vec<String>,
    destination: String,
    force_copy: bool,
) -> Result<String, String> {
    if paths.is_empty() {
        return Err("No files were dropped".to_string());
    }
    let destination = std::fs::canonicalize(&destination)
        .map_err(|e| format!("{}: {e}", Path::new(&destination).display()))?;
    let force_copy = force_copy || option_key_held();
    let mut operations = Vec::with_capacity(paths.len());
    for source in paths {
        operations.push(operation_for_path(
            Path::new(&source),
            &destination,
            force_copy,
        )?);
    }
    Ok(combined_operation(operations).to_string())
}

/// Transfer files dropped from Finder or another Scope window into a directory.
/// The expensive tree walk and file I/O stay off Tauri's command thread, while
/// an IPC channel streams determinate byte/item progress back to the webview.
#[tauri::command]
async fn transfer_paths(
    paths: Vec<String>,
    destination: String,
    force_copy: bool,
    on_event: Channel<TransferProgress>,
) -> Result<TransferSummary, String> {
    // Query AppKit before moving to the blocking worker so Option reflects the
    // actual modifier state at the instant of the native drop.
    let force_copy = force_copy || option_key_held();
    tauri::async_runtime::spawn_blocking(move || {
        transfer_paths_blocking(paths, destination, force_copy, |progress| {
            // A closed destination window should not abort a copy already in
            // progress; sending to its channel simply becomes best-effort.
            let _ = on_event.send(progress);
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn transfer_paths_blocking<F>(
    paths: Vec<String>,
    destination: String,
    force_copy: bool,
    emit: F,
) -> Result<TransferSummary, String>
where
    F: FnMut(TransferProgress),
{
    transfer_paths_with_finder(paths, destination, force_copy, emit, finder_transfer)
}

fn transfer_paths_with_finder<F, R>(
    paths: Vec<String>,
    destination: String,
    force_copy: bool,
    mut emit: F,
    mut retry: R,
) -> Result<TransferSummary, String>
where
    F: FnMut(TransferProgress),
    R: FnMut(&[PlannedTransfer]) -> Result<Vec<PathBuf>, String>,
{
    if paths.is_empty() {
        return Err("No files were dropped".to_string());
    }

    let destination = std::fs::canonicalize(&destination)
        .map_err(|e| format!("{}: {e}", Path::new(&destination).display()))?;
    if !destination.is_dir() {
        return Err(format!("Not a directory: {}", destination.display()));
    }

    emit(TransferProgress {
        phase: "scanning",
        operation: "transfer",
        transferred_bytes: 0,
        total_bytes: 0,
        transferred_items: 0,
        total_items: 0,
        current: String::new(),
    });

    // Plan every top-level target and operation before writing anything. This
    // catches bad inputs early and reserves distinct names when two sources
    // share a name.
    let mut planned: Vec<PlannedTransfer> = Vec::new();
    let mut seen_sources = HashSet::new();
    let mut reserved_targets = HashSet::new();
    let mut totals = TransferTotals::default();
    for source in paths {
        let source = PathBuf::from(source);
        let source = if source.is_absolute() {
            source
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(source)
        };
        if !seen_sources.insert(source.clone()) {
            continue;
        }

        let metadata =
            std::fs::symlink_metadata(&source).map_err(|e| format!("{}: {e}", source.display()))?;
        let name = source
            .file_name()
            .ok_or_else(|| format!("Cannot transfer {}", source.display()))?;
        let operation = operation_for_path(&source, &destination, force_copy)?;
        let source_directory = source
            .parent()
            .ok_or_else(|| format!("Cannot transfer {}", source.display()))?;
        let source_directory = std::fs::canonicalize(source_directory)
            .map_err(|e| format!("{}: {e}", source_directory.display()))?;

        if operation == TransferOperation::Move && source_directory == destination {
            return Err(format!(
                "{} is already in this folder",
                source.file_name().unwrap_or_default().to_string_lossy()
            ));
        }

        if metadata.is_dir() {
            let canonical_source =
                std::fs::canonicalize(&source).map_err(|e| format!("{}: {e}", source.display()))?;
            if destination == canonical_source || destination.starts_with(&canonical_source) {
                return Err(format!(
                    "Cannot transfer {} into itself",
                    source.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }

        let mut path_totals = TransferTotals::default();
        let needs_finder = match measure_transfer(&source, &mut path_totals) {
            Ok(()) => false,
            Err(error) if finder_can_retry(&error) => {
                // Finder can read protected descendants after authentication.
                path_totals = TransferTotals { bytes: 0, items: 1 };
                true
            }
            Err(error) => return Err(error.to_string()),
        };
        totals.bytes = totals.bytes.saturating_add(path_totals.bytes);
        totals.items = totals.items.saturating_add(path_totals.items);
        emit(TransferProgress {
            phase: "scanning",
            operation: operation.as_str(),
            transferred_bytes: 0,
            total_bytes: totals.bytes,
            transferred_items: 0,
            total_items: totals.items,
            current: source.to_string_lossy().to_string(),
        });

        let target =
            unique_copy_target(&destination, name, metadata.is_dir(), &mut reserved_targets);
        planned.push(PlannedTransfer {
            source,
            target,
            source_directory,
            operation,
            totals: path_totals,
            needs_finder,
        });
    }

    if planned.is_empty() {
        return Err("No files were dropped".to_string());
    }

    let mut state = TransferState {
        totals,
        transferred_bytes: 0,
        transferred_items: 0,
        operation: combined_operation(planned.iter().map(|item| item.operation)),
        current: String::new(),
        last_update: Instant::now() - Duration::from_secs(1),
        emit: &mut emit,
    };
    state.update(true);

    let mut copied_targets: Vec<PathBuf> = Vec::new();
    let mut moved_paths: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut actual_destinations = Vec::new();
    for (index, item) in planned.iter().enumerate() {
        state.operation = item.operation.as_str();
        state.current = item.source.to_string_lossy().to_string();
        let before_bytes = state.transferred_bytes;
        let before_items = state.transferred_items;
        let mut root_created = false;
        let actual_target = item.target.clone();
        let mut result = if item.needs_finder {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        } else {
            match item.operation {
                TransferOperation::Copy => copy_entry(
                    &item.source,
                    &item.target,
                    &mut state,
                    &mut root_created,
                    true,
                ),
                TransferOperation::Move => std::fs::rename(&item.source, &item.target)
                    .map_err(|e| transfer_io_error(&item.source, e)),
            }
        };

        if result.as_ref().is_err_and(finder_can_retry) {
            // Never let Finder merge into a partial copy, or retry after failed cleanup.
            if root_created {
                match remove_copied_path(&item.target) {
                    Ok(()) => root_created = false,
                    Err(error) => {
                        result = Err(std::io::Error::other(format!(
                        "Could not remove incomplete copy {}: {error}. Transfer was not retried.",
                        item.target.display()
                    )))
                    }
                }
            }
            if !root_created {
                (state.emit)(TransferProgress {
                    phase: "authorizing",
                    operation: item.operation.as_str(),
                    transferred_bytes: before_bytes,
                    total_bytes: state.totals.bytes,
                    transferred_items: before_items,
                    total_items: state.totals.items,
                    current: item.source.to_string_lossy().to_string(),
                });
                // A single Finder command carries the remaining selection, so
                // authorization applies to the batch instead of each file.
                match retry(&planned[index..]) {
                    Ok(targets) if targets.len() == planned.len() - index => {
                        actual_destinations.extend(targets.iter().map(|path| path.to_string_lossy().into_owned()));
                        state.transferred_bytes = state.totals.bytes;
                        state.transferred_items = state.totals.items;
                        state.update(true);
                        break;
                    }
                    Ok(_) => result = Err(std::io::Error::other("Finder returned an incomplete batch result. Check the destination before retrying.")),
                    Err(error) => result = Err(std::io::Error::other(error)),
                }
            }
        }

        if root_created {
            copied_targets.push(actual_target.clone());
        }
        if let Err(error) = result {
            let mut error = error.to_string();
            for copied in copied_targets.iter().rev() {
                if let Err(rollback_error) = remove_copied_path(copied) {
                    error.push_str(&format!(
                        "; could not remove incomplete copy {}: {rollback_error}",
                        copied.display()
                    ));
                }
            }
            for (source, target) in moved_paths.iter().rev() {
                if let Err(rollback_error) = std::fs::rename(target, source) {
                    error.push_str(&format!(
                        "; could not restore {}: {rollback_error}",
                        source.display()
                    ));
                }
            }
            return Err(error);
        }
        if item.operation == TransferOperation::Move {
            moved_paths.push((item.source.clone(), actual_target.clone()));
        }
        actual_destinations.push(actual_target.to_string_lossy().to_string());
        state.transferred_bytes = before_bytes.saturating_add(item.totals.bytes);
        state.transferred_items = before_items.saturating_add(item.totals.items);
        state.update(true);
    }

    let operation = combined_operation(planned.iter().map(|item| item.operation)).to_string();
    let source_directories: HashSet<String> = planned
        .iter()
        .filter(|item| item.operation == TransferOperation::Move)
        .map(|item| item.source_directory.to_string_lossy().to_string())
        .collect();
    state.operation = combined_operation(planned.iter().map(|item| item.operation));
    state.update(true);
    Ok(TransferSummary {
        operation,
        transferred_bytes: state.transferred_bytes,
        transferred_items: state.transferred_items,
        destinations: actual_destinations,
        source_directories: source_directories.into_iter().collect(),
    })
}

fn measure_transfer(path: &Path, totals: &mut TransferTotals) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| transfer_io_error(path, e))?;
    totals.items = totals.items.saturating_add(1);
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        let entries = std::fs::read_dir(path).map_err(|e| transfer_io_error(path, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| transfer_io_error(path, e))?;
            measure_transfer(&entry.path(), totals)?;
        }
    } else if metadata.is_file() {
        totals.bytes = totals.bytes.saturating_add(metadata.len());
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("Unsupported file type: {}", path.display()),
        ));
    }
    Ok(())
}

fn copy_entry<F>(
    source: &Path,
    target: &Path,
    state: &mut TransferState<'_, F>,
    root_created: &mut bool,
    is_root: bool,
) -> std::io::Result<()>
where
    F: FnMut(TransferProgress),
{
    let metadata = std::fs::symlink_metadata(source).map_err(|e| transfer_io_error(source, e))?;
    state.current = source.to_string_lossy().to_string();

    if metadata.file_type().is_symlink() {
        copy_symlink(source, target)?;
        if is_root {
            *root_created = true;
        }
        state.transferred_items = state.transferred_items.saturating_add(1);
        state.update(false);
        return Ok(());
    }

    if metadata.is_dir() {
        std::fs::create_dir(target).map_err(|e| transfer_io_error(target, e))?;
        if is_root {
            *root_created = true;
        }
        let entries = std::fs::read_dir(source).map_err(|e| transfer_io_error(source, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| transfer_io_error(source, e))?;
            copy_entry(
                &entry.path(),
                &target.join(entry.file_name()),
                state,
                root_created,
                false,
            )?;
        }
        // Apply restrictive source permissions only after the children exist.
        std::fs::set_permissions(target, metadata.permissions())
            .map_err(|e| transfer_io_error(target, e))?;
        state.transferred_items = state.transferred_items.saturating_add(1);
        state.update(false);
        return Ok(());
    }

    if metadata.is_file() {
        let mut input = File::open(source).map_err(|e| transfer_io_error(source, e))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .map_err(|e| transfer_io_error(target, e))?;
        if is_root {
            *root_created = true;
        }
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let count = input
                .read(&mut buffer)
                .map_err(|e| transfer_io_error(source, e))?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .map_err(|e| transfer_io_error(target, e))?;
            state.transferred_bytes = state.transferred_bytes.saturating_add(count as u64);
            state.update(false);
        }
        output.flush().map_err(|e| transfer_io_error(target, e))?;
        std::fs::set_permissions(target, metadata.permissions())
            .map_err(|e| transfer_io_error(target, e))?;
        state.transferred_items = state.transferred_items.saturating_add(1);
        state.update(false);
        return Ok(());
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("Unsupported file type: {}", source.display()),
    ))
}

fn copy_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
    let link = std::fs::read_link(source).map_err(|e| transfer_io_error(source, e))?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(link, target).map_err(|e| transfer_io_error(target, e))
    }
    #[cfg(windows)]
    {
        if source.is_dir() {
            std::os::windows::fs::symlink_dir(link, target)
                .map_err(|e| transfer_io_error(target, e))
        } else {
            std::os::windows::fs::symlink_file(link, target)
                .map_err(|e| transfer_io_error(target, e))
        }
    }
}

fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn unique_copy_target(
    destination: &Path,
    name: &std::ffi::OsStr,
    is_dir: bool,
    reserved: &mut HashSet<PathBuf>,
) -> PathBuf {
    let original = destination.join(name);
    if !path_exists(&original) && reserved.insert(original.clone()) {
        return original;
    }

    let name_path = Path::new(name);
    let stem = if is_dir {
        name.to_string_lossy().into_owned()
    } else {
        name_path
            .file_stem()
            .unwrap_or(name)
            .to_string_lossy()
            .into_owned()
    };
    let extension = if is_dir {
        None
    } else {
        name_path.extension().map(|value| value.to_string_lossy())
    };

    for copy_number in 1_u64.. {
        let suffix = if copy_number == 1 {
            " copy".to_string()
        } else {
            format!(" copy {copy_number}")
        };
        let candidate_name = match &extension {
            Some(extension) => format!("{stem}{suffix}.{extension}"),
            None => format!("{stem}{suffix}"),
        };
        let candidate = destination.join(candidate_name);
        if !path_exists(&candidate) && reserved.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!()
}

fn remove_copied_path(path: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn transfer_io_error(path: &Path, error: std::io::Error) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn finder_can_retry(error: &std::io::Error) -> bool {
    cfg!(target_os = "macos") && error.kind() == std::io::ErrorKind::PermissionDenied
}

#[cfg(target_os = "macos")]
fn finder_transfer(items: &[PlannedTransfer]) -> Result<Vec<PathBuf>, String> {
    let first = items.first().ok_or("Empty Finder batch")?;
    // Paths are argv data, never interpolated into AppleScript or a shell.
    let destination = first.target.parent().ok_or("Missing destination folder")?;
    let mut command = std::process::Command::new("/usr/bin/osascript");
    command
        .args(["-e", include_str!("finder_transfer.applescript")])
        .arg(destination);
    for item in items {
        if item.target.parent() != Some(destination) {
            return Err("Finder batch destinations must match".to_string());
        }
        command.arg(item.operation.as_str()).arg(&item.source);
    }
    let output = command
        .output()
        .map_err(|error| format!("Could not start Finder transfer: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(if detail.contains("(-1743)") {
            "Allow Scope to control Finder in System Settings → Privacy & Security → Automation, then try again.".to_string()
        } else if detail.contains("(-128)") {
            "Transfer canceled in Finder. Check the destination for any incomplete copy."
                .to_string()
        } else {
            format!("Finder could not complete the transfer: {}. Check the destination before retrying.", detail.trim())
        });
    }
    serde_json::from_slice::<Vec<PathBuf>>(&output.stdout).map_err(|error| {
        format!(
            "Could not read Finder batch result: {error}. Check the destination before retrying."
        )
    })
}

#[cfg(not(target_os = "macos"))]
fn finder_transfer(_items: &[PlannedTransfer]) -> Result<Vec<PathBuf>, String> {
    Err("Finder permission approval is only available on macOS".to_string())
}

/// Recursively sum the on-disk size of a directory's contents (like Finder's
/// "Calculate all sizes"). Symlinks are not followed, to avoid cycles and
/// double-counting. Runs on a blocking thread since it walks the whole subtree.
#[tauri::command]
async fn dir_size(path: String) -> Result<u64, String> {
    let generation = SIZE_SCAN_GENERATION.load(Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        let _scan = SIZE_SCAN_LOCK.read().map_err(|e| e.to_string())?;
        dir_size_walk_cancellable(Path::new(&path), || {
            SIZE_SCAN_GENERATION.load(Ordering::SeqCst) != generation
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Filesystem allocation, not the logical length of sparse/compressed files.
fn allocated_size(metadata: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.blocks().saturating_mul(512)
    }
    #[cfg(not(unix))]
    {
        metadata.len()
    }
}

#[cfg(test)]
fn dir_size_walk(path: &Path) -> u64 {
    dir_size_walk_cancellable(path, || false).unwrap()
}

fn dir_size_walk_cancellable(path: &Path, cancelled: impl Fn() -> bool) -> Result<u64, String> {
    let mut pending = vec![path.to_path_buf()];
    #[cfg(unix)]
    let mut seen = HashSet::new();
    let mut total = 0u64;
    while let Some(path) = pending.pop() {
        if cancelled() {
            return Err("Folder size scan cancelled for eject".to_string());
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // Count hard-linked files once, like du, and never follow symlinks.
            if !seen.insert((metadata.dev(), metadata.ino())) {
                continue;
            }
        }
        total = total.saturating_add(allocated_size(&metadata));
        if metadata.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                // Drop the iterator (and its open directory handle) before
                // visiting children. Recursive iteration pins every ancestor
                // on an external drive for the entire scan, preventing eject.
                for entry in entries.flatten() {
                    if cancelled() {
                        return Err("Folder size scan cancelled for eject".to_string());
                    }
                    pending.push(entry.path());
                }
            }
        }
    }
    Ok(total)
}

fn open_with(args: &[&str]) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "windows")]
    let program = "explorer";

    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn initial_path(window: tauri::WebviewWindow, state: tauri::State<AppState>) -> Option<String> {
    state.initial_paths.lock().unwrap().remove(window.label())
}

#[derive(Serialize)]
struct MountedVolume {
    name: String,
    path: String,
    removable: bool,
    ejectable: bool,
}

fn mount_is_ejectable(path: &Path, removable: bool) -> bool {
    path != Path::new("/") && (removable || path.starts_with("/Volumes"))
}

/// Return the local, user-visible mounted volumes that macOS exposes in
/// Finder. `sysinfo` deliberately omits hidden APFS/system volumes here.
#[tauri::command]
fn mounted_volumes() -> Vec<MountedVolume> {
    let disks = Disks::new_with_refreshed_list();
    let mut volumes: Vec<MountedVolume> = disks
        .iter()
        .map(|disk| {
            let path = disk.mount_point();
            MountedVolume {
                name: disk.name().to_string_lossy().to_string(),
                path: path.to_string_lossy().to_string(),
                removable: disk.is_removable(),
                ejectable: mount_is_ejectable(path, disk.is_removable()),
            }
        })
        .collect();

    // Keep the startup disk first, followed by mounted volumes by name.
    volumes.sort_by(
        |a, b| match (a.path.as_str() == "/", b.path.as_str() == "/") {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        },
    );
    volumes
}

/// Safely eject a user-visible mounted disk. `diskutil eject` first performs
/// a non-forced unmount, so open files can veto the operation instead of being
/// disconnected. The path must exactly match an ejectable mount reported by macOS.
#[tauri::command]
async fn eject_volume(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || eject_volume_blocking(&path))
        .await
        .map_err(|e| e.to_string())?
}

fn eject_volume_blocking(path: &str) -> Result<(), String> {
    SIZE_SCAN_GENERATION.fetch_add(1, Ordering::SeqCst);
    let _scans = SIZE_SCAN_LOCK.write().map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    {
        let requested = std::fs::canonicalize(path).map_err(|e| format!("{path}: {e}"))?;
        let info = std::process::Command::new("/usr/sbin/diskutil")
            .args(["info", "-plist"])
            .arg(&requested)
            .output()
            .map_err(|error| format!("Could not inspect mounted disk: {error}"))?;
        if !info.status.success() {
            return Err(format!(
                "Could not inspect mounted disk: {}",
                String::from_utf8_lossy(&info.stderr).trim()
            ));
        }
        let info = plist::Value::from_reader_xml(std::io::Cursor::new(info.stdout))
            .map_err(|error| format!("Could not read mounted disk information: {error}"))?;
        let mount = validated_eject_mount(&requested, &info)?;
        let output = std::process::Command::new("/usr/sbin/diskutil")
            .arg("eject")
            .arg(&mount)
            .output()
            .map_err(|e| format!("Could not start diskutil: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("diskutil exited with {}", output.status)
        };
        let finder = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", include_str!("finder_eject.applescript")])
            .arg(&mount)
            .current_dir("/")
            .output()
            .map_err(|error| format!("{detail}; could not ask Finder to eject: {error}"))?;
        if finder.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{detail}; Finder: {}",
                String::from_utf8_lossy(&finder.stderr).trim()
            ))
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err("Safe eject is currently supported on macOS".to_string())
    }
}

#[cfg(target_os = "macos")]
fn validated_eject_mount(requested: &Path, info: &plist::Value) -> Result<PathBuf, String> {
    let fields = info
        .as_dictionary()
        .ok_or("Invalid mounted disk information")?;
    let mount = fields
        .get("MountPoint")
        .and_then(plist::Value::as_string)
        .ok_or("This disk is not mounted")?;
    let mount = std::fs::canonicalize(mount).map_err(|error| error.to_string())?;
    let ejectable = fields
        .get("Ejectable")
        .and_then(plist::Value::as_boolean)
        .unwrap_or(false);
    if mount != requested || !mount_is_ejectable(&mount, ejectable) {
        return Err("This path is not an ejectable mounted volume".to_string());
    }
    Ok(mount)
}

#[tauri::command]
fn open_full_disk_access_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/open")
            .arg("x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles")
            .status()
            .map_err(|e| format!("Could not open System Settings: {e}"))?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("System Settings exited with {status}"))
    }

    #[cfg(not(target_os = "macos"))]
    Err("Full Disk Access settings are only available on macOS".to_string())
}

#[derive(Serialize)]
struct TextPreview {
    is_text: bool,
    text: String,
    truncated: bool,
}

/// Read the start of a file for previewing. Returns up to `max_bytes` (default
/// 256 KiB) of text if the content looks like UTF-8 text, otherwise flags it as
/// binary so the UI can fall back to an info panel.
#[tauri::command]
fn read_text_preview(path: String) -> Result<TextPreview, String> {
    use std::io::Read;
    const CAP: usize = 256 * 1024;
    let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; CAP];
    let n = f.read(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(n);

    // Binary if it contains a NUL byte in the sampled region.
    let has_nul = buf.contains(&0);
    let truncated = n == CAP;
    match (has_nul, String::from_utf8(buf)) {
        (false, Ok(text)) => Ok(TextPreview {
            is_text: true,
            text,
            truncated,
        }),
        _ => Ok(TextPreview {
            is_text: false,
            text: String::new(),
            truncated: false,
        }),
    }
}

#[derive(Serialize)]
struct PathInfo {
    name: String,
    path: String,
    is_dir: bool,
    is_symlink: bool,
    kind: String,
    size: u64,
    logical_size: u64,
    item_count: Option<usize>,
    modified: Option<i64>,
    created: Option<i64>,
    accessed: Option<i64>,
    mode: Option<u32>,
}

fn systime_secs(t: std::io::Result<std::time::SystemTime>) -> Option<i64> {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// Detailed metadata for a single path, used to populate the preview/info panel.
#[tauri::command]
fn stat_path(path: String) -> Result<PathInfo, String> {
    let p = PathBuf::from(&path);
    let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    let sym = std::fs::symlink_metadata(&p).ok();
    let is_dir = meta.is_dir();

    let item_count = if is_dir {
        std::fs::read_dir(&p).ok().map(|rd| rd.count())
    } else {
        None
    };

    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode())
    };
    #[cfg(not(unix))]
    let mode = None;

    Ok(PathInfo {
        name: p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone()),
        kind: describe_kind(&p, is_dir),
        is_dir,
        is_symlink: sym
            .as_ref()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false),
        size: if is_dir {
            0
        } else {
            allocated_size(sym.as_ref().unwrap_or(&meta))
        },
        logical_size: sym.as_ref().unwrap_or(&meta).len(),
        item_count,
        modified: systime_secs(meta.modified()),
        created: systime_secs(meta.created()),
        accessed: systime_secs(meta.accessed()),
        mode,
        path,
    })
}

/// Stable, filesystem-safe cache key for a thumbnail: FNV-1a over the path,
/// modification time and requested size, so edits invalidate the cache.
fn thumb_key(path: &str, mtime: i64, size: u32) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    mix(path.as_bytes());
    mix(&mtime.to_le_bytes());
    mix(&size.to_le_bytes());
    format!("{:016x}", h)
}

/// Quick Look commonly returns only a generic icon for WMV files. Prefer a
/// decoded video frame when FFmpeg is available, checking the standard Intel
/// and Apple Silicon Homebrew locations as well as the inherited PATH.
#[cfg(target_os = "macos")]
fn render_wmv_thumbnail(path: &str, out: &Path, size: u32) -> bool {
    let scale = format!("scale={size}:{size}:force_original_aspect_ratio=decrease");
    for seek in ["1", "0"] {
        for ffmpeg in [
            "/opt/homebrew/bin/ffmpeg",
            "/usr/local/bin/ffmpeg",
            "ffmpeg",
        ] {
            let _ = std::fs::remove_file(out);
            let result = std::process::Command::new(ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-ss",
                    seek,
                    "-i",
                ])
                .arg(path)
                .args(["-frames:v", "1", "-vf", &scale])
                .arg(out)
                .output();
            if result
                .map(|output| output.status.success())
                .unwrap_or(false)
                && out.is_file()
            {
                return true;
            }
        }
    }
    // Do not let a partial file from a failed decoder attempt become a cache
    // hit on the next request.
    let _ = std::fs::remove_file(out);
    false
}

/// Generate (or return a cached) QuickLook thumbnail for a file, exactly like
/// Finder does — image content, PDF first pages, video frames, doc icons, etc.
/// Returns the absolute path to a PNG, or None if no thumbnail is available.
#[tauri::command]
async fn thumbnail(
    app: tauri::AppHandle,
    path: String,
    size: u32,
) -> Result<Option<String>, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, path, size);
        return Ok(None);
    }

    #[cfg(target_os = "macos")]
    {
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return Ok(None),
        };
        if meta.is_dir() {
            return Ok(None);
        }
        let size = size.clamp(16, 1024);
        let mtime = systime_secs(meta.modified()).unwrap_or(0);
        let is_wmv = Path::new(&path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.eq_ignore_ascii_case("wmv"))
            .unwrap_or(false);
        // Keep WMV frame previews separate from any generic Quick Look icon
        // cached by an older Scope version for the same file.
        let key = if is_wmv {
            format!("wmv-frame-{}", thumb_key(&path, mtime, size))
        } else {
            thumb_key(&path, mtime, size)
        };

        let cache_dir = app
            .path()
            .app_cache_dir()
            .map_err(|e| e.to_string())?
            .join("thumbnails");
        std::fs::create_dir_all(&cache_dir).map_err(|e| e.to_string())?;
        let out = cache_dir.join(format!("{key}.png"));
        if out.exists() {
            return Ok(Some(out.to_string_lossy().to_string()));
        }

        if is_wmv {
            let rendered = tauri::async_runtime::spawn_blocking({
                let path = path.clone();
                let out = out.clone();
                move || render_wmv_thumbnail(&path, &out, size)
            })
            .await
            .map_err(|e| e.to_string())?;
            if rendered {
                return Ok(Some(out.to_string_lossy().to_string()));
            }
        }

        // qlmanage writes "<basename>.png" into the output dir; give each
        // request its own scratch dir to avoid basename collisions.
        let scratch = cache_dir.join(format!("tmp-{key}"));
        std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;

        let gen = tauri::async_runtime::spawn_blocking({
            let path = path.clone();
            let scratch = scratch.clone();
            move || {
                std::process::Command::new("qlmanage")
                    .args([
                        "-t",
                        "-s",
                        &size.to_string(),
                        "-o",
                        &scratch.to_string_lossy(),
                        &path,
                    ])
                    .output()
                    .ok()
            }
        })
        .await
        .map_err(|e| e.to_string())?;
        let _ = gen;

        // Find whatever qlmanage produced (it appends ".png" to the filename).
        let produced = std::fs::read_dir(&scratch)
            .ok()
            .and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| p.is_file()));

        let result = match produced {
            Some(p) => {
                let _ = std::fs::rename(&p, &out);
                if out.exists() {
                    Some(out.to_string_lossy().to_string())
                } else {
                    None
                }
            }
            None => None,
        };
        let _ = std::fs::remove_dir_all(&scratch);
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// System monitor
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CoreInfo {
    name: String,
    usage: f32,
    frequency: u64, // MHz
}

#[derive(Serialize)]
struct DiskInfo {
    name: String,
    mount: String,
    fs: String,
    total: u64,
    available: u64,
    removable: bool,
}

#[derive(Serialize)]
struct Snapshot {
    // cpu
    cpu_usage: f32,
    cores: Vec<CoreInfo>,
    cpu_brand: String,
    load_one: f64,
    load_five: f64,
    load_fifteen: f64,
    // memory (bytes)
    mem_total: u64,
    mem_used: u64,
    mem_available: u64,
    swap_total: u64,
    swap_used: u64,
    // network (cumulative bytes; the UI derives rates from timestamps)
    net_rx_total: u64,
    net_tx_total: u64,
    // process rollups
    process_count: usize,
    // host
    uptime: u64,
    host_name: String,
    os_version: String,
    kernel_version: String,
    disks: Vec<DiskInfo>,
}

#[tauri::command]
fn system_snapshot(state: tauri::State<AppState>) -> Snapshot {
    let mut sys = state.sys.lock().unwrap();
    sys.refresh_cpu_all();
    sys.refresh_memory();

    let cores: Vec<CoreInfo> = sys
        .cpus()
        .iter()
        .map(|c| CoreInfo {
            name: c.name().to_string(),
            usage: c.cpu_usage(),
            frequency: c.frequency(),
        })
        .collect();

    let cpu_brand = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_default();

    let load = System::load_average();
    let process_count = sys.processes().len();

    let mut networks = state.networks.lock().unwrap();
    networks.refresh(true);
    let (mut net_rx_total, mut net_tx_total) = (0u64, 0u64);
    for (_name, data) in networks.iter() {
        net_rx_total += data.total_received();
        net_tx_total += data.total_transmitted();
    }

    let disks = Disks::new_with_refreshed_list();
    let disk_info: Vec<DiskInfo> = disks
        .iter()
        .map(|d| DiskInfo {
            name: d.name().to_string_lossy().to_string(),
            mount: d.mount_point().to_string_lossy().to_string(),
            fs: d.file_system().to_string_lossy().to_string(),
            total: d.total_space(),
            available: d.available_space(),
            removable: d.is_removable(),
        })
        .collect();

    Snapshot {
        cpu_usage: sys.global_cpu_usage(),
        cores,
        cpu_brand,
        load_one: load.one,
        load_five: load.five,
        load_fifteen: load.fifteen,
        mem_total: sys.total_memory(),
        mem_used: sys.used_memory(),
        mem_available: sys.available_memory(),
        swap_total: sys.total_swap(),
        swap_used: sys.used_swap(),
        net_rx_total,
        net_tx_total,
        process_count,
        uptime: System::uptime(),
        host_name: System::host_name().unwrap_or_default(),
        os_version: System::long_os_version().unwrap_or_default(),
        kernel_version: System::kernel_version().unwrap_or_default(),
        disks: disk_info,
    }
}

#[derive(Serialize)]
struct ProcInfo {
    pid: u32,
    parent: Option<u32>,
    name: String,
    cpu: f32,
    memory: u64,
    status: String,
    disk_read: u64,
    disk_write: u64,
    run_time: u64,
}

#[tauri::command]
fn process_list(state: tauri::State<AppState>) -> Vec<ProcInfo> {
    let mut sys = state.sys.lock().unwrap();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let mut list: Vec<ProcInfo> = sys
        .processes()
        .iter()
        .map(|(pid, p)| {
            let du = p.disk_usage();
            ProcInfo {
                pid: pid.as_u32(),
                parent: p.parent().map(|pp| pp.as_u32()),
                name: p.name().to_string_lossy().to_string(),
                cpu: p.cpu_usage(),
                memory: p.memory(),
                status: p.status().to_string(),
                disk_read: du.total_read_bytes,
                disk_write: du.total_written_bytes,
                run_time: p.run_time(),
            }
        })
        .collect();

    // Default ordering: hungriest first.
    list.sort_by(|a, b| {
        b.cpu
            .partial_cmp(&a.cpu)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    list
}

#[tauri::command]
fn kill_process(state: tauri::State<AppState>, pid: u32) -> Result<bool, String> {
    let sys = state.sys.lock().unwrap();
    match sys.process(Pid::from_u32(pid)) {
        Some(p) => Ok(p.kill()),
        None => Err(format!("No process with pid {}", pid)),
    }
}

// ---------------------------------------------------------------------------
// CLI argument handling for `scope <folder>`
// ---------------------------------------------------------------------------

/// Pull the first non-flag argument (skipping the executable name) and resolve
/// it to an absolute, canonical path if possible.
fn parse_path_arg(argv: &[String], cwd: &Path) -> Option<String> {
    for arg in argv.iter().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        let p = cwd.join(arg);
        let resolved = std::fs::canonicalize(&p).unwrap_or(p);
        return Some(resolved.to_string_lossy().to_string());
    }
    None
}

/// Bring a newly launched Finder window forward even when Scope was started
/// by a backgrounded shell process. On macOS, `set_focus` also activates the
/// application ahead of Terminal.
fn present_window(window: &tauri::WebviewWindow) {
    let _ = window.unminimize();
    let _ = window.center();
    let _ = window.show();
    let _ = window.set_focus();
}

// ---------------------------------------------------------------------------
// App bootstrap
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // A CLI launch from an external disk must not pin that disk as our cwd.
    // Resolve the requested folder before moving off the launch directory.
    let launch_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let initial = parse_path_arg(&std::env::args().collect::<Vec<_>>(), &launch_cwd);
    if let Err(error) = std::env::set_current_dir("/") {
        eprintln!("scope: could not release launch directory: {error}");
    }
    let state = AppState {
        // Keep launch cheap. CPU, memory, processes, and network interfaces are
        // populated lazily when the user first opens the Monitor tab.
        sys: Mutex::new(System::new()),
        networks: Mutex::new(Networks::new()),
        initial_paths: Mutex::new(HashMap::new()),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_drag::init())
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // A second `scope <folder>` invocation gets a new Finder window,
            // while the single-instance plugin keeps every window in the same
            // app process (matching Finder's behavior).
            let label = format!("scope-{}", NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed));
            if let Some(path) = parse_path_arg(&argv, Path::new(&_cwd)) {
                app.state::<AppState>()
                    .initial_paths
                    .lock()
                    .unwrap()
                    .insert(label.clone(), path);
            }

            let Some(base_config) = app.config().app.windows.iter().find(|c| c.label == "main")
            else {
                eprintln!("scope: missing main window configuration");
                return;
            };
            let mut config = base_config.clone();
            config.label = label.clone();

            match tauri::WebviewWindowBuilder::from_config(app, &config)
                .and_then(|builder| builder.build())
            {
                Ok(window) => {
                    present_window(&window);
                }
                Err(error) => {
                    app.state::<AppState>()
                        .initial_paths
                        .lock()
                        .unwrap()
                        .remove(&label);
                    eprintln!("scope: could not open a new window: {error}");
                }
            }
        }))
        .manage(state)
        .setup(move |app| {
            if let Some(path) = initial {
                app.state::<AppState>()
                    .initial_paths
                    .lock()
                    .unwrap()
                    .insert("main".to_string(), path);
            }
            if let Some(window) = app.get_webview_window("main") {
                present_window(&window);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            home_dir,
            list_dir,
            open_path,
            reveal_in_finder,
            copy_absolute_paths,
            move_to_trash,
            transfer_operation,
            transfer_paths,
            dir_size,
            initial_path,
            mounted_volumes,
            eject_volume,
            open_full_disk_access_settings,
            read_text_preview,
            stat_path,
            thumbnail,
            system_snapshot,
            process_list,
            kill_process,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Scope");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_scan_cancellation_does_not_return_a_partial_size() {
        let root = TestDir::new();
        std::fs::create_dir(root.0.join("child")).unwrap();
        std::fs::write(root.0.join("child/file"), b"test").unwrap();
        let checks = std::cell::Cell::new(0);
        let result = dir_size_walk_cancellable(&root.0, || {
            checks.set(checks.get() + 1);
            checks.get() > 1
        });
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(dir_size_walk(&root.0) > 0);
    }

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "scope-copy-test-{}-{}",
                std::process::id(),
                NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    #[cfg(unix)]
    fn sparse_sizes_use_allocated_blocks_in_listings_previews_and_folder_totals() {
        use std::io::{Seek, SeekFrom};
        use std::os::unix::fs::MetadataExt;
        let root = TestDir::new();
        let nested = root.0.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let path = nested.join("sparse.raw");
        let mut file = File::create(&path).unwrap();
        let logical = 256_u64 * 1024 * 1024 * 1024;
        file.set_len(logical).unwrap();
        file.seek(SeekFrom::Start(logical - 4096)).unwrap();
        file.write_all(&[1; 4096]).unwrap();
        file.sync_all().unwrap();
        let metadata = file.metadata().unwrap();
        let allocated = metadata.blocks() * 512;
        assert!(allocated < logical / 100);
        let listing = list_dir(nested.to_string_lossy().into_owned()).unwrap();
        assert_eq!(listing.entries[0].size, allocated);
        let preview = stat_path(path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(preview.size, allocated);
        assert_eq!(preview.logical_size, logical);
        std::fs::hard_link(&path, nested.join("hard-link.raw")).unwrap();
        let link = nested.join("symlink.raw");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let expected = allocated
            + std::fs::metadata(&root.0).unwrap().blocks() * 512
            + std::fs::metadata(&nested).unwrap().blocks() * 512
            + std::fs::symlink_metadata(&link).unwrap().blocks() * 512;
        assert_eq!(dir_size_walk(&root.0), expected);
        let output = std::process::Command::new("/usr/bin/du")
            .args(["-sk"])
            .arg(&root.0)
            .output()
            .unwrap();
        assert!(output.status.success());
        let du_kib: u64 = String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(expected.div_ceil(1024), du_kib);
    }

    #[test]
    fn only_external_or_removable_mounts_are_ejectable() {
        assert!(!mount_is_ejectable(Path::new("/"), false));
        assert!(!mount_is_ejectable(
            Path::new("/System/Volumes/Data"),
            false
        ));
        assert!(mount_is_ejectable(Path::new("/Volumes/Backup"), false));
        assert!(mount_is_ejectable(Path::new("/custom/card"), true));
    }

    #[test]
    fn copies_trees_with_progress_and_finder_style_collision_names() {
        let root = TestDir::new();
        let source = root.0.join("Project");
        let destination = root.0.join("Destination");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::create_dir_all(destination.join("Project")).unwrap();
        std::fs::write(source.join("one.txt"), b"one").unwrap();
        std::fs::write(source.join("nested/two.bin"), vec![7_u8; 2 * 1024 * 1024]).unwrap();
        std::fs::write(destination.join("Project/keep.txt"), b"keep").unwrap();

        let mut progress = Vec::new();
        let summary = transfer_paths_blocking(
            vec![source.to_string_lossy().to_string()],
            destination.to_string_lossy().to_string(),
            true,
            |event| progress.push(event),
        )
        .unwrap();

        let copied = PathBuf::from(&summary.destinations[0]);
        assert_eq!(summary.operation, "copy");
        assert!(source.exists());
        assert_eq!(copied.file_name().unwrap(), "Project copy");
        assert_eq!(std::fs::read(copied.join("one.txt")).unwrap(), b"one");
        assert_eq!(
            std::fs::metadata(copied.join("nested/two.bin"))
                .unwrap()
                .len(),
            2 * 1024 * 1024
        );
        assert_eq!(
            std::fs::read(destination.join("Project/keep.txt")).unwrap(),
            b"keep"
        );
        assert_eq!(progress.first().unwrap().phase, "scanning");
        let last = progress.last().unwrap();
        assert_eq!(last.phase, "copying");
        assert_eq!(last.transferred_bytes, last.total_bytes);
        assert_eq!(last.transferred_items, last.total_items);
    }

    #[test]
    fn moves_on_the_same_filesystem_without_leaving_the_source() {
        let root = TestDir::new();
        let source_directory = root.0.join("source");
        let destination = root.0.join("destination");
        let source = source_directory.join("move-me.txt");
        std::fs::create_dir(&source_directory).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(&source, b"move me").unwrap();

        let mut progress = Vec::new();
        let summary = transfer_paths_blocking(
            vec![source.to_string_lossy().to_string()],
            destination.to_string_lossy().to_string(),
            false,
            |event| progress.push(event),
        )
        .unwrap();

        assert_eq!(summary.operation, "move");
        assert!(!source.exists());
        assert_eq!(
            std::fs::read(destination.join("move-me.txt")).unwrap(),
            b"move me"
        );
        assert_eq!(summary.source_directories.len(), 1);
        assert_eq!(progress.last().unwrap().operation, "move");
        assert_eq!(
            progress.last().unwrap().transferred_bytes,
            progress.last().unwrap().total_bytes
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn permission_retry_removes_partial_copy_and_resets_progress() {
        use std::os::unix::fs::PermissionsExt;
        let root = TestDir::new();
        let source = root.0.join("source");
        let destination = root.0.join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        let protected = source.join("private.txt");
        std::fs::write(&protected, b"private").unwrap();
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0)).unwrap();
        let mut retries = 0;
        let mut events = Vec::new();
        let result = transfer_paths_with_finder(
            vec![source.to_string_lossy().into_owned()],
            destination.to_string_lossy().into_owned(),
            true,
            |event| events.push(event),
            |items| {
                let item = &items[0];
                retries += 1;
                assert!(
                    !item.target.exists(),
                    "partial directory must be removed first"
                );
                std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
                std::fs::create_dir(&item.target).unwrap();
                std::fs::copy(&protected, item.target.join("private.txt")).unwrap();
                Ok(vec![item.target.clone()])
            },
        );
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o600)).unwrap();
        let summary = result.unwrap();
        assert_eq!(retries, 1);
        assert_eq!(summary.transferred_bytes, 7);
        assert_eq!(summary.transferred_items, 2);
        assert!(events.iter().any(|event| event.phase == "authorizing"));
        assert_eq!(
            std::fs::read(destination.join("source/private.txt")).unwrap(),
            b"private"
        );
        assert!(source.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unwritable_destination_requests_finder_retry() {
        use std::os::unix::fs::PermissionsExt;
        let root = TestDir::new();
        let source = root.0.join("source.txt");
        let destination = root.0.join("destination");
        let second = root.0.join("second.txt");
        std::fs::write(&second, b"second").unwrap();
        std::fs::write(&source, b"copy me").unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o500)).unwrap();
        let mut retries = 0;
        let result = transfer_paths_with_finder(
            vec![
                source.to_string_lossy().into_owned(),
                second.to_string_lossy().into_owned(),
            ],
            destination.to_string_lossy().into_owned(),
            true,
            |_| {},
            |items| {
                retries += 1;
                assert_eq!(items.len(), 2, "send the whole remaining batch once");
                std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                let targets = items
                    .iter()
                    .map(|item| {
                        assert!(!item.target.exists());
                        std::fs::copy(&item.source, &item.target).unwrap();
                        item.target.clone()
                    })
                    .collect();
                Ok(targets)
            },
        );
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(retries, 1);
        assert_eq!(result.unwrap().destinations.len(), 2);
        assert_eq!(
            std::fs::read(destination.join("second.txt")).unwrap(),
            b"second"
        );
        assert_eq!(
            std::fs::read(destination.join("source.txt")).unwrap(),
            b"copy me"
        );
        assert!(source.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn permission_retry_cancellation_restores_earlier_moves() {
        use std::os::unix::fs::PermissionsExt;
        let root = TestDir::new();
        let source = root.0.join("first.txt");
        let protected = root.0.join("protected");
        let destination = root.0.join("destination");
        std::fs::write(&source, b"keep").unwrap();
        std::fs::create_dir(&protected).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0)).unwrap();
        let result = transfer_paths_with_finder(
            vec![
                source.to_string_lossy().into_owned(),
                protected.to_string_lossy().into_owned(),
            ],
            destination.to_string_lossy().into_owned(),
            false,
            |_| {},
            |_| Err("Transfer canceled in Finder".to_string()),
        );
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.unwrap_err().contains("canceled"));
        assert_eq!(std::fs::read(&source).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
    }

    #[test]
    fn missing_sources_never_request_permission() {
        let root = TestDir::new();
        let result = transfer_paths_with_finder(
            vec![root.0.join("missing").to_string_lossy().into_owned()],
            root.0.to_string_lossy().into_owned(),
            true,
            |_| {},
            |_| panic!("non-permission errors must not launch Finder"),
        );
        assert!(result.is_err());
        assert!(!finder_can_retry(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
        assert!(!finder_can_retry(&std::io::Error::from(
            std::io::ErrorKind::AlreadyExists
        )));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "launches Finder; run manually for macOS integration verification"]
    fn finder_transfer_copies_and_moves_literal_filenames_without_overwriting() {
        let root = TestDir::new();
        let destination = root.0.join("destination");
        std::fs::create_dir(&destination).unwrap();
        let mut items = Vec::new();
        for name in [
            "literal \"quotes\" '$() Unicode é.txt",
            "line\nbreak 🐈.txt",
        ] {
            let source = root.0.join(name);
            std::fs::write(&source, b"original").unwrap();
            items.push(PlannedTransfer {
                target: destination.join(name),
                source,
                source_directory: root.0.clone(),
                operation: TransferOperation::Copy,
                totals: TransferTotals::default(),
                needs_finder: true,
            });
        }
        let copied = finder_transfer(&items).unwrap();
        assert_eq!(copied.len(), 2);
        for item in &items {
            assert_eq!(std::fs::read(&item.target).unwrap(), b"original");
            assert!(item.source.exists());
            std::fs::write(&item.source, b"changed").unwrap();
        }
        let _ = finder_transfer(&items);
        for path in &copied {
            assert_eq!(std::fs::read(path).unwrap(), b"original");
        }
        let move_destination = root.0.join("move destination");
        std::fs::create_dir(&move_destination).unwrap();
        for item in &mut items {
            item.target = move_destination.join(item.source.file_name().unwrap());
            item.operation = TransferOperation::Move;
        }
        let moved = finder_transfer(&items).unwrap();
        assert_eq!(moved.len(), 2);
        for path in moved {
            assert_eq!(std::fs::read(path).unwrap(), b"changed");
        }
        assert!(items.iter().all(|item| !item.source.exists()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "creates and ejects a temporary disk image; run manually on macOS"]
    fn ejects_temporary_disk_image_with_diskutil_and_finder() {
        use std::process::Command;
        let root = TestDir::new();
        let image = root.0.join("eject-test.dmg");
        let name = format!("ScopeEjectTest{}", std::process::id());
        let mount = PathBuf::from("/Volumes").join(&name);
        assert!(!mount.exists());
        assert!(Command::new("/usr/bin/hdiutil")
            .args(["create", "-size", "32m", "-fs", "HFS+", "-volname", &name])
            .arg(&image)
            .output()
            .unwrap()
            .status
            .success());
        for use_finder in [false, true] {
            assert!(Command::new("/usr/bin/hdiutil")
                .args(["attach", "-nobrowse"])
                .arg(&image)
                .output()
                .unwrap()
                .status
                .success());
            let result = if use_finder {
                let output = Command::new("/usr/bin/osascript")
                    .args(["-e", include_str!("finder_eject.applescript")])
                    .arg(&mount)
                    .output()
                    .unwrap();
                if output.status.success() {
                    Ok(())
                } else {
                    Err(String::from_utf8_lossy(&output.stderr).into_owned())
                }
            } else {
                eject_volume_blocking(mount.to_str().unwrap())
            };
            let unmounted = !mount.exists();
            if !unmounted {
                let _ = Command::new("/usr/bin/hdiutil")
                    .arg("detach")
                    .arg(&mount)
                    .output();
            }
            assert!(result.is_ok(), "eject failed: {result:?}");
            assert!(unmounted, "eject reported success but disk stayed mounted");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn eject_validation_rejects_startup_disk_and_subdirectories() {
        let root = TestDir::new();
        let requested = std::fs::canonicalize(&root.0).unwrap();
        let mut fields = plist::Dictionary::new();
        fields.insert("MountPoint".into(), plist::Value::String("/".into()));
        fields.insert("Ejectable".into(), plist::Value::Boolean(true));
        let info = plist::Value::Dictionary(fields);
        assert!(validated_eject_mount(Path::new("/"), &info).is_err());
        assert!(validated_eject_mount(&requested, &info).is_err());
    }

    #[test]
    fn resolves_cli_paths_before_releasing_launch_directory() {
        let root = TestDir::new();
        let folder = root.0.join("folder");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(
            parse_path_arg(&["scope".into(), "folder".into()], &root.0),
            Some(
                std::fs::canonicalize(folder)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            )
        );
    }

    #[test]
    fn rejects_transferring_a_directory_into_itself() {
        let root = TestDir::new();
        let source = root.0.join("source");
        let destination = source.join("inside");
        std::fs::create_dir_all(&destination).unwrap();

        let result = transfer_paths_blocking(
            vec![source.to_string_lossy().to_string()],
            destination.to_string_lossy().to_string(),
            false,
            |_| {},
        );

        assert!(result.unwrap_err().contains("into itself"));
    }
}
