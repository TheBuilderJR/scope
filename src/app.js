// Scope frontend. Talks to the Rust backend via the global Tauri API
// (enabled with `withGlobalTauri` in tauri.conf.json).

const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

function fmtBytes(n) {
  if (n === 0 || n == null) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  const v = n / Math.pow(1024, i);
  return `${v >= 100 || i === 0 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

function fmtRate(bytesPerSec) {
  return `${fmtBytes(bytesPerSec)}/s`;
}

function fmtDate(secs) {
  if (!secs) return "—";
  return new Date(secs * 1000).toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function fmtDuration(secs) {
  secs = Math.floor(secs);
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (d > 0) return `${d}d ${h}h ${m}m`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

function fmtMode(mode) {
  if (mode == null) return "—";
  const perms = ["---", "--x", "-w-", "-wx", "r--", "r-x", "rw-", "rwx"];
  const o = mode & 0o777;
  return perms[(o >> 6) & 7] + perms[(o >> 3) & 7] + perms[o & 7];
}

function usageClass(pct) {
  if (pct >= 80) return "u-high";
  if (pct >= 50) return "u-med";
  return "u-low";
}

function ext(path) {
  const m = /\.([^./\\]+)$/.exec(path);
  return m ? m[1].toLowerCase() : "";
}

const IMG = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "tiff", "heic"];
const VID = ["mp4", "mov", "mkv", "webm", "m4v", "avi"];
const AUD = ["mp3", "wav", "flac", "aac", "m4a", "ogg"];

function iconFor(entry) {
  if (entry.kind === "Application") return "🅰";
  if (entry.is_dir) return "📁";
  switch (entry.kind) {
    case "Image":
      return "🖼";
    case "Movie":
      return "🎬";
    case "Audio":
      return "🎵";
    case "PDF Document":
      return "📕";
    case "Archive":
      return "🗜";
    case "Rust Source":
    case "JavaScript":
    case "TypeScript":
    case "Python Source":
    case "Go Source":
    case "C Source":
    case "C++ Source":
    case "Shell Script":
      return "📜";
    default:
      return "📄";
  }
}

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

// ---------------------------------------------------------------------------
// Inline SVG icons (Feather-style strokes, inherit currentColor)
// ---------------------------------------------------------------------------

const ICONS = {
  home: '<path d="M3 9.5 12 3l9 6.5"/><path d="M5 8.8V20a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8.8"/><path d="M9.5 21v-6h5v6"/>',
  desktop:
    '<rect x="2.5" y="3.5" width="19" height="13" rx="2"/><path d="M8 20.5h8"/><path d="M12 16.5v4"/>',
  documents:
    '<path d="M13.5 2.5H6.5a2 2 0 0 0-2 2v15a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2V8.5z"/><path d="M13.5 2.5v6h6"/><path d="M8.5 13h7"/><path d="M8.5 16.5h7"/>',
  downloads:
    '<path d="M20.5 15v4a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 19v-4"/><path d="M7.5 10.5 12 15l4.5-4.5"/><path d="M12 15V3"/>',
  applications:
    '<rect x="3.5" y="3.5" width="7" height="7" rx="1.5"/><rect x="13.5" y="3.5" width="7" height="7" rx="1.5"/><rect x="3.5" y="13.5" width="7" height="7" rx="1.5"/><rect x="13.5" y="13.5" width="7" height="7" rx="1.5"/>',
  drive:
    '<path d="M2.5 12 5.4 5.5A2 2 0 0 1 7.2 4.3h9.6a2 2 0 0 1 1.8 1.2L21.5 12v6a2 2 0 0 1-2 2h-15a2 2 0 0 1-2-2z"/><path d="M2.5 12h19"/><path d="M6.5 16h.01"/><path d="M10 16h.01"/>',
  back: '<path d="M15 18.5 8.5 12 15 5.5"/>',
  forward: '<path d="M9 5.5 15.5 12 9 18.5"/>',
  up: '<path d="M12 20V5"/><path d="M5.5 11 12 4.5 18.5 11"/>',
  list: '<path d="M8 6.5h12"/><path d="M8 12h12"/><path d="M8 17.5h12"/><circle cx="4" cy="6.5" r="1.1"/><circle cx="4" cy="12" r="1.1"/><circle cx="4" cy="17.5" r="1.1"/>',
  columns:
    '<rect x="3.5" y="4" width="17" height="16" rx="2"/><path d="M9 4v16"/><path d="M15 4v16"/>',
};

function svg(name, cls = "icon-svg") {
  return `<svg class="${cls}" viewBox="0 0 24 24" aria-hidden="true">${ICONS[name] || ""}</svg>`;
}

// ---------------------------------------------------------------------------
// Lazy QuickLook thumbnails (like Finder), disk-cached in the backend
// ---------------------------------------------------------------------------

const thumbCache = new Map(); // path -> asset URL | null (null = no thumbnail)
const thumbWaiting = [];
let thumbActive = 0;
const THUMB_MAX = 6;

const thumbObserver = new IntersectionObserver(
  (entries) => {
    for (const ent of entries) {
      if (ent.isIntersecting) {
        const el = ent.target;
        thumbObserver.unobserve(el);
        queueThumb(el, el.dataset.thumbPath, 64);
      }
    }
  },
  { rootMargin: "300px" }
);

function thumbEligible(entry) {
  // Files, plus .app bundles (QuickLook renders their app icon).
  return !entry.is_dir || entry.kind === "Application";
}

// Attach a thumbnail to an icon slot: cached -> apply now, else observe lazily.
function attachThumb(icoEl, entry) {
  if (!thumbEligible(entry)) return;
  const cached = thumbCache.get(entry.path);
  if (cached) {
    icoEl.innerHTML = `<img class="thumb" src="${cached}" alt="">`;
  } else if (cached === undefined) {
    icoEl.dataset.thumbPath = entry.path;
    thumbObserver.observe(icoEl);
  }
}

function queueThumb(el, path, size) {
  if (!path) return;
  if (thumbCache.has(path)) {
    const url = thumbCache.get(path);
    if (url && el.isConnected) el.innerHTML = `<img class="thumb" src="${url}" alt="">`;
    return;
  }
  thumbWaiting.push({ el, path, size });
  pumpThumbs();
}

function pumpThumbs() {
  while (thumbActive < THUMB_MAX && thumbWaiting.length) {
    const job = thumbWaiting.shift();
    thumbActive++;
    invoke("thumbnail", { path: job.path, size: job.size })
      .then((res) => {
        const url = res ? convertFileSrc(res) : null;
        thumbCache.set(job.path, url);
        if (url && job.el.isConnected && job.el.dataset.thumbPath === job.path) {
          job.el.innerHTML = `<img class="thumb" src="${url}" alt="">`;
        }
      })
      .catch(() => thumbCache.set(job.path, null))
      .finally(() => {
        thumbActive--;
        pumpThumbs();
      });
  }
}

// ---------------------------------------------------------------------------
// View switching
// ---------------------------------------------------------------------------

const views = { finder: document.getElementById("finder"), monitor: document.getElementById("monitor") };
let activeView = "finder";

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => switchView(tab.dataset.view));
});

function switchView(name) {
  activeView = name;
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t.dataset.view === name));
  Object.entries(views).forEach(([k, el]) => el.classList.toggle("active", k === name));
  if (name === "monitor") {
    resetNetBaseline();
    pollMonitor();
  }
}

// ===========================================================================
// FINDER
// ===========================================================================

const fileRows = document.getElementById("file-rows");
const breadcrumbEl = document.getElementById("breadcrumb");
const favoritesEl = document.getElementById("favorites");
const finderStatus = document.getElementById("finder-status");
const finderEmpty = document.getElementById("finder-empty");
const finderSearch = document.getElementById("finder-search");
const listViewEl = document.getElementById("list-view");
const columnViewEl = document.getElementById("column-view");
const previewEl = document.getElementById("preview");

let viewMode = "list"; // "list" | "columns"
let hiddenShown = false; // toggled with ⌘⇧. like Finder
let currentDir = null;
let currentEntries = [];
let selectedPath = null;
let selectedEntry = null;
let sortKey = "name";
let sortAsc = true;
let columns = []; // [{ path, listing, selectedPath }]
const history = [];
let historyIndex = -1;
let HOME = "/";

async function initFinder() {
  HOME = await invoke("home_dir");
  buildFavorites();
  const initial = await invoke("initial_path");
  await navigate(initial || HOME, true);
}

function buildFavorites() {
  const favs = [
    { name: "Home", path: HOME, ico: "home" },
    { name: "Desktop", path: `${HOME}/Desktop`, ico: "desktop" },
    { name: "Documents", path: `${HOME}/Documents`, ico: "documents" },
    { name: "Downloads", path: `${HOME}/Downloads`, ico: "downloads" },
    { name: "Applications", path: "/Applications", ico: "applications" },
    { name: "Root", path: "/", ico: "drive" },
  ];
  favoritesEl.innerHTML = "";
  favs.forEach((f) => {
    const li = document.createElement("li");
    li.dataset.path = f.path;
    li.innerHTML = `<span class="ico">${svg(f.ico)}</span><span>${f.name}</span>`;
    li.addEventListener("click", () => navigate(f.path));
    favoritesEl.appendChild(li);
  });
}

async function navigate(path, replace = false) {
  let listing;
  try {
    listing = await invoke("list_dir", { path });
  } catch (e) {
    finderStatus.textContent = `⚠ ${e}`;
    return;
  }
  currentDir = listing.path;
  currentEntries = listing.entries;
  selectedPath = null;
  selectedEntry = null;

  if (!replace) {
    history.splice(historyIndex + 1);
    history.push(currentDir);
    historyIndex = history.length - 1;
  } else if (historyIndex < 0) {
    history.push(currentDir);
    historyIndex = 0;
  } else {
    history[historyIndex] = currentDir;
  }

  updateNavButtons();
  renderBreadcrumb(currentDir);
  highlightFavorite();

  if (viewMode === "columns") {
    columns = [{ path: currentDir, listing, selectedPath: null }];
    renderColumns();
  } else {
    renderFiles();
  }
  clearPreview();
}

function highlightFavorite() {
  favoritesEl.querySelectorAll("li").forEach((li) => li.classList.toggle("active", li.dataset.path === currentDir));
}

function updateNavButtons() {
  document.getElementById("nav-back").disabled = historyIndex <= 0;
  document.getElementById("nav-forward").disabled = historyIndex >= history.length - 1;
}

function renderBreadcrumb(fullPath) {
  breadcrumbEl.innerHTML = "";
  const parts = fullPath.split("/").filter(Boolean);
  const addCrumb = (label, path, isCurrent) => {
    const span = document.createElement("span");
    span.className = "crumb" + (isCurrent ? " current" : "");
    span.textContent = label;
    span.addEventListener("click", () => navigate(path));
    breadcrumbEl.appendChild(span);
  };
  addCrumb("/", "/", parts.length === 0);
  let acc = "";
  parts.forEach((part, i) => {
    acc += "/" + part;
    const sep = document.createElement("span");
    sep.className = "crumb-sep";
    sep.textContent = "›";
    breadcrumbEl.appendChild(sep);
    addCrumb(part, acc, i === parts.length - 1);
  });
}

function filterEntries(entries) {
  const q = finderSearch.value.trim().toLowerCase();
  return entries.filter((e) => {
    if (!hiddenShown && e.hidden) return false;
    if (q && !e.name.toLowerCase().includes(q)) return false;
    return true;
  });
}

// ---- List view ----

function sortedFiltered() {
  const list = filterEntries(currentEntries);
  const dir = sortAsc ? 1 : -1;
  list.sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    let cmp = 0;
    switch (sortKey) {
      case "size":
        cmp = a.size - b.size;
        break;
      case "kind":
        cmp = a.kind.localeCompare(b.kind);
        break;
      case "modified":
        cmp = (a.modified || 0) - (b.modified || 0);
        break;
      default:
        cmp = a.name.toLowerCase().localeCompare(b.name.toLowerCase());
    }
    return cmp * dir;
  });
  return list;
}

function renderFiles() {
  const list = sortedFiltered();
  thumbObserver.disconnect();
  fileRows.innerHTML = "";
  finderEmpty.classList.toggle("hidden", list.length > 0);

  const frag = document.createDocumentFragment();
  for (const e of list) {
    const tr = document.createElement("tr");
    if (e.hidden) tr.className = "row-hidden";
    tr.dataset.path = e.path;
    tr.innerHTML = `
      <td><div class="name-cell"><span class="ico">${iconFor(e)}</span><span class="txt">${escapeHtml(e.name)}${
      e.is_symlink ? " ↪" : ""
    }</span></div></td>
      <td class="size">${e.is_dir ? "—" : fmtBytes(e.size)}</td>
      <td class="kind">${e.kind}</td>
      <td class="date">${fmtDate(e.modified)}</td>`;
    tr.addEventListener("click", () => {
      selectRowList(tr, e);
    });
    tr.addEventListener("dblclick", () => openEntry(e));
    if (e.path === selectedPath) tr.classList.add("selected");
    attachThumb(tr.querySelector(".ico"), e);
    frag.appendChild(tr);
  }
  fileRows.appendChild(frag);

  const folders = list.filter((e) => e.is_dir).length;
  finderStatus.textContent = `${list.length} items · ${folders} folders · ${list.length - folders} files`;
}

function selectRowList(tr, entry) {
  fileRows.querySelectorAll("tr.selected").forEach((r) => r.classList.remove("selected"));
  tr.classList.add("selected");
  selectedPath = entry.path;
  selectedEntry = entry;
  showPreview(entry);
}

function openEntry(entry) {
  if (entry.is_dir && entry.kind !== "Application") {
    navigate(entry.path);
  } else {
    invoke("open_path", { path: entry.path }).catch((e) => (finderStatus.textContent = `⚠ ${e}`));
  }
}

// ---- Column (Miller) view ----

function renderColumns() {
  thumbObserver.disconnect();
  columnViewEl.innerHTML = "";
  columns.forEach((col, idx) => {
    const colEl = document.createElement("div");
    colEl.className = "mcol";
    const list = filterEntries(col.listing.entries).sort((a, b) => {
      if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
      return a.name.toLowerCase().localeCompare(b.name.toLowerCase());
    });
    for (const e of list) {
      const item = document.createElement("div");
      item.className = "mcol-item" + (e.hidden ? " row-hidden" : "") + (e.path === col.selectedPath ? " selected" : "");
      item.innerHTML = `<span class="ico">${iconFor(e)}</span><span class="txt">${escapeHtml(e.name)}</span>${
        e.is_dir && e.kind !== "Application" ? '<span class="chev">›</span>' : ""
      }`;
      item.addEventListener("click", () => columnSelect(idx, e));
      item.addEventListener("dblclick", () => {
        if (!(e.is_dir && e.kind !== "Application")) openEntry(e);
      });
      attachThumb(item.querySelector(".ico"), e);
      colEl.appendChild(item);
    }
    columnViewEl.appendChild(colEl);
  });
  // scroll to reveal the newest column
  columnViewEl.scrollLeft = columnViewEl.scrollWidth;
}

async function columnSelect(colIndex, entry) {
  columns = columns.slice(0, colIndex + 1);
  columns[colIndex].selectedPath = entry.path;
  selectedPath = entry.path;
  selectedEntry = entry;

  if (entry.is_dir && entry.kind !== "Application") {
    try {
      const listing = await invoke("list_dir", { path: entry.path });
      columns.push({ path: entry.path, listing, selectedPath: null });
      currentDir = entry.path;
      renderBreadcrumb(entry.path);
      highlightFavorite();
    } catch (e) {
      finderStatus.textContent = `⚠ ${e}`;
    }
  }
  renderColumns();
  showPreview(entry);
}

// ---- Preview pane ----

function clearPreview() {
  previewEl.innerHTML = '<div class="preview-empty">Select an item to preview</div>';
}

// A larger QuickLook thumbnail for the preview pane, falling back to an icon.
async function bigThumb(entry) {
  const cached = thumbCache.get(entry.path);
  if (cached) return `<img class="pv-thumb" src="${cached}" alt="">`;
  try {
    const res = await invoke("thumbnail", { path: entry.path, size: 256 });
    if (res) return `<img class="pv-thumb" src="${convertFileSrc(res)}" alt="">`;
  } catch {
    /* fall through */
  }
  return `<div class="pv-bigicon">${iconFor(entry)}</div>`;
}

async function showPreview(entry) {
  let info;
  try {
    info = await invoke("stat_path", { path: entry.path });
  } catch (e) {
    previewEl.innerHTML = `<div class="preview-empty">⚠ ${escapeHtml(String(e))}</div>`;
    return;
  }
  // Guard against races: only render if this is still the selected item.
  if (selectedPath !== entry.path) return;

  const e = ext(entry.path);
  let media = "";

  if (!entry.is_dir && IMG.includes(e)) {
    media = `<img class="pv-thumb" src="${convertFileSrc(entry.path)}" alt="" />`;
  } else if (!entry.is_dir && VID.includes(e)) {
    media = `<video class="pv-media" controls preload="metadata" src="${convertFileSrc(entry.path)}"></video>`;
  } else if (!entry.is_dir && AUD.includes(e)) {
    media = `<audio class="pv-media" controls src="${convertFileSrc(entry.path)}"></audio>`;
  } else if (!entry.is_dir && e === "pdf") {
    media = `<iframe class="pv-media" style="height:300px" src="${convertFileSrc(entry.path)}"></iframe>`;
  } else if (!entry.is_dir) {
    // Try a text preview; fall back to a QuickLook thumbnail, then an icon.
    try {
      const tp = await invoke("read_text_preview", { path: entry.path });
      if (selectedPath !== entry.path) return;
      if (tp.is_text) {
        media = `<pre class="pv-text">${escapeHtml(tp.text)}${tp.truncated ? "\n…" : ""}</pre>`;
      } else {
        media = await bigThumb(entry);
      }
    } catch {
      media = await bigThumb(entry);
    }
    if (selectedPath !== entry.path) return;
  } else {
    media = `<div class="pv-bigicon">${iconFor(entry)}</div>`;
  }

  const rows = [];
  rows.push(["Kind", info.kind]);
  if (info.is_dir) rows.push(["Items", info.item_count != null ? info.item_count : "—"]);
  else rows.push(["Size", fmtBytes(info.size)]);
  rows.push(["Created", fmtDate(info.created)]);
  rows.push(["Modified", fmtDate(info.modified)]);
  rows.push(["Accessed", fmtDate(info.accessed)]);
  rows.push(["Permissions", fmtMode(info.mode)]);
  rows.push(["Where", info.path]);

  previewEl.innerHTML = `
    ${media}
    <div class="pv-name">${escapeHtml(info.name)}</div>
    <div class="pv-kind">${info.kind}</div>
    <div class="pv-info">
      ${rows
        .map(([k, v]) => `<div class="kv"><span>${k}</span><span title="${escapeHtml(String(v))}">${escapeHtml(String(v))}</span></div>`)
        .join("")}
    </div>
    <div class="pv-actions">
      <button id="pv-open">Open</button>
      <button id="pv-reveal">Reveal</button>
    </div>`;
  document.getElementById("pv-open").addEventListener("click", () => openEntry(entry));
  document.getElementById("pv-reveal").addEventListener("click", () => invoke("reveal_in_finder", { path: entry.path }));
}

// ---- View mode toggle ----

function setViewMode(mode) {
  viewMode = mode;
  document.getElementById("view-list").classList.toggle("active", mode === "list");
  document.getElementById("view-columns").classList.toggle("active", mode === "columns");
  listViewEl.classList.toggle("hidden", mode !== "list");
  columnViewEl.classList.toggle("hidden", mode !== "columns");
  if (mode === "columns") {
    columns = [{ path: currentDir, listing: { entries: currentEntries }, selectedPath: null }];
    renderColumns();
  } else {
    renderFiles();
  }
}

document.getElementById("view-list").addEventListener("click", () => setViewMode("list"));
document.getElementById("view-columns").addEventListener("click", () => setViewMode("columns"));

// Finder controls
document.getElementById("nav-back").addEventListener("click", () => {
  if (historyIndex > 0) {
    historyIndex--;
    navigate(history[historyIndex], "silent");
  }
});
document.getElementById("nav-forward").addEventListener("click", () => {
  if (historyIndex < history.length - 1) {
    historyIndex++;
    navigate(history[historyIndex], "silent");
  }
});
document.getElementById("nav-up").addEventListener("click", () => {
  const parent = (currentDir || "/").replace(/\/[^/]+\/?$/, "") || "/";
  navigate(parent);
});
document.getElementById("nav-home").addEventListener("click", () => navigate(HOME));
document.getElementById("reveal-btn").addEventListener("click", () => {
  invoke("reveal_in_finder", { path: selectedPath || currentDir });
});
finderSearch.addEventListener("input", () => (viewMode === "columns" ? renderColumns() : renderFiles()));

// Toggle hidden files with the Finder hotkey ⌘⇧. (Cmd+Shift+Period)
document.addEventListener("keydown", (ev) => {
  if (ev.metaKey && ev.shiftKey && ev.code === "Period") {
    ev.preventDefault();
    hiddenShown = !hiddenShown;
    if (viewMode === "columns") renderColumns();
    else renderFiles();
  }
});

// Swap the toolbar glyphs for crisp SVG icons.
document.getElementById("nav-back").innerHTML = svg("back");
document.getElementById("nav-forward").innerHTML = svg("forward");
document.getElementById("nav-up").innerHTML = svg("up");
document.getElementById("nav-home").innerHTML = svg("home");
document.getElementById("view-list").innerHTML = svg("list");
document.getElementById("view-columns").innerHTML = svg("columns");

document.querySelectorAll(".filelist th[data-sort]").forEach((th) => {
  th.addEventListener("click", () => {
    const key = th.dataset.sort;
    if (sortKey === key) sortAsc = !sortAsc;
    else {
      sortKey = key;
      sortAsc = true;
    }
    renderFiles();
  });
});

// ===========================================================================
// MONITOR
// ===========================================================================

const POLL_MS = 1500;
const HIST = 120; // ~3 minutes of history at POLL_MS
let netPrev = null; // { rx, tx, t }
let procSortKey = "cpu";
let procSortAsc = false;
let lastProcs = [];

const cpuHist = [];
const memHist = [];
const netDownHist = [];
const netUpHist = [];

function pushHist(arr, v) {
  arr.push(v);
  if (arr.length > HIST) arr.shift();
}

function resetNetBaseline() {
  netPrev = null;
}

async function pollMonitor() {
  if (activeView !== "monitor") return;
  try {
    const [snap, procs] = await Promise.all([invoke("system_snapshot"), maybeProcs()]);
    renderSnapshot(snap);
    if (procs) {
      lastProcs = procs;
      renderProcs();
    }
  } catch (e) {
    console.error(e);
  }
}

function maybeProcs() {
  if (document.getElementById("proc-pause").checked) return Promise.resolve(null);
  return invoke("process_list");
}

function renderSnapshot(s) {
  // ----- CPU -----
  document.getElementById("cpu-total").textContent = `${s.cpu_usage.toFixed(1)}%`;
  document.getElementById("cpu-brand").textContent = s.cpu_brand;
  pushHist(cpuHist, s.cpu_usage);

  const coresEl = document.getElementById("cpu-cores");
  coresEl.innerHTML = "";
  s.cores.forEach((c, i) => {
    const div = document.createElement("div");
    div.className = "core";
    div.innerHTML = `<span class="core-id">${i}</span>
      <div class="usage-bar"><div class="usage-fill ${usageClass(c.usage)}" style="width:${Math.min(
      100,
      c.usage
    ).toFixed(0)}%"></div></div>
      <span class="core-pct">${c.usage.toFixed(0)}%</span>`;
    coresEl.appendChild(div);
  });
  document.getElementById("cpu-mini").innerHTML = `<span>${s.cores.length} cores</span><span>${(
    s.cores.reduce((a, c) => a + c.frequency, 0) /
    (s.cores.length || 1) /
    1000
  ).toFixed(2)} GHz avg</span>`;

  // ----- Memory -----
  const memPct = s.mem_total ? (s.mem_used / s.mem_total) * 100 : 0;
  pushHist(memHist, memPct);
  document.getElementById("mem-bar").style.width = `${memPct.toFixed(1)}%`;
  document.getElementById("mem-label").textContent = `${memPct.toFixed(0)}%`;
  const memText = `${fmtBytes(s.mem_used)} / ${fmtBytes(s.mem_total)}`;
  document.getElementById("mem-metric").textContent = memText;
  document.getElementById("mem-metric2").textContent = memText;

  const swapPct = s.swap_total ? (s.swap_used / s.swap_total) * 100 : 0;
  document.getElementById("swap-bar").style.width = `${swapPct.toFixed(1)}%`;
  document.getElementById("swap-label").textContent = `${swapPct.toFixed(0)}%`;
  document.getElementById("swap-metric").textContent = s.swap_total
    ? `${fmtBytes(s.swap_used)} / ${fmtBytes(s.swap_total)}`
    : "none";

  // ----- Network -----
  const now = performance.now() / 1000;
  let down = 0;
  let up = 0;
  if (netPrev) {
    const dt = now - netPrev.t;
    if (dt > 0) {
      down = Math.max(0, (s.net_rx_total - netPrev.rx) / dt);
      up = Math.max(0, (s.net_tx_total - netPrev.tx) / dt);
    }
  }
  netPrev = { rx: s.net_rx_total, tx: s.net_tx_total, t: now };
  pushHist(netDownHist, down);
  pushHist(netUpHist, up);
  document.getElementById("net-down").textContent = fmtRate(down);
  document.getElementById("net-up").textContent = fmtRate(up);
  document.getElementById("net-metric").textContent = `↓${fmtRate(down)}  ↑${fmtRate(up)}`;

  // ----- System info -----
  document.getElementById("load-avg").textContent = `${s.load_one.toFixed(2)}  ${s.load_five.toFixed(
    2
  )}  ${s.load_fifteen.toFixed(2)}`;
  document.getElementById("proc-count").textContent = s.process_count;
  document.getElementById("uptime").textContent = fmtDuration(s.uptime);
  document.getElementById("os-version").textContent = s.os_version || "—";
  document.getElementById("kernel").textContent = s.kernel_version || "—";
  document.getElementById("host-label").textContent = s.host_name;

  // ----- Disks -----
  const disksEl = document.getElementById("disks");
  disksEl.innerHTML = "";
  const seen = new Set();
  s.disks.forEach((d) => {
    if (seen.has(d.mount)) return;
    seen.add(d.mount);
    const used = d.total - d.available;
    const pct = d.total ? (used / d.total) * 100 : 0;
    const row = document.createElement("div");
    row.className = "disk";
    row.innerHTML = `<span class="disk-name">${escapeHtml(d.mount)}${d.removable ? " ⏏" : ""}</span>
      <div class="usage-bar"><div class="usage-fill ${usageClass(pct)}" style="width:${pct.toFixed(0)}%"></div></div>
      <span class="disk-info">${fmtBytes(used)} / ${fmtBytes(d.total)}</span>`;
    disksEl.appendChild(row);
  });

  // ----- Graphs -----
  drawGraph(document.getElementById("graph-cpu"), [{ data: cpuHist, color: "#0a63ce", fill: "rgba(10,99,206,0.12)" }], 100);
  drawGraph(document.getElementById("graph-mem"), [{ data: memHist, color: "#34c759", fill: "rgba(52,199,89,0.12)" }], 100);
  const netMax = Math.max(1024, ...netDownHist, ...netUpHist);
  drawGraph(
    document.getElementById("graph-net"),
    [
      { data: netDownHist, color: "#34c759", fill: "rgba(52,199,89,0.10)" },
      { data: netUpHist, color: "#0a63ce", fill: "rgba(10,99,206,0.10)" },
    ],
    netMax
  );
}

// Canvas time-series renderer.
function drawGraph(canvas, seriesList, yMax) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth;
  const h = canvas.clientHeight;
  if (w === 0 || h === 0) return;
  if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
    canvas.width = w * dpr;
    canvas.height = h * dpr;
  }
  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  // gridlines at 25/50/75%
  ctx.strokeStyle = "rgba(0,0,0,0.06)";
  ctx.lineWidth = 1;
  for (let g = 1; g <= 3; g++) {
    const y = (h * g) / 4;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
  }

  const n = HIST;
  const stepX = w / (n - 1);
  const xFor = (i, len) => (i + (n - len)) * stepX; // right-align newest sample
  const yFor = (v) => h - (Math.min(v, yMax) / yMax) * (h - 2) - 1;

  for (const series of seriesList) {
    const d = series.data;
    if (d.length < 2) continue;
    // fill under the curve
    ctx.beginPath();
    ctx.moveTo(xFor(0, d.length), h);
    d.forEach((v, i) => ctx.lineTo(xFor(i, d.length), yFor(v)));
    ctx.lineTo(xFor(d.length - 1, d.length), h);
    ctx.closePath();
    ctx.fillStyle = series.fill;
    ctx.fill();
    // line
    ctx.beginPath();
    d.forEach((v, i) => {
      const x = xFor(i, d.length);
      const y = yFor(v);
      i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
    });
    ctx.strokeStyle = series.color;
    ctx.lineWidth = 1.5;
    ctx.stroke();
  }
}

function renderProcs() {
  const q = document.getElementById("proc-search").value.trim().toLowerCase();
  let list = lastProcs.filter((p) => !q || p.name.toLowerCase().includes(q) || String(p.pid).includes(q));

  const dir = procSortAsc ? 1 : -1;
  list.sort((a, b) => {
    let cmp;
    if (procSortKey === "name" || procSortKey === "status") cmp = a[procSortKey].localeCompare(b[procSortKey]);
    else cmp = (a[procSortKey] || 0) - (b[procSortKey] || 0);
    return cmp * dir;
  });
  list = list.slice(0, 400);

  const rows = document.getElementById("proc-rows");
  rows.innerHTML = "";
  const frag = document.createDocumentFragment();
  for (const p of list) {
    const tr = document.createElement("tr");
    const cpuCls = p.cpu >= 80 ? "cpu-veryhot" : p.cpu >= 30 ? "cpu-hot" : "";
    tr.innerHTML = `
      <td>${p.pid}</td>
      <td>${escapeHtml(p.name)}</td>
      <td class="num ${cpuCls}">${p.cpu.toFixed(1)}</td>
      <td class="num">${fmtBytes(p.memory)}</td>
      <td class="num">${fmtBytes(p.disk_write)}</td>
      <td>${p.status}</td>
      <td class="num">${fmtDuration(p.run_time)}</td>
      <td><button class="kill-btn" title="Force quit">✕</button></td>`;
    tr.querySelector(".kill-btn").addEventListener("click", (ev) => {
      ev.stopPropagation();
      killProc(p);
    });
    frag.appendChild(tr);
  }
  rows.appendChild(frag);
}

async function killProc(p) {
  if (!confirm(`Force quit "${p.name}" (PID ${p.pid})?`)) return;
  try {
    await invoke("kill_process", { pid: p.pid });
    lastProcs = lastProcs.filter((x) => x.pid !== p.pid);
    renderProcs();
  } catch (e) {
    alert(`Could not kill process: ${e}`);
  }
}

document.querySelectorAll(".proc-table th[data-psort]").forEach((th) => {
  th.addEventListener("click", () => {
    const key = th.dataset.psort;
    if (procSortKey === key) procSortAsc = !procSortAsc;
    else {
      procSortKey = key;
      procSortAsc = key === "name" || key === "status";
    }
    document.querySelectorAll(".proc-table th").forEach((h) => h.classList.remove("active"));
    th.classList.add("active");
    renderProcs();
  });
});
document.getElementById("proc-search").addEventListener("input", renderProcs);

setInterval(pollMonitor, POLL_MS);

// ===========================================================================
// CLI forwarding: `scope <folder>` from a second invocation
// ===========================================================================

listen("scope://open-path", (event) => {
  const path = event.payload;
  if (path) {
    switchView("finder");
    navigate(path);
  }
});

// ---------------------------------------------------------------------------
initFinder();
