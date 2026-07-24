// Scope frontend. Talks to the Rust backend via the global Tauri API
// (enabled with `withGlobalTauri` in tauri.conf.json).

const { invoke } = window.__TAURI__.core;
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
  const d = new Date(secs * 1000);
  return d.toLocaleString(undefined, {
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

function usageClass(pct) {
  if (pct >= 80) return "u-high";
  if (pct >= 50) return "u-med";
  return "u-low";
}

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
    case "JSON":
    case "TOML":
    case "Markdown":
    case "Plain Text":
    case "HTML Document":
    case "Stylesheet":
      return "📄";
    default:
      return "📄";
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
const showHidden = document.getElementById("show-hidden");

let currentDir = null;
let currentEntries = [];
let selectedPath = null;
let sortKey = "name";
let sortAsc = true;
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
    { name: "Home", path: HOME, ico: "⌂" },
    { name: "Desktop", path: `${HOME}/Desktop`, ico: "🖥" },
    { name: "Documents", path: `${HOME}/Documents`, ico: "📄" },
    { name: "Downloads", path: `${HOME}/Downloads`, ico: "⬇" },
    { name: "Applications", path: "/Applications", ico: "🅰" },
    { name: "Root", path: "/", ico: "💾" },
  ];
  favoritesEl.innerHTML = "";
  favs.forEach((f) => {
    const li = document.createElement("li");
    li.dataset.path = f.path;
    li.innerHTML = `<span class="ico">${f.ico}</span><span>${f.name}</span>`;
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

  if (!replace) {
    // truncate any forward history, then push
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
  renderBreadcrumb(listing);
  renderFiles();
  highlightFavorite();
}

function highlightFavorite() {
  favoritesEl.querySelectorAll("li").forEach((li) => li.classList.toggle("active", li.dataset.path === currentDir));
}

function updateNavButtons() {
  document.getElementById("nav-back").disabled = historyIndex <= 0;
  document.getElementById("nav-forward").disabled = historyIndex >= history.length - 1;
}

function renderBreadcrumb(listing) {
  breadcrumbEl.innerHTML = "";
  const parts = listing.path.split("/").filter(Boolean);
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
    if (i > 0 || true) {
      const sep = document.createElement("span");
      sep.className = "crumb-sep";
      sep.textContent = "›";
      breadcrumbEl.appendChild(sep);
    }
    addCrumb(part, acc, i === parts.length - 1);
  });
}

function sortedFiltered() {
  const q = finderSearch.value.trim().toLowerCase();
  let list = currentEntries.filter((e) => {
    if (!showHidden.checked && e.hidden) return false;
    if (q && !e.name.toLowerCase().includes(q)) return false;
    return true;
  });
  const dir = sortAsc ? 1 : -1;
  list.sort((a, b) => {
    // folders always first regardless of sort direction on name
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
  fileRows.innerHTML = "";
  finderEmpty.classList.toggle("hidden", list.length > 0);

  const frag = document.createDocumentFragment();
  for (const e of list) {
    const tr = document.createElement("tr");
    if (e.hidden) tr.className = "row-hidden";
    tr.dataset.path = e.path;

    const nameTd = document.createElement("td");
    nameTd.innerHTML = `<div class="name-cell"><span class="ico">${iconFor(e)}</span><span class="txt">${escapeHtml(
      e.name
    )}${e.is_symlink ? " ↪" : ""}</span></div>`;

    const sizeTd = document.createElement("td");
    sizeTd.className = "size";
    sizeTd.textContent = e.is_dir ? "—" : fmtBytes(e.size);

    const kindTd = document.createElement("td");
    kindTd.className = "kind";
    kindTd.textContent = e.kind;

    const dateTd = document.createElement("td");
    dateTd.className = "date";
    dateTd.textContent = fmtDate(e.modified);

    tr.append(nameTd, sizeTd, kindTd, dateTd);

    tr.addEventListener("click", () => selectRow(tr, e));
    tr.addEventListener("dblclick", () => openEntry(e));
    if (e.path === selectedPath) tr.classList.add("selected");
    frag.appendChild(tr);
  }
  fileRows.appendChild(frag);

  const folders = list.filter((e) => e.is_dir).length;
  finderStatus.textContent = `${list.length} items · ${folders} folders · ${list.length - folders} files`;
}

function selectRow(tr, entry) {
  fileRows.querySelectorAll("tr.selected").forEach((r) => r.classList.remove("selected"));
  tr.classList.add("selected");
  selectedPath = entry.path;
}

function openEntry(entry) {
  if (entry.is_dir && entry.kind !== "Application") {
    navigate(entry.path);
  } else {
    invoke("open_path", { path: entry.path }).catch((e) => (finderStatus.textContent = `⚠ ${e}`));
  }
}

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

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
  const parent = currentDir.replace(/\/[^/]+\/?$/, "") || "/";
  navigate(parent);
});
document.getElementById("nav-home").addEventListener("click", () => navigate(HOME));
document.getElementById("reveal-btn").addEventListener("click", () => {
  const target = selectedPath || currentDir;
  invoke("reveal_in_finder", { path: target });
});
finderSearch.addEventListener("input", renderFiles);
showHidden.addEventListener("change", renderFiles);

// Note: back/forward call navigate(path, "silent"); the truthy replace flag
// prevents a new history push (it just overwrites the current slot).

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
let netPrev = null; // { rx, tx, t }
let procSortKey = "cpu";
let procSortAsc = false;
let lastProcs = [];

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
  // CPU
  document.getElementById("cpu-total").textContent = `${s.cpu_usage.toFixed(1)}%`;
  document.getElementById("cpu-brand").textContent = s.cpu_brand;
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
    s.cores.reduce((a, c) => a + c.frequency, 0) / (s.cores.length || 1) /
    1000
  ).toFixed(2)} GHz avg</span>`;

  // Memory
  const memPct = s.mem_total ? (s.mem_used / s.mem_total) * 100 : 0;
  document.getElementById("mem-bar").style.width = `${memPct.toFixed(1)}%`;
  document.getElementById("mem-label").textContent = `${memPct.toFixed(0)}%`;
  document.getElementById("mem-metric").textContent = `${fmtBytes(s.mem_used)} / ${fmtBytes(s.mem_total)}`;

  const swapPct = s.swap_total ? (s.swap_used / s.swap_total) * 100 : 0;
  document.getElementById("swap-bar").style.width = `${swapPct.toFixed(1)}%`;
  document.getElementById("swap-label").textContent = `${swapPct.toFixed(0)}%`;
  document.getElementById("swap-metric").textContent = s.swap_total
    ? `${fmtBytes(s.swap_used)} / ${fmtBytes(s.swap_total)}`
    : "none";

  // Network rates (derived from cumulative counters)
  const now = performance.now() / 1000;
  if (netPrev) {
    const dt = now - netPrev.t;
    if (dt > 0) {
      const down = Math.max(0, (s.net_rx_total - netPrev.rx) / dt);
      const up = Math.max(0, (s.net_tx_total - netPrev.tx) / dt);
      document.getElementById("net-down").textContent = fmtRate(down);
      document.getElementById("net-up").textContent = fmtRate(up);
    }
  }
  netPrev = { rx: s.net_rx_total, tx: s.net_tx_total, t: now };
  document.getElementById("net-total").innerHTML = `<span>↓ ${fmtBytes(s.net_rx_total)} total</span><span>↑ ${fmtBytes(
    s.net_tx_total
  )} total</span>`;

  // System
  document.getElementById("load-avg").textContent = `${s.load_one.toFixed(2)}  ${s.load_five.toFixed(
    2
  )}  ${s.load_fifteen.toFixed(2)}`;
  document.getElementById("proc-count").textContent = s.process_count;
  document.getElementById("uptime").textContent = fmtDuration(s.uptime);
  document.getElementById("os-version").textContent = s.os_version || "—";
  document.getElementById("kernel").textContent = s.kernel_version || "—";
  document.getElementById("host-label").textContent = s.host_name;

  // Disks
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
  list = list.slice(0, 400); // cap DOM size

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
      procSortAsc = key === "name" || key === "status"; // text asc, numbers desc by default
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
