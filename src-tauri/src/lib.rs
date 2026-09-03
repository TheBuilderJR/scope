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
use std::sync::Mutex;
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
            sym_meta
        };
        let is_dir = meta
            .as_ref()
            .map(|m| m.is_dir())
            .unwrap_or_else(|| p.is_dir());
        let size = if is_dir {
            0
        } else {
            meta.as_ref().map(|m| m.len()).unwrap_or(0)
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
    mut emit: F,
) -> Result<TransferSummary, String>
where
    F: FnMut(TransferProgress),
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
        measure_transfer(&source, &mut path_totals)?;
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
    for item in &planned {
        state.operation = item.operation.as_str();
        state.current = item.source.to_string_lossy().to_string();
        let result = match item.operation {
            TransferOperation::Copy => {
                let mut root_created = false;
                let result = copy_entry(
                    &item.source,
                    &item.target,
                    &mut state,
                    &mut root_created,
                    true,
                );
                if root_created {
                    copied_targets.push(item.target.clone());
                }
                result
            }
            TransferOperation::Move => {
                let result = std::fs::rename(&item.source, &item.target)
                    .map_err(|e| format!("{}: {e}", item.source.display()));
                if result.is_ok() {
                    moved_paths.push((item.source.clone(), item.target.clone()));
                    state.transferred_bytes =
                        state.transferred_bytes.saturating_add(item.totals.bytes);
                    state.transferred_items =
                        state.transferred_items.saturating_add(item.totals.items);
                    state.update(true);
                }
                result
            }
        };

        if let Err(mut error) = result {
            // Roll back only paths created by this operation, so a failed
            // multi-item drop is not left half transferred.
            for copied in copied_targets.iter().rev() {
                remove_copied_path(copied);
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
        destinations: planned
            .iter()
            .map(|item| item.target.to_string_lossy().to_string())
            .collect(),
        source_directories: source_directories.into_iter().collect(),
    })
}

fn measure_transfer(path: &Path, totals: &mut TransferTotals) -> Result<(), String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    totals.items = totals.items.saturating_add(1);
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        let entries = std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", path.display()))?;
            measure_transfer(&entry.path(), totals)?;
        }
    } else if metadata.is_file() {
        totals.bytes = totals.bytes.saturating_add(metadata.len());
    } else {
        return Err(format!("Unsupported file type: {}", path.display()));
    }
    Ok(())
}

fn copy_entry<F>(
    source: &Path,
    target: &Path,
    state: &mut TransferState<'_, F>,
    root_created: &mut bool,
    is_root: bool,
) -> Result<(), String>
where
    F: FnMut(TransferProgress),
{
    let metadata =
        std::fs::symlink_metadata(source).map_err(|e| format!("{}: {e}", source.display()))?;
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
        std::fs::create_dir(target).map_err(|e| format!("{}: {e}", target.display()))?;
        if is_root {
            *root_created = true;
        }
        let entries =
            std::fs::read_dir(source).map_err(|e| format!("{}: {e}", source.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", source.display()))?;
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
            .map_err(|e| format!("{}: {e}", target.display()))?;
        state.transferred_items = state.transferred_items.saturating_add(1);
        state.update(false);
        return Ok(());
    }

    if metadata.is_file() {
        let mut input = File::open(source).map_err(|e| format!("{}: {e}", source.display()))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .map_err(|e| format!("{}: {e}", target.display()))?;
        if is_root {
            *root_created = true;
        }
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let count = input
                .read(&mut buffer)
                .map_err(|e| format!("{}: {e}", source.display()))?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .map_err(|e| format!("{}: {e}", target.display()))?;
            state.transferred_bytes = state.transferred_bytes.saturating_add(count as u64);
            state.update(false);
        }
        output
            .flush()
            .map_err(|e| format!("{}: {e}", target.display()))?;
        std::fs::set_permissions(target, metadata.permissions())
            .map_err(|e| format!("{}: {e}", target.display()))?;
        state.transferred_items = state.transferred_items.saturating_add(1);
        state.update(false);
        return Ok(());
    }

    Err(format!("Unsupported file type: {}", source.display()))
}

fn copy_symlink(source: &Path, target: &Path) -> Result<(), String> {
    let link = std::fs::read_link(source).map_err(|e| format!("{}: {e}", source.display()))?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(link, target).map_err(|e| format!("{}: {e}", target.display()))
    }
    #[cfg(windows)]
    {
        if source.is_dir() {
            std::os::windows::fs::symlink_dir(link, target)
                .map_err(|e| format!("{}: {e}", target.display()))
        } else {
            std::os::windows::fs::symlink_file(link, target)
                .map_err(|e| format!("{}: {e}", target.display()))
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

fn remove_copied_path(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// Recursively sum the on-disk size of a directory's contents (like Finder's
/// "Calculate all sizes"). Symlinks are not followed, to avoid cycles and
/// double-counting. Runs on a blocking thread since it walks the whole subtree.
#[tauri::command]
async fn dir_size(path: String) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || dir_size_walk(Path::new(&path)))
        .await
        .map_err(|e| e.to_string())
}

fn dir_size_walk(path: &Path) -> u64 {
    let mut total: u64 = 0;
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in rd.flatten() {
        // file_type()/metadata() on a DirEntry do not follow symlinks.
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        } else if ft.is_dir() {
            total = total.saturating_add(dir_size_walk(&entry.path()));
        } else if let Ok(m) = entry.metadata() {
            total = total.saturating_add(m.len());
        }
    }
    total
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
}

/// Return the local, user-visible mounted volumes that macOS exposes in
/// Finder. `sysinfo` deliberately omits hidden APFS/system volumes here.
#[tauri::command]
fn mounted_volumes() -> Vec<MountedVolume> {
    let disks = Disks::new_with_refreshed_list();
    let mut volumes: Vec<MountedVolume> = disks
        .iter()
        .map(|disk| MountedVolume {
            name: disk.name().to_string_lossy().to_string(),
            path: disk.mount_point().to_string_lossy().to_string(),
            removable: disk.is_removable(),
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
        is_symlink: sym.map(|m| m.file_type().is_symlink()).unwrap_or(false),
        size: if is_dir { 0 } else { meta.len() },
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
fn parse_path_arg(argv: &[String]) -> Option<String> {
    for arg in argv.iter().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        let p = PathBuf::from(arg);
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
            if let Some(path) = parse_path_arg(&argv) {
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
        .setup(|app| {
            // Record any folder passed on the command line at first launch.
            let args: Vec<String> = std::env::args().collect();
            if let Some(path) = parse_path_arg(&args) {
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
            move_to_trash,
            transfer_operation,
            transfer_paths,
            dir_size,
            initial_path,
            mounted_volumes,
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
