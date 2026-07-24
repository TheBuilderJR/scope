// Scope - a combined Finder-style file browser and Activity-Monitor / htop-style
// system monitor, built with Tauri v2.
//
// The Rust side exposes two families of commands to the webview:
//   * file browsing  (list_dir, open_path, reveal_in_finder, home_dir, ...)
//   * system metrics (system_snapshot, process_list, kill_process)
// plus initial_path() so the GUI can honour `scope <folder>` from the CLI.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use serde::Serialize;
use sysinfo::{Disks, Networks, Pid, ProcessesToUpdate, System};
use tauri::{Emitter, Manager};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct AppState {
    sys: Mutex<System>,
    networks: Mutex<Networks>,
    initial_path: Mutex<Option<String>>,
}

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
        Some("mp4") | Some("mov") | Some("mkv") | Some("avi") => "Movie",
        Some("mp3") | Some("wav") | Some("flac") | Some("aac") | Some("m4a") => "Audio",
        Some("zip") | Some("gz") | Some("tar") | Some("bz2") | Some("xz") | Some("7z") => {
            "Archive"
        }
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
        // Use symlink_metadata so we can flag symlinks, but fall back to
        // metadata (following the link) for the is_dir / size determination.
        let sym_meta = item.metadata().ok();
        let is_symlink = sym_meta
            .as_ref()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        let follow_meta = std::fs::metadata(&p).ok();
        let is_dir = follow_meta
            .as_ref()
            .map(|m| m.is_dir())
            .unwrap_or_else(|| p.is_dir());
        let size = if is_dir {
            0
        } else {
            follow_meta.as_ref().map(|m| m.len()).unwrap_or(0)
        };
        let modified = follow_meta
            .as_ref()
            .and_then(|m| m.modified().ok())
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
fn initial_path(state: tauri::State<AppState>) -> Option<String> {
    state.initial_path.lock().unwrap().clone()
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
    list.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal));
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

// ---------------------------------------------------------------------------
// App bootstrap
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let state = AppState {
        sys: Mutex::new(System::new_all()),
        networks: Mutex::new(Networks::new_with_refreshed_list()),
        initial_path: Mutex::new(None),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // A second `scope <folder>` invocation: forward the path to the
            // already-running window instead of launching a new one.
            if let Some(path) = parse_path_arg(&argv) {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.set_focus();
                    let _ = win.emit("scope://open-path", path);
                }
            } else if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_focus();
            }
        }))
        .manage(state)
        .setup(|app| {
            // Prime the CPU counters so the first snapshot isn't all zeros.
            {
                let state = app.state::<AppState>();
                let mut sys = state.sys.lock().unwrap();
                sys.refresh_cpu_all();
            }
            // Record any folder passed on the command line at first launch.
            let args: Vec<String> = std::env::args().collect();
            if let Some(path) = parse_path_arg(&args) {
                *app.state::<AppState>().initial_path.lock().unwrap() = Some(path);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            home_dir,
            list_dir,
            open_path,
            reveal_in_finder,
            initial_path,
            system_snapshot,
            process_list,
            kill_process,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Scope");
}
