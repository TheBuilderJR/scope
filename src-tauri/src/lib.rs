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
        let created = follow_meta
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
/// rather than deleting them outright.
#[tauri::command]
fn move_to_trash(paths: Vec<String>) -> Result<(), String> {
    trash::delete_all(&paths).map_err(|e| e.to_string())
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
fn initial_path(state: tauri::State<AppState>) -> Option<String> {
    state.initial_path.lock().unwrap().clone()
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
    let has_nul = buf.iter().any(|&b| b == 0);
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
        let key = thumb_key(&path, mtime, size);

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
        .plugin(tauri_plugin_drag::init())
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
            move_to_trash,
            dir_size,
            initial_path,
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
