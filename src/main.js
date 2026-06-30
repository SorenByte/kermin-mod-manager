// Kermin Mod Manager - frontend
const T = window.__TAURI__;
const invoke = T.core.invoke;
const SPT_KEY = "spt_root";

const state = {
  sptRoot: null, sptValid: false, sptVersion: null,
  activeTab: "install", modalResolve: null,
  queue: [], qCounter: 0, installing: false,
  searchResults: [],
  invAll: [], invMods: [], updates: {},
  invSort: "name", invSearch: "", invType: "all", invStatus: "all",
  selection: new Set(), anchor: null, dragging: false, dragStart: null, dragPrev: new Set(),
};

const $ = (id) => document.getElementById(id);
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
function toast(msg, isError = false) {
  const t = $("toast"); t.textContent = msg; t.classList.toggle("error", isError);
  t.classList.remove("hidden"); clearTimeout(toast._timer);
  toast._timer = setTimeout(() => t.classList.add("hidden"), 4500);
}
function humanSize(b) {
  if (b == null) return "size unknown";
  const u = ["B", "KB", "MB", "GB"]; let n = b, i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return (i === 0 ? n : n.toFixed(1)) + " " + u[i];
}
function humanNum(n) {
  if (n >= 1e6) return (n / 1e6).toFixed(1).replace(/\.0$/, "") + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1).replace(/\.0$/, "") + "k";
  return String(n || 0);
}
function prettify(slug) {
  return String(slug).split("-").filter(Boolean).map((w) => w[0].toUpperCase() + w.slice(1)).join(" ");
}

// ---- modals ---------------------------------------------------------------
function confirmModal(title, body, label = "OK") {
  return new Promise((res) => {
    $("modal-title").textContent = title; $("modal-body").innerHTML = body;
    $("modal-confirm").textContent = label; $("modal").classList.remove("hidden");
    state.modalResolve = res;
  });
}
function closeModal(r) { $("modal").classList.add("hidden"); if (state.modalResolve) { state.modalResolve(r); state.modalResolve = null; } }
function openChangelog(u) {
  $("cl-title").textContent = `${u.name}  v${u.installed_version} to v${u.latest_version}`;
  const html = u.changelog && u.changelog.trim() ? u.changelog : "<p>No changelog provided.</p>";
  $("cl-body").innerHTML = `<div class="cl-text">${html}</div><div class="cl-actions"><button id="cl-update" class="btn primary">Update this mod</button></div>`;
  $("cl-modal").classList.remove("hidden");
  const b = $("cl-update"); if (b) b.addEventListener("click", () => { $("cl-modal").classList.add("hidden"); doUpdate([u]); });
}

// ---- tabs -----------------------------------------------------------------
const TABS = ["install", "installed"];
function setTab(tab) {
  state.activeTab = tab;
  for (const t of TABS) { $("tab-" + t).classList.toggle("hidden", t !== tab); $("tab-btn-" + t).classList.toggle("active", t === tab); }
  if (tab === "installed") loadInventory();
}

// ---- SPT folder -----------------------------------------------------------
async function setSptRoot(path) {
  state.sptRoot = path;
  let check = { valid: false, note: "" };
  try { check = await invoke("validate_spt_root", { path }); } catch (e) { check = { valid: false, note: String(e) }; }
  state.sptValid = check.valid;
  const base = path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
  $("spt-btn-label").textContent = state.sptValid ? base : "Set SPT folder";
  $("spt-dot").className = "spt-dot " + (state.sptValid ? "ok" : "bad");
  $("spt-btn").title = path + (check.note ? ("   " + check.note) : "");
  const warn = $("spt-warning");
  if (state.sptValid) { warn.classList.add("hidden"); warn.innerHTML = ""; }
  else {
    warn.classList.remove("hidden");
    warn.innerHTML = `<div class="warn-row"><span class="warn-ico">&#9888;</span><div><strong>This is not a valid SPT installation.</strong> Kermin Mod Manager should sit in your SPT game folder (the one containing <code>BepInEx</code> and <code>SPT</code>). Installing here will not work. <span class="mono small">${escapeHtml(path)}</span></div><button id="spt-warn-pick" class="btn sm">Choose SPT folder</button></div>`;
    const b = $("spt-warn-pick"); if (b) b.addEventListener("click", pickSpt);
  }
  state.sptVersion = null;
  if (state.sptValid) { try { state.sptVersion = await invoke("detect_spt_version", { sptRoot: path }); } catch {} }
  if (path) localStorage.setItem(SPT_KEY, path);
  if (state.activeTab === "installed") loadInventory();
}
async function pickSpt() {
  try { const p = await invoke("pick_spt_root"); if (p) await setSptRoot(p); }
  catch (e) { toast("Could not open folder picker: " + e, true); }
}

// ---- SPT compatibility ----------------------------------------------------
function verArr(v) { return (String(v).match(/\d+/g) || []).map(Number); }
function cmpVer(a, b) { for (let i = 0; i < Math.max(a.length, b.length); i++) { const x = a[i] || 0, y = b[i] || 0; if (x !== y) return x < y ? -1 : 1; } return 0; }
function satisfyOne(op, tv, sv) {
  const c = cmpVer(sv, tv);
  switch (op) {
    case ">=": return c >= 0; case ">": return c > 0; case "<=": return c <= 0; case "<": return c < 0;
    case "^": return c >= 0 && sv[0] === tv[0];
    case "~": return c >= 0 && (tv.length >= 2 ? (sv[0] === tv[0] && sv[1] === tv[1]) : sv[0] === tv[0]);
    default: return c === 0;
  }
}
function evalCompat(constraint, sptVer) {
  if (!constraint || !sptVer) return "unknown";
  const sv = verArr(sptVer); if (!sv.length) return "unknown";
  const parts = String(constraint).split(/[,\s]+/).filter(Boolean);
  let satisfied = true, targetMajor = null;
  for (const c of parts) {
    const m = c.match(/^([~^>=<]*)\s*v?(\d+(?:\.\d+){0,2})/); if (!m) continue;
    const tv = verArr(m[2]); if (targetMajor == null) targetMajor = tv[0];
    if (!satisfyOne(m[1], tv, sv)) satisfied = false;
  }
  if (satisfied) return "ok";
  if (targetMajor != null && sv[0] === targetMajor) return "warn";
  return "bad";
}
function compatRank(c) { return ({ ok: 0, warn: 1, unknown: 2, bad: 3 })[c] ?? 2; }
function compatTitle(level, c) {
  if (level === "ok") return `Compatible with your SPT (${c})`;
  if (level === "warn") return `Same major SPT version but targets ${c}; probably fine`;
  if (level === "bad") return `Targets a different SPT major version (${c}); likely incompatible`;
  return c ? `Targets SPT ${c}` : "";
}

// ---- install queue --------------------------------------------------------
function queueHas(id) { return state.queue.some((i) => i.kind === "forge" && i.id === id); }
function addForge(rm) {
  if (!rm || !rm.link) return false;
  if (queueHas(rm.id)) return false;
  state.queue.push({ uid: "f" + rm.id, kind: "forge", id: rm.id, name: rm.name, slug: rm.slug, version: rm.version, link: rm.link, size: rm.size, spt_constraint: rm.spt_constraint, checked: true, status: "", cls: "" });
  return true;
}
function addLocal(name, info) {
  state.qCounter++;
  state.queue.push({ uid: "l" + state.qCounter, kind: "local", name, path: info.path || null, temp: !!info.temp, size: info.size, checked: true, status: "", cls: "" });
}
function clearQueue() { if (state.installing) return; state.queue = []; renderQueue(); }
function removeFromQueue(uid) { if (state.installing) return; state.queue = state.queue.filter((i) => i.uid !== uid); renderQueue(); }
function qSelected() { return state.queue.filter((i) => i.checked); }
function setQStatus(uid, text, cls, err) {
  const it = state.queue.find((x) => x.uid === uid);
  if (it) { it.status = text; it.cls = cls || ""; it.err = err || ""; }
  const row = document.querySelector(`.q-row[data-uid="${uid}"]`); if (!row) return;
  const t = row.querySelector(".q-stext"); if (!t) return;
  t.textContent = text; t.className = "q-stext " + (cls || "");
  if (err) { t.title = err; t.style.cursor = "help"; t.onclick = () => toast(err, true); }
  else { t.title = ""; t.style.cursor = ""; t.onclick = null; }
}
function showQ(uid, sel, show) {
  const row = document.querySelector(`.q-row[data-uid="${uid}"]`); if (!row) return;
  const el = row.querySelector(sel); if (el) el.classList.toggle("hidden", !show);
}
function updateQSummary() {
  const n = qSelected().length;
  $("q-summary").textContent = `${n} of ${state.queue.length} queued`;
  $("q-select-all").checked = state.queue.length > 0 && n === state.queue.length;
  $("q-install").disabled = n === 0 || state.installing;
}
function renderQueue() {
  const list = $("queue-list"); list.innerHTML = "";
  $("queue-empty").classList.toggle("hidden", state.queue.length > 0);
  state.queue.forEach((it) => {
    const row = document.createElement("div"); row.className = "q-row"; row.dataset.uid = it.uid;
    let meta;
    if (it.kind === "forge") meta = `v${escapeHtml(it.version || "?")} &middot; ${humanSize(it.size)}`;
    else meta = (it.classification ? escapeHtml(it.classification) + " &middot; " : "") + (it.size ? humanSize(it.size) : "local file");
    if (it.kind === "forge" && it.spt_constraint) {
      const lvl = evalCompat(it.spt_constraint, state.sptVersion);
      meta += ` &middot; <span class="compat-${lvl}" title="${compatTitle(lvl, it.spt_constraint)}">SPT ${escapeHtml(it.spt_constraint)}</span>`;
    } else if (it.kind === "local") meta += " &middot; local";
    row.innerHTML =
      `<input type="checkbox" class="q-check" data-uid="${it.uid}" ${it.checked ? "checked" : ""} />` +
      `<div class="q-info"><div class="q-name" title="${escapeHtml(it.name)}">${escapeHtml(it.name)}</div>` +
      `<div class="q-meta">${meta} <span class="q-stext ${it.cls || ""}"${it.err ? ` title="${escapeHtml(it.err)}" style="cursor:help"` : ""}>${escapeHtml(it.status)}</span></div>` +
      `<div class="q-prog hidden" data-id="${it.uid}"><span class="q-prog-fill"></span></div></div>` +
      `<button class="q-abort hidden" data-uid="${it.uid}" title="Abort">&#9632;</button>` +
      `<button class="q-remove" data-uid="${it.uid}" title="Remove from queue">&times;</button>`;
    list.appendChild(row);
  });
  const chip = $("ml-spt-chip");
  if (state.sptVersion) { chip.textContent = `Your SPT: ${state.sptVersion}`; chip.classList.remove("hidden"); } else chip.classList.add("hidden");
  list.querySelectorAll(".q-check").forEach((c) => c.addEventListener("change", (e) => { const it = state.queue.find((x) => x.uid === c.dataset.uid); if (it) it.checked = e.target.checked; updateQSummary(); }));
  list.querySelectorAll(".q-remove").forEach((b) => b.addEventListener("click", (e) => { e.stopPropagation(); removeFromQueue(b.dataset.uid); }));
  list.querySelectorAll(".q-abort").forEach((b) => b.addEventListener("click", (e) => { e.stopPropagation(); invoke("cancel_download", { key: b.dataset.uid }); setQStatus(b.dataset.uid, "Aborting...", "err"); }));
  updateQSummary();
}
function onDownloadProgress(p) {
  if (!p || p.key == null) return;
  const bar = document.querySelector(`.q-prog[data-id="${p.key}"]`); if (!bar) return;
  bar.classList.remove("hidden");
  const fill = bar.querySelector(".q-prog-fill");
  if (p.total) { fill.style.width = Math.min(100, Math.round((p.received / p.total) * 100)) + "%"; bar.classList.remove("indet"); }
  else bar.classList.add("indet");
  if (p.done) { setQStatus(p.key, "Installing...", "busy"); bar.classList.add("hidden"); }
}

// ---- unified add bar (name / mod link / list link) ------------------------
async function uniGo() {
  const v = $("uni-search").value.trim();
  if (!v) return;
  if (/forge\.sp-tarkov\.com\/list\//i.test(v)) return addListUrl(v);
  const mm = v.match(/forge\.sp-tarkov\.com\/mod\/(\d+)(?:\/([A-Za-z0-9_-]+))?/i);
  if (mm) return addModUrl(parseInt(mm[1], 10), mm[2] || "");
  return doSearch(v);
}
async function addListUrl(url) {
  const slug = (url.match(/\/list\/\d+\/([A-Za-z0-9_-]+)/) || [])[1] || "";
  const label = slug ? prettify(slug) : "mod list";
  const btn = $("uni-go"); btn.disabled = true; btn.textContent = "...";
  $("ml-status").innerHTML = `&#128203; Mod list detected. Resolving <strong>${escapeHtml(label)}</strong> and looking up versions...`;
  try {
    const mods = await invoke("resolve_modlist", { url });
    let added = 0, failed = 0;
    mods.forEach((m) => { if (m.link) { if (addForge(m)) added++; } else failed++; });
    renderQueue();
    $("ml-status").innerHTML = `&#128203; <strong>Mod list:</strong> ${escapeHtml(label)} &mdash; added ${added} mod(s) to the queue${failed ? `, ${failed} could not be resolved (run again to retry)` : ""}.`;
    $("search-results").innerHTML = "";
  } catch (e) { $("ml-status").textContent = ""; toast("Could not resolve list: " + e, true); }
  finally { btn.disabled = false; btn.textContent = "Go"; }
}
async function addModUrl(id, slug) {
  const btn = $("uni-go"); btn.disabled = true; btn.textContent = "...";
  $("ml-status").textContent = "Mod link detected. Looking up version...";
  try {
    const rm = await invoke("lookup_mod", { id, slug });
    if (rm.link) { addForge(rm); renderQueue(); $("ml-status").innerHTML = `Added mod: <strong>${escapeHtml(rm.name)}</strong>.`; }
    else $("ml-status").textContent = `${rm.name}: no downloadable version found.`;
    $("search-results").innerHTML = "";
  } catch (e) { $("ml-status").textContent = ""; toast("Could not add mod: " + e, true); }
  finally { btn.disabled = false; btn.textContent = "Go"; }
}
async function doSearch(q) {
  const btn = $("uni-go"); btn.disabled = true; btn.textContent = "...";
  $("ml-status").textContent = "Searching the Forge...";
  try { state.searchResults = await invoke("search_mods", { query: q }); renderSearch(); $("ml-status").textContent = ""; }
  catch (e) { $("ml-status").textContent = ""; toast("Search failed: " + e, true); }
  finally { btn.disabled = false; btn.textContent = "Go"; }
}
function renderSearch() {
  const box = $("search-results"); box.innerHTML = "";
  const results = [...state.searchResults];
  results.sort((a, b) => {
    const ca = compatRank(evalCompat(a.spt_constraint, state.sptVersion));
    const cb = compatRank(evalCompat(b.spt_constraint, state.sptVersion));
    if (ca !== cb) return ca - cb;
    return (b.downloads || 0) - (a.downloads || 0);
  });
  if (!results.length) { box.innerHTML = '<p class="muted empty-note">No mods found.</p>'; return; }
  results.forEach((r) => {
    const row = document.createElement("div"); row.className = "search-row";
    const inQ = queueHas(r.id);
    const canAdd = !!r.link;
    const thumb = `<img class="search-thumb" src="${escapeHtml(r.thumbnail || "no-mod-icon.png")}" loading="lazy" alt="" onerror="this.onerror=null;this.src='no-mod-icon.png'">`;
    const meta = [];
    if (r.version) meta.push("v" + escapeHtml(r.version));
    if (r.size) meta.push(humanSize(r.size));
    meta.push(humanNum(r.downloads) + " downloads");
    if (r.author) meta.push("by " + escapeHtml(r.author));
    if (r.category) meta.push(escapeHtml(r.category));
    let compat = "";
    if (r.spt_constraint) { const lvl = evalCompat(r.spt_constraint, state.sptVersion); compat = `<span class="ml-spt compat-${lvl}" title="${compatTitle(lvl, r.spt_constraint)}">SPT ${escapeHtml(r.spt_constraint)}</span>`; }
    const fika = r.fika ? `<span class="fika-badge" title="Fika compatible">Fika</span>` : "";
    const feat = r.featured ? `<span class="feat-badge" title="Featured">&#9733;</span>` : "";
    const btnTxt = inQ ? "in queue" : canAdd ? "Add" : "no download";
    row.innerHTML = thumb +
      `<div class="search-info"><div class="search-name">${escapeHtml(r.name)} ${fika} ${feat} ${compat}</div>` +
      (r.teaser ? `<div class="search-teaser">${escapeHtml(r.teaser)}</div>` : "") +
      `<div class="search-meta">${meta.join(" &middot; ")}</div></div>` +
      `<button class="btn sm ${inQ || !canAdd ? "" : "primary"} search-add" ${inQ || !canAdd ? "disabled" : ""}>${btnTxt}</button>`;
    const b = row.querySelector(".search-add");
    if (!inQ && canAdd) b.addEventListener("click", () => { if (addForge(r)) { renderQueue(); b.textContent = "in queue"; b.disabled = true; b.classList.remove("primary"); } });
    box.appendChild(row);
  });
}

// ---- drag & drop / browse -------------------------------------------------
function addLocalFromPath(path) {
  const name = path.split(/[\\/]/).pop() || path;
  addLocal(name, { path });
  renderQueue();
}
async function streamFileToTemp(file) {
  const tempPath = await invoke("temp_new", { name: file.name });
  const CH = 16 * 1024 * 1024;
  for (let off = 0; off < file.size; off += CH) {
    const buf = await file.slice(off, Math.min(off + CH, file.size)).arrayBuffer();
    await invoke("temp_append", { path: tempPath, bytes: Array.from(new Uint8Array(buf)) });
    const pct = Math.round(Math.min(100, ((off + CH) / Math.max(1, file.size)) * 100));
    $("ml-status").textContent = `Reading ${file.name}... ${pct}%`;
  }
  return tempPath;
}
async function handleDroppedFile(file) {
  const n = file.name.toLowerCase();
  if (!n.endsWith(".zip") && !n.endsWith(".7z") && !n.endsWith(".rar")) return toast("Please drop a .zip, .7z, or .rar file.", true);
  try {
    const path = await streamFileToTemp(file);
    addLocal(file.name, { path, temp: true, size: file.size });
    renderQueue();
    $("ml-status").textContent = "";
  } catch (e) { $("ml-status").textContent = ""; toast("Could not read dropped file: " + e, true); }
}
function wireDragDrop() {
  const dz = $("dropzone");
  ["dragenter", "dragover", "dragleave", "drop"].forEach((evt) => window.addEventListener(evt, (e) => { e.preventDefault(); e.stopPropagation(); }));
  dz.addEventListener("dragover", (e) => { e.preventDefault(); if (e.dataTransfer) e.dataTransfer.dropEffect = "copy"; dz.classList.add("dragging"); });
  dz.addEventListener("dragleave", () => dz.classList.remove("dragging"));
  dz.addEventListener("drop", (e) => { e.preventDefault(); dz.classList.remove("dragging"); const files = e.dataTransfer && e.dataTransfer.files; if (!files || !files.length) return toast("No file detected.", true); for (const f of files) handleDroppedFile(f); });
}

// ---- install the queue ----------------------------------------------------
async function installQueue() {
  if (state.installing) return;
  if (!state.sptValid) return toast("Set a valid SPT folder first.", true);
  let items = qSelected();
  if (items.length === 0) return toast("Nothing queued.", true);
  try {
    const sel = items.filter((i) => i.kind === "forge" && i.version).map((i) => [i.id, i.version]);
    if (sel.length) {
      const deps = await invoke("resolve_dependencies", { sptRoot: state.sptRoot, mods: sel });
      const newDeps = deps.filter((d) => !queueHas(d.id));
      if (newDeps.length) {
        const names = newDeps.slice(0, 12).map((d) => "&bull; " + escapeHtml(d.name)).join("<br>") + (newDeps.length > 12 ? `<br>and ${newDeps.length - 12} more` : "");
        const ok = await confirmModal(`Install ${newDeps.length} missing dependenc${newDeps.length === 1 ? "y" : "ies"}?`, `These are required by queued mods and will be added:<br>${names}`, "Add and install");
        if (ok) { newDeps.forEach((d) => addForge(d)); renderQueue(); }
      }
    }
  } catch (e) { console.error("dependency resolve failed", e); }
  items = qSelected();
  state.installing = true; updateQSummary();
  $("q-install").disabled = true;
  let done = 0, failed = 0;
  for (const it of items) {
    setQStatus(it.uid, it.kind === "forge" ? "Downloading..." : "Installing...", "busy");
    showQ(it.uid, ".q-prog", it.kind === "forge"); showQ(it.uid, ".q-abort", true);
    try {
      if (it.kind === "forge") await invoke("download_and_install", { key: it.uid, name: it.name, url: it.link, sptRoot: state.sptRoot, id: it.id, version: it.version });
      else await invoke("install_local", { key: it.uid, path: it.path, sptRoot: state.sptRoot, deleteAfter: !!it.temp });
      setQStatus(it.uid, "installed", "ok"); done++;
    } catch (e) { const msg = String(e); if (/cancel/i.test(msg)) setQStatus(it.uid, "aborted", "err"); else { setQStatus(it.uid, "failed", "err", msg); failed++; console.error(it.name, e); } }
    showQ(it.uid, ".q-prog", false); showQ(it.uid, ".q-abort", false);
  }
  state.installing = false; updateQSummary();
  toast(`Queue done: ${done} installed${failed ? `, ${failed} failed` : ""}.`);
}

// ---- Manage tab -----------------------------------------------------------
function buildInventory(list) {
  const all = [...list.client, ...list.server];
  const byForge = {}; const result = [];
  for (const m of all) {
    if (m.forge_id && byForge[m.forge_id]) {
      const e = byForge[m.forge_id];
      e.kinds.add(m.kind); e.rel_paths.push(m.rel_path);
      e.file_count += m.file_count; e.size_bytes += m.size_bytes; e.enabled = e.enabled && m.enabled;
      if (!e.version && m.version) e.version = m.version;
      continue;
    }
    const e = { forge_id: m.forge_id || null, name: m.name, kinds: new Set([m.kind]), rel_paths: [m.rel_path], rel_path: m.rel_path, file_count: m.file_count, size_bytes: m.size_bytes, enabled: m.enabled, version: m.version, author: m.author || "", source: m.source, thumbnail: "", teaser: "", downloads: 0, category: "" };
    if (m.forge_id) byForge[m.forge_id] = e;
    result.push(e);
  }
  return result;
}
async function loadInventory() {
  const listEl = $("inv-list");
  if (!state.sptValid) { listEl.innerHTML = ""; state.invAll = []; state.invMods = []; state.selection = new Set(); $("inv-summary").textContent = "Set a valid SPT folder."; updateInvSelection(); return; }
  $("inv-summary").textContent = "Scanning...";
  try {
    const list = await invoke("list_installed", { sptRoot: state.sptRoot });
    state.invAll = buildInventory(list); state.selection = new Set();
    renderInventory();
    const ids = [...new Set(state.invAll.filter((e) => e.forge_id).map((e) => e.forge_id))];
    if (ids.length) {
      try {
        const metas = await invoke("get_mod_meta", { sptRoot: state.sptRoot, ids });
        const map = {}; metas.forEach((m) => { map[m.id] = m; });
        state.invAll.forEach((e) => { const m = e.forge_id && map[e.forge_id]; if (m) { e.thumbnail = m.thumbnail; e.teaser = m.teaser; e.downloads = m.downloads; e.category = m.category; if (!e.author) e.author = m.author; } });
        renderInventory();
      } catch (err) { console.error("enrich failed", err); }
    }
  } catch (e) { $("inv-summary").textContent = "Could not read mods: " + e; }
}
function typeOf(e) { return (e.kinds.has("client") && e.kinds.has("server")) ? "both" : e.kinds.has("client") ? "client" : "server"; }
function modCard(e, idx) {
  const el = document.createElement("div");
  el.className = "mcard" + (e.enabled ? "" : " mcard-disabled");
  el.dataset.idx = idx;
  const ty = typeOf(e);
  const typeTag = ty === "both" ? '<span class="tag tag-both">client + server</span>' : ty === "client" ? '<span class="tag tag-client">client</span>' : '<span class="tag tag-server">server</span>';
  const srcTag = e.source === "app" ? '<span class="tag tag-app">app</span>' : '<span class="tag tag-ext">external</span>';
  const disTag = e.enabled ? "" : '<span class="tag tag-dis">disabled</span>';
  const thumb = `<img class="mcard-thumb" src="${escapeHtml(e.thumbnail || "no-mod-icon.png")}" loading="lazy" alt="" onerror="this.onerror=null;this.src='no-mod-icon.png'">`;
  const meta = [];
  if (e.version) meta.push("v" + escapeHtml(e.version));
  if (e.size_bytes) meta.push(humanSize(e.size_bytes));
  meta.push(e.file_count + (e.file_count === 1 ? " file" : " files"));
  if (e.downloads) meta.push(humanNum(e.downloads) + " downloads");
  if (e.author) meta.push("by " + escapeHtml(e.author));
  if (e.category) meta.push(escapeHtml(e.category));
  const upd = e.forge_id && state.updates[e.forge_id];
  const updBadge = upd ? `<button class="update-badge" title="View changelog and update">&#9650; v${escapeHtml(upd.latest_version)}</button>` : "";
  el.innerHTML = thumb +
    `<div class="mcard-info">` +
      `<div class="mcard-name">${escapeHtml(e.name)} ${typeTag} ${srcTag} ${disTag}</div>` +
      (e.teaser ? `<div class="mcard-teaser">${escapeHtml(e.teaser)}</div>` : "") +
      `<div class="mcard-meta">${meta.join(" &middot; ")}</div>` +
      `<div class="mcard-path mono">${escapeHtml(e.rel_paths.join("   +   "))}</div>` +
    `</div>` +
    `<div class="mcard-right">${updBadge}<label class="mod-toggle" title="${e.enabled ? "Disable" : "Enable"}"><input type="checkbox" class="toggle-input" ${e.enabled ? "checked" : ""} /><span class="toggle-slider"></span></label></div>`;
  const toggle = el.querySelector(".toggle-input");
  toggle.addEventListener("click", (ev) => ev.stopPropagation());
  toggle.addEventListener("change", async (ev) => {
    const enable = ev.target.checked;
    try { await invoke("toggle_mod", { sptRoot: state.sptRoot, relPath: e.rel_path, enable }); loadInventory(); }
    catch (err) { toast("Could not " + (enable ? "enable" : "disable") + ": " + err, true); ev.target.checked = !enable; }
  });
  const ub = el.querySelector(".update-badge");
  if (ub) ub.addEventListener("click", (ev) => { ev.stopPropagation(); if (upd) openChangelog(upd); });
  el.addEventListener("mousedown", (ev) => {
    if (ev.button !== 0) return;
    if (ev.target.closest(".mod-toggle") || ev.target.closest(".update-badge")) return;
    ev.preventDefault();
    if (ev.shiftKey && state.anchor != null) selectRange(state.anchor, idx, false);
    else if (ev.ctrlKey || ev.metaKey) { if (state.selection.has(idx)) state.selection.delete(idx); else state.selection.add(idx); state.anchor = idx; }
    else { state.selection = new Set([idx]); state.anchor = idx; state.dragging = true; state.dragStart = idx; }
    applySelection(); updateInvSelection();
  });
  el.addEventListener("mouseenter", () => { if (!state.dragging) return; selectRange(state.dragStart, idx, false); applySelection(); updateInvSelection(); });
  return el;
}
function selectRange(a, b) { const lo = Math.min(a, b), hi = Math.max(a, b); const set = new Set(); for (let i = lo; i <= hi; i++) set.add(i); state.selection = set; }
function applySelection() { document.querySelectorAll("#inv-list .mcard").forEach((card) => { card.classList.toggle("selected", state.selection.has(parseInt(card.dataset.idx, 10))); }); }
function sortInv(arr) {
  const a = [...arr]; const byName = (x, y) => x.name.toLowerCase().localeCompare(y.name.toLowerCase());
  if (state.invSort === "size") a.sort((x, y) => (y.size_bytes || 0) - (x.size_bytes || 0) || byName(x, y));
  else if (state.invSort === "downloads") a.sort((x, y) => (y.downloads || 0) - (x.downloads || 0) || byName(x, y));
  else if (state.invSort === "updates") a.sort((x, y) => (state.updates[y.forge_id] ? 1 : 0) - (state.updates[x.forge_id] ? 1 : 0) || byName(x, y));
  else if (state.invSort === "disabled") a.sort((x, y) => (x.enabled ? 1 : 0) - (y.enabled ? 1 : 0) || byName(x, y));
  else a.sort(byName);
  return a;
}
function matchesFilter(e) {
  if (state.invType !== "all" && typeOf(e) !== state.invType) return false;
  if (state.invStatus === "enabled" && !e.enabled) return false;
  if (state.invStatus === "disabled" && e.enabled) return false;
  if (state.invStatus === "updates" && !(e.forge_id && state.updates[e.forge_id])) return false;
  const q = state.invSearch.trim().toLowerCase();
  if (q) {
    const hay = [e.name, e.author, e.category, typeOf(e), e.source, e.rel_paths.join(" ")].join(" ").toLowerCase();
    if (!hay.includes(q)) return false;
  }
  return true;
}
function applyInvFilter() {
  let shown = 0;
  document.querySelectorAll("#inv-list .mcard").forEach((card) => {
    const e = state.invMods[parseInt(card.dataset.idx, 10)];
    const show = e && matchesFilter(e);
    card.classList.toggle("hidden", !show);
    if (show) shown++;
  });
  updateInvSelection(shown);
}
function renderInventory() {
  const listEl = $("inv-list"); listEl.innerHTML = "";
  state.invMods = sortInv(state.invAll);
  if (state.invMods.length === 0) { listEl.innerHTML = '<p class="muted empty-note">No mods installed.</p>'; updateInvSelection(0); return; }
  state.invMods.forEach((e, i) => listEl.appendChild(modCard(e, i)));
  $("inv-select-all").checked = false;
  applySelection(); applyInvFilter();
}
function selectedInv() { return Array.from(state.selection).map((i) => state.invMods[i]).filter(Boolean); }
function updateInvSelection(shown) {
  const sel = selectedInv();
  const unBtn = $("inv-uninstall-sel"); unBtn.disabled = sel.length === 0;
  unBtn.textContent = sel.length ? `Uninstall selected (${sel.length})` : "Uninstall selected";
  const updatable = sel.filter((m) => m.forge_id && state.updates[m.forge_id]).length;
  const upBtn = $("inv-update-sel"); const anyUpdates = Object.keys(state.updates).length > 0;
  upBtn.classList.toggle("hidden", !anyUpdates); upBtn.disabled = updatable === 0;
  upBtn.textContent = updatable ? `Update selected (${updatable})` : "Update selected";
  const total = state.invMods.length;
  const shownTxt = (shown != null && shown !== total) ? `${shown} shown of ${total}` : `${total} mods`;
  $("inv-summary").textContent = `${shownTxt}${sel.length ? `, ${sel.length} selected` : ""}`;
}
async function bulkUninstall() {
  const sel = selectedInv(); if (sel.length === 0) return;
  const hasServer = sel.some((m) => m.kinds.has("server"));
  const warn = hasServer ? '<br><br><span class="warn-text">Some are server mods. Removing ones that added traders, quests, or items can affect existing profiles.</span>' : "";
  const names = sel.slice(0, 10).map((m) => "&bull; " + escapeHtml(m.name)).join("<br>") + (sel.length > 10 ? `<br>and ${sel.length - 10} more` : "");
  const ok = await confirmModal(`Uninstall ${sel.length} mods?`, `${names}${warn}`, `Uninstall ${sel.length}`);
  if (!ok) return;
  let done = 0, failed = 0;
  for (const m of sel) { try { await invoke("uninstall_mod", { sptRoot: state.sptRoot, relPath: m.rel_path }); done++; } catch (e) { failed++; console.error(m.name, e); } }
  toast(`Uninstalled ${done}${failed ? `, ${failed} failed` : ""}.`); loadInventory();
}
async function checkUpdates() {
  if (!state.sptValid) return;
  const btn = $("inv-check-updates"); btn.disabled = true; btn.textContent = "Checking...";
  try {
    const ups = await invoke("check_updates", { sptRoot: state.sptRoot });
    state.updates = {}; ups.forEach((u) => { state.updates[u.forge_id] = u; });
    renderInventory();
    toast(ups.length ? `${ups.length} update(s) available.` : "All tracked mods are up to date.");
  } catch (e) { toast("Update check failed: " + e, true); }
  finally { btn.disabled = false; btn.textContent = "Check for updates"; }
}
async function doUpdate(updates) {
  if (!updates || !updates.length) return;
  let done = 0, failed = 0;
  for (const u of updates) {
    toast(`Updating ${u.name}...`);
    try { await invoke("download_and_install", { key: "u" + u.forge_id, name: u.name, url: u.link, sptRoot: state.sptRoot, id: u.forge_id, version: u.latest_version }); delete state.updates[u.forge_id]; done++; }
    catch (e) { failed++; console.error(u.name, e); }
  }
  toast(`Updated ${done}${failed ? `, ${failed} failed` : ""}.`); loadInventory();
}
function updateSelected() {
  const ups = selectedInv().map((m) => m.forge_id && state.updates[m.forge_id]).filter(Boolean);
  if (!ups.length) return toast("None of the selected mods have updates.", true);
  doUpdate(ups);
}

// ---- init -----------------------------------------------------------------
async function init() {
  $("spt-btn").addEventListener("click", pickSpt);
  $("tab-btn-install").addEventListener("click", () => setTab("install"));
  $("tab-btn-installed").addEventListener("click", () => setTab("installed"));
  $("uni-go").addEventListener("click", uniGo);
  $("uni-search").addEventListener("keydown", (e) => { if (e.key === "Enter") uniGo(); });
  $("browse-zip").addEventListener("click", async () => { try { const p = await invoke("pick_zip"); if (p) addLocalFromPath(p); } catch (e) { toast("Could not open file picker: " + e, true); } });
  $("q-install").addEventListener("click", installQueue);
  $("q-clear").addEventListener("click", clearQueue);
  $("q-select-all").addEventListener("change", (e) => { state.queue.forEach((it) => { it.checked = e.target.checked; }); renderQueue(); });

  $("inv-uninstall-sel").addEventListener("click", bulkUninstall);
  $("inv-check-updates").addEventListener("click", checkUpdates);
  $("inv-update-sel").addEventListener("click", updateSelected);
  $("inv-search").addEventListener("input", (e) => { state.invSearch = e.target.value; applyInvFilter(); });
  $("inv-filter-type").addEventListener("change", (e) => { state.invType = e.target.value; applyInvFilter(); });
  $("inv-filter-status").addEventListener("change", (e) => { state.invStatus = e.target.value; applyInvFilter(); });
  $("inv-sort").addEventListener("change", (e) => { state.invSort = e.target.value; state.selection = new Set(); renderInventory(); });
  $("inv-select-all").addEventListener("change", (e) => {
    if (e.target.checked) { state.selection = new Set(); state.invMods.forEach((m, i) => { if (matchesFilter(m)) state.selection.add(i); }); }
    else state.selection = new Set();
    applySelection(); updateInvSelection();
  });

  $("modal-cancel").addEventListener("click", () => closeModal(false));
  $("modal-confirm").addEventListener("click", () => closeModal(true));
  $("modal").addEventListener("click", (e) => { if (e.target.id === "modal") closeModal(false); });
  $("cl-close").addEventListener("click", () => $("cl-modal").classList.add("hidden"));
  $("cl-modal").addEventListener("click", (e) => { if (e.target.id === "cl-modal") $("cl-modal").classList.add("hidden"); });
  document.addEventListener("mouseup", () => { state.dragging = false; });

  wireDragDrop();
  T.event.listen("resolve-progress", (e) => { const p = e.payload || {}; $("ml-status").textContent = `Looking up versions... ${p.done}/${p.total}`; });
  T.event.listen("download-progress", (e) => onDownloadProgress(e.payload));

  // Portable: prefer the folder the exe is sitting in. Fall back to a saved
  // folder only if the exe's folder isn't a valid SPT install (e.g. dev runs).
  let here = null;
  try { here = await invoke("default_spt_root"); } catch {}
  const saved = localStorage.getItem(SPT_KEY);
  let chosen = null;
  if (here) {
    let chk = { valid: false };
    try { chk = await invoke("validate_spt_root", { path: here }); } catch {}
    if (chk.valid) chosen = here;
  }
  if (!chosen && saved) chosen = saved;
  if (!chosen && here) chosen = here;
  if (chosen) await setSptRoot(chosen);
  else $("spt-dot").className = "spt-dot bad";
}
window.addEventListener("DOMContentLoaded", init);
