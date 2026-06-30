use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::Emitter;
use tauri_plugin_dialog::DialogExt;

// ---------------------------------------------------------------------------
// SPT 4.0 install targets (verified against wiki.sp-tarkov.com):
//   * Client mods  -> <game>/BepInEx/plugins  (and BepInEx/patchers, BepInEx/config)
//   * Server mods  -> <game>/SPT/user/mods
// The resolver normalises ANY archive layout onto these canonical targets.
// ---------------------------------------------------------------------------

const ANCHORS: [&str; 6] = ["spt", "bepinex", "user", "mods", "plugins", "patchers"];

// ---------------------------------------------------------------------------
// Data shapes returned to the frontend
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct MappedFile {
    original: String,
    /// Path relative to the SPT game root where this file will be written.
    target: String,
    /// "client" | "server"
    category: String,
}

#[derive(Serialize)]
struct ZipReport {
    mod_name: String,
    /// "client" | "server" | "mixed" | "unknown"
    classification: String,
    /// "anchored" | "bare-server" | "bare-client" | "empty"
    mode: String,
    /// True when targets were guessed from a non-standard layout (verify these!).
    heuristic: bool,
    client: Vec<MappedFile>,
    server: Vec<MappedFile>,
    unrecognized: Vec<String>,
    total_files: usize,
}

#[derive(Serialize)]
struct InstallResult {
    installed: Vec<MappedFile>,
    overwritten: Vec<String>,
    skipped: Vec<String>,
    client_count: usize,
    server_count: usize,
    manifest_path: String,
}

#[derive(Serialize)]
struct SptRootCheck {
    valid: bool,
    note: String,
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

fn parts_of(name: &str) -> Vec<String> {
    name.replace('\\', "/")
        .split('/')
        .filter(|p| !p.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn has_traversal(parts: &[String]) -> bool {
    parts.iter().any(|p| p == "..")
}

fn is_anchored(files: &[String]) -> bool {
    files.iter().any(|n| {
        parts_of(n)
            .iter()
            .any(|p| ANCHORS.contains(&p.to_lowercase().as_str()))
    })
}

/// If every entry shares a single top-level folder, return it; otherwise None
/// (None also when any file sits at the archive root).
fn common_top(files: &[String]) -> Option<String> {
    let mut top: Option<String> = None;
    for n in files {
        let p = parts_of(n);
        if p.len() >= 2 {
            match &top {
                None => top = Some(p[0].clone()),
                Some(t) => {
                    if *t != p[0] {
                        return None;
                    }
                }
            }
        } else {
            return None;
        }
    }
    top
}

/// Resolve one entry from an archive that contains recognised anchor folders.
fn resolve_anchored(name: &str) -> Option<(String, &'static str)> {
    let parts = parts_of(name);
    if has_traversal(&parts) {
        return None;
    }
    for (i, p) in parts.iter().enumerate() {
        match p.to_lowercase().as_str() {
            "spt" => {
                let rest = parts[i + 1..].join("/");
                let t = if rest.is_empty() { "SPT".to_string() } else { format!("SPT/{rest}") };
                return Some((t, "server"));
            }
            "bepinex" => {
                let rest = parts[i + 1..].join("/");
                let t = if rest.is_empty() { "BepInEx".to_string() } else { format!("BepInEx/{rest}") };
                return Some((t, "client"));
            }
            "user" => return Some((format!("SPT/{}", parts[i..].join("/")), "server")),
            "mods" => return Some((format!("SPT/user/{}", parts[i..].join("/")), "server")),
            "plugins" => return Some((format!("BepInEx/{}", parts[i..].join("/")), "client")),
            "patchers" => return Some((format!("BepInEx/{}", parts[i..].join("/")), "client")),
            _ => {}
        }
    }
    None
}

struct Resolution {
    mode: &'static str,
    mapped: Vec<MappedFile>,
    unrecognized: Vec<String>,
}

/// Loose .exe files (e.g. FikaSync) are standalone tools that belong in the SPT
/// game root. Map them there by file name.
fn exe_to_root(name: &str) -> Option<(String, &'static str)> {
    let norm = name.replace('\\', "/");
    if norm.to_lowercase().ends_with(".exe") {
        let base = norm.rsplit('/').next().unwrap_or(&norm).to_string();
        if base.is_empty() || base.contains("..") {
            return None;
        }
        return Some((base, "client"));
    }
    None
}

fn resolve_all(names: &[String], mod_name: &str) -> Resolution {
    let files: Vec<String> = names.iter().filter(|n| !n.ends_with('/')).cloned().collect();

    if is_anchored(&files) {
        let mut mapped = Vec::new();
        let mut unrecognized = Vec::new();
        for n in &files {
            match resolve_anchored(n).or_else(|| exe_to_root(n)) {
                Some((target, category)) => mapped.push(MappedFile {
                    original: n.clone(),
                    target,
                    category: category.to_string(),
                }),
                None => unrecognized.push(n.clone()),
            }
        }
        return Resolution { mode: "anchored", mapped, unrecognized };
    }

    // No anchor folders: this is a "bare" mod. Guess from its contents.
    let has_pkg = files.iter().any(|f| {
        parts_of(f)
            .last()
            .map(|s| s.eq_ignore_ascii_case("package.json"))
            .unwrap_or(false)
    });
    let has_dll = files.iter().any(|f| f.to_lowercase().ends_with(".dll"));
    let has_exe = files.iter().any(|f| f.to_lowercase().ends_with(".exe"));
    let ct = common_top(&files);

    let mut unrecognized = Vec::new();

    if has_pkg {
        let mut mapped = Vec::new();
        for n in &files {
            if has_traversal(&parts_of(n)) {
                unrecognized.push(n.clone());
                continue;
            }
            let norm = n.replace('\\', "/");
            let target = if ct.is_some() {
                format!("SPT/user/mods/{norm}")
            } else {
                format!("SPT/user/mods/{mod_name}/{norm}")
            };
            mapped.push(MappedFile { original: n.clone(), target, category: "server".into() });
        }
        Resolution { mode: "bare-server", mapped, unrecognized }
    } else if has_dll {
        let mut mapped = Vec::new();
        for n in &files {
            if has_traversal(&parts_of(n)) {
                unrecognized.push(n.clone());
                continue;
            }
            let norm = n.replace('\\', "/");
            mapped.push(MappedFile {
                original: n.clone(),
                target: format!("BepInEx/plugins/{norm}"),
                category: "client".into(),
            });
        }
        Resolution { mode: "bare-client", mapped, unrecognized }
    } else if has_exe {
        // Bare archive whose payload is a root-level executable (e.g. FikaSync).
        let mut mapped = Vec::new();
        for n in &files {
            match exe_to_root(n) {
                Some((target, category)) => mapped.push(MappedFile {
                    original: n.clone(),
                    target,
                    category: category.to_string(),
                }),
                None => unrecognized.push(n.clone()),
            }
        }
        Resolution { mode: "bare-root", mapped, unrecognized }
    } else {
        Resolution { mode: "empty", mapped: Vec::new(), unrecognized: files }
    }
}

fn classify(mapped: &[MappedFile]) -> String {
    let has_client = mapped.iter().any(|m| m.category == "client");
    let has_server = mapped.iter().any(|m| m.category == "server");
    match (has_client, has_server) {
        (true, true) => "mixed",
        (true, false) => "client",
        (false, true) => "server",
        (false, false) => "unknown",
    }
    .to_string()
}

fn now_millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn mod_name_from_zip(zip_path: &str) -> String {
    Path::new(zip_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mod")
        .to_string()
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn new_temp_dir() -> Result<PathBuf, String> {
    let n = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("sptmod-extract-{}-{}", now_millis(), n));
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create temp dir: {e}"))?;
    Ok(dir)
}

fn detect_format(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 6 && bytes[..6] == [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C] {
        "7z"
    } else if bytes.len() >= 4 && &bytes[..4] == b"PK\x03\x04" {
        "zip"
    } else if bytes.len() >= 4 && &bytes[..4] == b"PK\x05\x06" {
        "zip"
    } else if bytes.len() >= 4 && &bytes[..4] == b"Rar!" {
        "rar"
    } else {
        "unknown"
    }
}

fn extract_zip<R: Read + std::io::Seek>(mut archive: zip::ZipArchive<R>, dir: &Path) -> Result<(), String> {
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
        let name = entry.name().replace('\\', "/");
        if name.contains("..") {
            continue;
        }
        let out = dir.join(&name);
        if !out.starts_with(dir) {
            continue;
        }
        if entry.is_dir() {
            let _ = fs::create_dir_all(&out);
        } else {
            if let Some(par) = out.parent() {
                let _ = fs::create_dir_all(par);
            }
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf).map_err(|e| format!("read zip entry: {e}"))?;
            fs::write(&out, &buf).map_err(|e| format!("write temp file: {e}"))?;
        }
    }
    Ok(())
}

const FMT_MSG: &str = "Unrecognized archive format (expected .zip or .7z).";

/// Extract from an in-memory archive (drag-drop / byte commands).
fn extract_rar(src: &Path, dir: &Path) -> Result<(), String> {
    let mut archive = unrar::Archive::new(src)
        .open_for_processing()
        .map_err(|e| format!("Could not open .rar: {e:?}"))?;
    while let Some(header) = archive.read_header().map_err(|e| format!("rar header: {e:?}"))? {
        let name = header.entry().filename.to_string_lossy().replace('\\', "/");
        let is_file = header.entry().is_file();
        if is_file && !name.contains("..") {
            let dest = dir.join(&name);
            if let Some(par) = dest.parent() {
                let _ = fs::create_dir_all(par);
            }
            archive = header.extract_to(dest).map_err(|e| format!("rar extract: {e:?}"))?;
        } else {
            archive = header.skip().map_err(|e| format!("rar skip: {e:?}"))?;
        }
    }
    Ok(())
}

fn extract_to_temp(bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = new_temp_dir()?;
    let result = match detect_format(bytes) {
        "zip" => zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|e| format!("Invalid zip: {e}"))
            .and_then(|a| extract_zip(a, &dir)),
        "7z" => {
            let tmp7z = dir.join("__src.7z");
            fs::write(&tmp7z, bytes)
                .map_err(|e| format!("write temp 7z: {e}"))
                .and_then(|_| {
                    let r = sevenz_rust2::decompress_file(&tmp7z, &dir)
                        .map_err(|e| format!("Could not extract .7z archive: {e}"));
                    let _ = fs::remove_file(&tmp7z);
                    r
                })
        }
        "rar" => {
            let tmprar = dir.join("__src.rar");
            fs::write(&tmprar, bytes)
                .map_err(|e| format!("write temp rar: {e}"))
                .and_then(|_| {
                    let r = extract_rar(&tmprar, &dir);
                    let _ = fs::remove_file(&tmprar);
                    r
                })
        }
        _ => Err(FMT_MSG.to_string()),
    };
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&dir);
        return Err(e);
    }
    Ok(dir)
}

/// Extract straight from a file on disk (downloads) - avoids holding the whole
/// archive in memory, so large mods work.
fn extract_path_to_temp(src: &Path) -> Result<PathBuf, String> {
    let mut header = [0u8; 6];
    {
        let mut f = fs::File::open(src).map_err(|e| format!("open archive: {e}"))?;
        let _ = f.read(&mut header);
    }
    let dir = new_temp_dir()?;
    let result = match detect_format(&header) {
        "zip" => fs::File::open(src)
            .map_err(|e| format!("open zip: {e}"))
            .and_then(|f| zip::ZipArchive::new(f).map_err(|e| format!("Invalid zip: {e}")))
            .and_then(|a| extract_zip(a, &dir)),
        "7z" => sevenz_rust2::decompress_file(src, &dir)
            .map_err(|e| format!("Could not extract .7z archive: {e}")),
        "rar" => extract_rar(src, &dir),
        _ => Err(FMT_MSG.to_string()),
    };
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&dir);
        return Err(e);
    }
    Ok(dir)
}

/// Collect file paths under `dir`, relative to `base`, using forward slashes.
fn walk_files(base: &Path, dir: &Path, out: &mut Vec<String>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk_files(base, &p, out);
            } else if let Ok(rel) = p.strip_prefix(base) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

fn report_from_extracted(dir: PathBuf, mod_name: &str) -> Result<ZipReport, String> {
    let mut names = Vec::new();
    walk_files(&dir, &dir, &mut names);
    let res = resolve_all(&names, mod_name);
    let total = names.len();
    let _ = fs::remove_dir_all(&dir);

    let client: Vec<MappedFile> = res.mapped.iter().filter(|m| m.category == "client").cloned().collect();
    let server: Vec<MappedFile> = res.mapped.iter().filter(|m| m.category == "server").cloned().collect();
    let classification = classify(&res.mapped);
    Ok(ZipReport {
        mod_name: mod_name.to_string(),
        classification,
        heuristic: res.mode.starts_with("bare"),
        mode: res.mode.to_string(),
        client,
        server,
        unrecognized: res.unrecognized,
        total_files: total,
    })
}

fn install_from_extracted(dir: PathBuf, mod_name: &str, source: &str, root: &Path) -> Result<InstallResult, String> {
    let mut names = Vec::new();
    walk_files(&dir, &dir, &mut names);
    let res = resolve_all(&names, mod_name);

    if res.mapped.is_empty() {
        let _ = fs::remove_dir_all(&dir);
        return Err("Nothing installable was found in this archive. Its layout is not recognised.".into());
    }

    let mut installed = Vec::new();
    let mut overwritten = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut client_count = 0usize;
    let mut server_count = 0usize;

    for m in &res.mapped {
        if m.target.contains("..") {
            skipped.push(m.original.clone());
            continue;
        }
        let target = root.join(&m.target);
        if !target.starts_with(root) {
            skipped.push(m.original.clone());
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
        }
        let existed = target.exists();
        let src = dir.join(&m.original);
        fs::copy(&src, &target).map_err(|e| format!("Could not write {}: {e}", m.target))?;
        if existed {
            overwritten.push(m.target.clone());
        }
        match m.category.as_str() {
            "client" => client_count += 1,
            "server" => server_count += 1,
            _ => {}
        }
        installed.push(MappedFile {
            original: m.original.clone(),
            target: m.target.clone(),
            category: m.category.clone(),
        });
    }
    for u in &res.unrecognized {
        skipped.push(u.clone());
    }

    let _ = fs::remove_dir_all(&dir);

    if installed.is_empty() {
        return Err("Nothing was installed (no files mapped to a valid SPT location).".into());
    }

    let classification = classify(&installed);
    let manifest_dir = root.join(".spt-mod-installer").join("manifests");
    fs::create_dir_all(&manifest_dir).map_err(|e| format!("Could not create manifest folder: {e}"))?;
    let manifest_path = manifest_dir.join(format!("{}-{}.json", sanitize(mod_name), now_millis()));
    let manifest = serde_json::json!({
        "mod_name": mod_name,
        "source": source,
        "installed_at_ms": now_millis(),
        "classification": classification,
        "client_count": client_count,
        "server_count": server_count,
        "files": installed,
    });
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap_or_default())
        .map_err(|e| format!("Could not write manifest: {e}"))?;

    Ok(InstallResult {
        installed,
        overwritten,
        skipped,
        client_count,
        server_count,
        manifest_path: manifest_path.to_string_lossy().to_string(),
    })
}

fn build_report(bytes: &[u8], mod_name: &str) -> Result<ZipReport, String> {
    let dir = extract_to_temp(bytes)?;
    report_from_extracted(dir, mod_name)
}

fn build_install(bytes: &[u8], mod_name: &str, source: &str, root: &Path) -> Result<InstallResult, String> {
    let dir = extract_to_temp(bytes)?;
    install_from_extracted(dir, mod_name, source, root)
}

fn build_install_path(src: &Path, mod_name: &str, source: &str, root: &Path) -> Result<InstallResult, String> {
    let dir = extract_path_to_temp(src)?;
    install_from_extracted(dir, mod_name, source, root)
}

// ---------------------------------------------------------------------------
// Inventory (Phase 2): scan the SPT folder for installed mods
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ModEntry {
    name: String,
    version: Option<String>,
    author: Option<String>,
    kind: String,      // "client" | "server"
    rel_path: String,  // live path relative to game root, e.g. "BepInEx/plugins/SAIN"
    file_count: usize,
    size_bytes: u64,
    source: String,    // "app" | "external"
    enabled: bool,
    forge_id: Option<u64>,
}

#[derive(Serialize)]
struct InstalledList {
    client: Vec<ModEntry>,
    server: Vec<ModEntry>,
}

#[derive(Serialize)]
struct UninstallResult {
    removed: Vec<String>,
    via_manifest: bool,
    spanned_both: bool,
}

struct Manifest {
    path: PathBuf,
    files: Vec<String>,
    forge_id: Option<u64>,
    forge_version: Option<String>,
    installed_at: u128,
}

fn dir_size(dir: &Path) -> u64 {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { n += dir_size(&p); }
            else if let Ok(m) = p.metadata() { n += m.len(); }
        }
    }
    n
}

fn count_files(dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { n += count_files(&p); } else { n += 1; }
        }
    }
    n
}

fn read_pkg(mod_dir: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let pkg = mod_dir.join("package.json");
    if let Ok(text) = fs::read_to_string(&pkg) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            let name = v.get("name").and_then(|x| x.as_str()).map(|s| s.to_string());
            let version = v.get("version").and_then(|x| x.as_str()).map(|s| s.to_string());
            let author = v.get("author").and_then(|x| x.as_str()).map(|s| s.to_string());
            return (name, version, author);
        }
    }
    (None, None, None)
}

fn load_manifests(root: &Path) -> Vec<Manifest> {
    let mut out = Vec::new();
    let dir = root.join(".spt-mod-installer").join("manifests");
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&p) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    let files = v
                        .get("files")
                        .and_then(|f| f.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|m| m.get("target").and_then(|t| t.as_str()).map(|s| s.replace('\\', "/")))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    out.push(Manifest {
                        path: p,
                        files,
                        forge_id: v.get("forge_id").and_then(|x| x.as_u64()),
                        forge_version: v.get("forge_version").and_then(|x| x.as_str()).map(|s| s.to_string()),
                        installed_at: v.get("installed_at_ms").and_then(|x| x.as_u64()).unwrap_or(0) as u128,
                    });
                }
            }
        }
    }
    out
}

fn manifest_for<'a>(manifests: &'a [Manifest], rel_path: &str) -> Option<&'a Manifest> {
    let prefix = format!("{}/", rel_path.trim_end_matches('/'));
    manifests
        .iter()
        .find(|m| m.files.iter().any(|f| f == rel_path || f.starts_with(&prefix)))
}

fn is_spt_core_client(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "spt" || n.starts_with("spt-") || n.starts_with("spt_")
}

fn push_entry(
    out: &mut Vec<ModEntry>,
    name: String,
    kind: &str,
    rel: String,
    file_count: usize,
    size_bytes: u64,
    pkg_version: Option<String>,
    author: Option<String>,
    manifests: &[Manifest],
    enabled: bool,
) {
    let m = manifest_for(manifests, &rel);
    let source = if m.is_some() { "app" } else { "external" };
    let forge_id = m.and_then(|x| x.forge_id);
    let version = m.and_then(|x| x.forge_version.clone()).or(pkg_version);
    out.push(ModEntry {
        name,
        version,
        author,
        kind: kind.to_string(),
        rel_path: rel,
        file_count,
        size_bytes,
        source: source.to_string(),
        enabled,
        forge_id,
    });
}

/// Scan one tree (the live root, or the disabled mirror) for installed mods.
fn scan_tree(base: &Path, manifests: &[Manifest], enabled: bool, out: &mut Vec<ModEntry>) {
    for sub in ["plugins", "patchers"] {
        let dir = base.join("BepInEx").join(sub);
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                let fname = e.file_name().to_string_lossy().to_string();
                let rel = format!("BepInEx/{sub}/{fname}");
                if p.is_dir() {
                    if is_spt_core_client(&fname) { continue; }
                    push_entry(out, fname, "client", rel, count_files(&p), dir_size(&p), None, None, manifests, enabled);
                } else if p.extension().and_then(|x| x.to_str()).map(|x| x.eq_ignore_ascii_case("dll")).unwrap_or(false) {
                    let stem = p.file_stem().and_then(|x| x.to_str()).unwrap_or(&fname).to_string();
                    if is_spt_core_client(&stem) { continue; }
                    let sz = p.metadata().map(|m| m.len()).unwrap_or(0);
                    push_entry(out, stem, "client", rel, 1, sz, None, None, manifests, enabled);
                }
            }
        }
    }
    let dir = base.join("SPT").join("user").join("mods");
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() { continue; }
            let fname = e.file_name().to_string_lossy().to_string();
            let rel = format!("SPT/user/mods/{fname}");
            let (pname, pver, pauth) = read_pkg(&p);
            push_entry(out, pname.unwrap_or(fname), "server", rel, count_files(&p), dir_size(&p), pver, pauth, manifests, enabled);
        }
    }
}

fn prune_empty_dirs(root: &Path, mut dir: PathBuf) {
    let guards: Vec<PathBuf> = [
        root.join("BepInEx").join("plugins"),
        root.join("BepInEx").join("patchers"),
        root.join("BepInEx").join("config"),
        root.join("BepInEx"),
        root.join("SPT").join("user").join("mods"),
        root.join("SPT").join("user"),
        root.join("SPT"),
        root.to_path_buf(),
    ]
    .into_iter()
    .collect();
    while dir.starts_with(root) && !guards.contains(&dir) {
        match fs::read_dir(&dir) {
            Ok(mut rd) => {
                if rd.next().is_some() {
                    break; // not empty
                }
            }
            Err(_) => break,
        }
        if fs::remove_dir(&dir).is_err() {
            break;
        }
        match dir.parent() {
            Some(par) => dir = par.to_path_buf(),
            None => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Accepts a .zip or .7z file path.
#[tauri::command]
fn inspect_zip(path: String) -> Result<ZipReport, String> {
    let bytes = fs::read(&path).map_err(|e| format!("Could not read file: {e}"))?;
    build_report(&bytes, &mod_name_from_zip(&path))
}

/// Drag-and-drop path: the frontend reads the dropped file and sends its bytes.
#[tauri::command]
fn inspect_zip_bytes(name: String, bytes: Vec<u8>) -> Result<ZipReport, String> {
    build_report(&bytes, &mod_name_from_zip(&name))
}

#[tauri::command]
fn install_zip(zip_path: String, spt_root: String) -> Result<InstallResult, String> {
    let root = PathBuf::from(&spt_root);
    if !root.is_dir() {
        return Err("SPT root folder does not exist.".into());
    }
    build_install_path(&PathBuf::from(&zip_path), &mod_name_from_zip(&zip_path), &zip_path, &root)
}

/// Create an empty temp file and return its path. Used to stream a large
/// dropped file to disk in chunks instead of loading it into memory.
#[tauri::command]
fn temp_new(name: String) -> Result<String, String> {
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("kdrop-{}-{}-{}", now_millis(), seq, sanitize(&name)));
    fs::File::create(&p).map_err(|e| format!("Could not create temp file: {e}"))?;
    Ok(p.to_string_lossy().to_string())
}

#[tauri::command]
fn temp_append(path: String, bytes: Vec<u8>) -> Result<(), String> {
    use std::io::Write;
    let p = PathBuf::from(&path);
    if !p.starts_with(std::env::temp_dir()) {
        return Err("Refusing to write outside the temp folder.".into());
    }
    let mut f = fs::OpenOptions::new().append(true).open(&p).map_err(|e| format!("Could not open temp file: {e}"))?;
    f.write_all(&bytes).map_err(|e| format!("Could not write temp file: {e}"))?;
    Ok(())
}

/// Install a local archive from a path, on a blocking thread so it can be
/// aborted/skipped. delete_after removes the file when done (for temp drops).
#[tauri::command]
async fn install_local(key: String, path: String, spt_root: String, delete_after: bool) -> Result<InstallResult, String> {
    let root = PathBuf::from(&spt_root);
    if !root.is_dir() {
        return Err("SPT root folder does not exist.".into());
    }
    cancel_clear(&key);
    let src = PathBuf::from(&path);
    let name = mod_name_from_zip(&path);
    let src2 = src.clone();
    let root2 = root.clone();
    let source = format!("{name} (local)");
    let handle = tokio::task::spawn_blocking(move || build_install_path(&src2, &name, &source, &root2));
    let res: Result<InstallResult, String> = loop {
        if handle.is_finished() {
            break match handle.await {
                Ok(r) => r,
                Err(e) => Err(format!("Install task failed: {e}")),
            };
        }
        if cancel_is_set(&key) {
            handle.abort();
            cancel_clear(&key);
            if delete_after {
                let _ = fs::remove_file(&src);
            }
            return Err("Cancelled".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    };
    if delete_after {
        let _ = fs::remove_file(&src);
    }
    cancel_clear(&key);
    res
}

#[tauri::command]
fn install_zip_bytes(name: String, bytes: Vec<u8>, spt_root: String) -> Result<InstallResult, String> {
    let root = PathBuf::from(&spt_root);
    if !root.is_dir() {
        return Err("SPT root folder does not exist.".into());
    }
    let mod_name = mod_name_from_zip(&name);
    build_install(&bytes, &mod_name, &format!("{name} (drag-drop)"), &root)
}

#[tauri::command]
fn validate_spt_root(path: String) -> SptRootCheck {
    let root = Path::new(&path);
    if !root.is_dir() {
        return SptRootCheck { valid: false, note: "Folder does not exist.".into() };
    }
    let has_spt = root.join("SPT").is_dir();
    let has_bepinex = root.join("BepInEx").is_dir();
    let has_exe = root.join("SPT.Launcher.exe").is_file()
        || root.join("EscapeFromTarkov.exe").is_file()
        || root.join("SPT.Server.exe").is_file();
    if has_spt || has_bepinex || has_exe {
        SptRootCheck { valid: true, note: "Looks like a valid SPT install.".into() }
    } else {
        SptRootCheck {
            valid: false,
            note: "No SPT/, BepInEx/, or SPT executable found here. Make sure this is your SPT game folder.".into(),
        }
    }
}

/// Portable workflow: the folder the .exe is sitting in (i.e. the SPT root).
#[tauri::command]
fn default_spt_root() -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_string_lossy().to_string()))
}

#[tauri::command]
async fn pick_spt_root(app: tauri::AppHandle) -> Option<String> {
    app.dialog().file().blocking_pick_folder().map(|p| p.to_string())
}

#[tauri::command]
async fn pick_zip(app: tauri::AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .add_filter("Mod archive", &["zip", "7z", "rar"])
        .blocking_pick_file()
        .map(|p| p.to_string())
}

#[tauri::command]
fn list_installed(spt_root: String) -> Result<InstalledList, String> {
    let root = PathBuf::from(&spt_root);
    if !root.is_dir() {
        return Err("SPT root folder does not exist.".into());
    }
    let manifests = load_manifests(&root);
    let mut all = Vec::new();
    scan_tree(&root, &manifests, true, &mut all);
    let disabled_base = root.join(".spt-mod-installer").join("disabled");
    scan_tree(&disabled_base, &manifests, false, &mut all);
    all.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let mut client = Vec::new();
    let mut server = Vec::new();
    for m in all {
        if m.kind == "client" { client.push(m); } else { server.push(m); }
    }
    Ok(InstalledList { client, server })
}

#[tauri::command]
fn uninstall_mod(spt_root: String, rel_path: String) -> Result<UninstallResult, String> {
    let root = PathBuf::from(&spt_root);
    if !root.is_dir() {
        return Err("SPT root folder does not exist.".into());
    }
    if rel_path.contains("..") || rel_path.trim().is_empty() {
        return Err("Invalid mod path.".into());
    }
    // Never allow removing SPT's own core client files.
    if let Some(last) = rel_path.trim_end_matches('/').rsplit('/').next() {
        let stem = last.strip_suffix(".dll").or_else(|| last.strip_suffix(".DLL")).unwrap_or(last);
        if (rel_path.starts_with("BepInEx/plugins/") || rel_path.starts_with("BepInEx/patchers/"))
            && is_spt_core_client(stem)
        {
            return Err("That's part of SPT itself and can't be removed here.".into());
        }
    }

    let manifests = load_manifests(&root);
    let prefix = format!("{}/", rel_path.trim_end_matches('/'));

    // Manifests that own files under this mod folder.
    let owning: Vec<&Manifest> = manifests
        .iter()
        .filter(|m| m.files.iter().any(|f| *f == rel_path || f.starts_with(&prefix)))
        .collect();

    let mut removed = Vec::new();

    // Disabled mod: its files are in the disabled store, so remove them there.
    let live = root.join(&rel_path);
    let disabled = root.join(".spt-mod-installer").join("disabled").join(&rel_path);
    if !live.exists() && disabled.exists() {
        if !disabled.starts_with(&root) {
            return Err("Refusing to delete outside the SPT folder.".into());
        }
        if disabled.is_dir() {
            fs::remove_dir_all(&disabled).map_err(|e| format!("Could not remove folder: {e}"))?;
        } else {
            fs::remove_file(&disabled).map_err(|e| format!("Could not remove file: {e}"))?;
        }
        removed.push(rel_path.clone());
        for m in &owning { let _ = fs::remove_file(&m.path); }
        return Ok(UninstallResult { removed, via_manifest: !owning.is_empty(), spanned_both: false });
    }

    if !owning.is_empty() {
        // App-installed: delete exactly the files recorded (covers combo mods
        // that span both BepInEx/ and SPT/user/mods/).
        let mut all_files: Vec<String> = Vec::new();
        let mut spanned_both = false;
        for m in &owning {
            let has_client = m.files.iter().any(|f| f.starts_with("BepInEx/"));
            let has_server = m.files.iter().any(|f| f.starts_with("SPT/"));
            if has_client && has_server {
                spanned_both = true;
            }
            for f in m.files.iter() {
                if !all_files.contains(f) {
                    all_files.push(f.clone());
                }
            }
        }
        let mut prune_targets: Vec<PathBuf> = Vec::new();
        for f in &all_files {
            if f.contains("..") {
                continue;
            }
            let target = root.join(f);
            if !target.starts_with(&root) {
                continue;
            }
            if target.is_file() {
                if fs::remove_file(&target).is_ok() {
                    removed.push(f.clone());
                    if let Some(par) = target.parent() {
                        prune_targets.push(par.to_path_buf());
                    }
                }
            }
        }
        for d in prune_targets {
            prune_empty_dirs(&root, d);
        }
        // Remove the consumed manifests.
        for m in &owning {
            let _ = fs::remove_file(&m.path);
        }
        Ok(UninstallResult { removed, via_manifest: true, spanned_both })
    } else {
        // External mod: remove just this folder / file.
        let target = root.join(&rel_path);
        if !target.starts_with(&root) {
            return Err("Refusing to delete outside the SPT folder.".into());
        }
        if target.is_dir() {
            fs::remove_dir_all(&target).map_err(|e| format!("Could not remove folder: {e}"))?;
            removed.push(rel_path.clone());
        } else if target.is_file() {
            fs::remove_file(&target).map_err(|e| format!("Could not remove file: {e}"))?;
            removed.push(rel_path.clone());
        } else {
            return Err("That mod no longer exists on disk.".into());
        }
        if let Some(par) = target.parent() {
            prune_empty_dirs(&root, par.to_path_buf());
        }
        Ok(UninstallResult { removed, via_manifest: false, spanned_both: false })
    }
}

// ---------------------------------------------------------------------------
// Phase 3a: install from a Forge mod-list URL
// ---------------------------------------------------------------------------

const USER_AGENT: &str = "spt-mod-installer/0.3 (SPT mod manager)";

#[derive(Serialize, Clone)]
struct ResolvedMod {
    id: u64,
    name: String,
    slug: String,
    version: Option<String>,
    link: Option<String>,
    size: Option<u64>,
    spt_constraint: Option<String>,
    note: Option<String>, // set when there's no usable download
}

/// Pull every distinct Forge mod id (+slug) from a list page's HTML, in order.
fn parse_list_mods(html: &str) -> Vec<(u64, String)> {
    let re = regex::Regex::new(r"forge\.sp-tarkov\.com/mod/(\d+)/([A-Za-z0-9_-]+)").unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(html) {
        let id: u64 = cap[1].parse().unwrap_or(0);
        let slug = cap[2].to_string();
        if id != 0 && seen.insert(id) {
            out.push((id, slug));
        }
    }
    out
}

/// "project-fika" -> "Project Fika"
fn prettify(slug: &str) -> String {
    slug.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Latest version of a mod: (version, download link, size, spt constraint).
/// Retries on rate-limit (429) / server errors so a busy API doesn't get
/// misreported as "no version".
async fn fetch_latest_version(
    client: &reqwest::Client,
    id: u64,
) -> Result<Option<(String, String, Option<u64>, Option<String>)>, String> {
    let url = format!(
        "https://forge.sp-tarkov.com/api/v0/mod/{id}/versions?sort=-created_at&per_page=1"
    );
    let mut attempt: u64 = 0;
    loop {
        attempt += 1;
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt < 4 {
                    tokio::time::sleep(std::time::Duration::from_millis(400 * attempt)).await;
                    continue;
                }
                return Err(e.to_string());
            }
        };
        let status = resp.status();
        if status.as_u16() == 429 || status.is_server_error() {
            if attempt < 6 {
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempt * attempt)).await;
                continue;
            }
            return Err(format!("rate limited (HTTP {})", status.as_u16()));
        }
        if !status.is_success() {
            return Err(format!("HTTP {}", status.as_u16()));
        }
        let text = resp.text().await.map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if let Some(first) = v.get("data").and_then(|d| d.as_array()).and_then(|a| a.first()) {
            let version = first.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let link = first.get("link").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let size = first.get("content_length").and_then(|x| x.as_u64());
            let cons = first.get("spt_version_constraint").and_then(|x| x.as_str()).map(|s| s.to_string());
            if link.is_empty() {
                return Ok(None);
            }
            return Ok(Some((version, link, size, cons)));
        }
        return Ok(None);
    }
}

#[tauri::command]
async fn resolve_modlist(app: tauri::AppHandle, url: String) -> Result<Vec<ResolvedMod>, String> {
    if !url.contains("forge.sp-tarkov.com/list/") {
        return Err("That doesn't look like a Forge mod-list URL (forge.sp-tarkov.com/list/...).".into());
    }
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;

    let html = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Could not load the list page: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;

    let mods = parse_list_mods(&html);
    if mods.is_empty() {
        return Err("No mods found on that page. Make sure the list is public and the URL is correct.".into());
    }

    let total = mods.len();
    let mut out = Vec::new();
    for (i, (id, slug)) in mods.into_iter().enumerate() {
        let _ = app.emit("resolve-progress", serde_json::json!({ "done": i, "total": total }));
        let mut rm = ResolvedMod {
            id,
            name: prettify(&slug),
            slug: slug.clone(),
            version: None,
            link: None,
            size: None,
            spt_constraint: None,
            note: None,
        };
        // Stay under the API's rate limit (~300/min).
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        match fetch_latest_version(&client, id).await {
            Ok(Some((ver, link, size, cons))) => {
                rm.version = Some(ver);
                rm.link = Some(link);
                rm.size = size;
                rm.spt_constraint = cons;
            }
            Ok(None) => rm.note = Some("No downloadable version found".into()),
            Err(e) => rm.note = Some(format!("Lookup failed: {e}")),
        }
        out.push(rm);
    }
    let _ = app.emit("resolve-progress", serde_json::json!({ "done": total, "total": total }));
    Ok(out)
}

/// Re-look up a single mod (used by the "Retry failed" button).
#[tauri::command]
async fn lookup_mod(id: u64, slug: String) -> Result<ResolvedMod, String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut rm = ResolvedMod {
        id,
        name: prettify(&slug),
        slug: slug.clone(),
        version: None,
        link: None,
        size: None,
        spt_constraint: None,
        note: None,
    };
    match fetch_latest_version(&client, id).await {
        Ok(Some((ver, link, size, cons))) => {
            rm.version = Some(ver);
            rm.link = Some(link);
            rm.size = size;
            rm.spt_constraint = cons;
        }
        Ok(None) => rm.note = Some("No downloadable version found".into()),
        Err(e) => rm.note = Some(format!("Lookup failed: {e}")),
    }
    Ok(rm)
}

#[tauri::command]
async fn download_and_install(
    app: tauri::AppHandle,
    key: String,
    name: String,
    url: String,
    spt_root: String,
    id: Option<u64>,
    version: Option<String>,
) -> Result<InstallResult, String> {
    use futures_util::StreamExt as _;
    use std::io::Write as _;

    let root = PathBuf::from(&spt_root);
    if !root.is_dir() {
        return Err("SPT root folder does not exist.".into());
    }
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client.get(&url).send().await.map_err(|e| format!("Download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Download failed: HTTP {}", resp.status().as_u16()));
    }
    let total = resp.content_length();

    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("sptdl-{}-{}.bin", now_millis(), seq));
    let mut file = fs::File::create(&tmp).map_err(|e| format!("temp file: {e}"))?;

    cancel_clear(&key);
    let mut stream = resp.bytes_stream();
    let mut received: u64 = 0;
    let mut last: u128 = 0;
    while let Some(item) = stream.next().await {
        if cancel_is_set(&key) {
            drop(file);
            let _ = fs::remove_file(&tmp);
            cancel_clear(&key);
            return Err("Cancelled".into());
        }
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                drop(file);
                let _ = fs::remove_file(&tmp);
                return Err(format!("Download error: {e}"));
            }
        };
        if let Err(e) = file.write_all(&chunk) {
            drop(file);
            let _ = fs::remove_file(&tmp);
            return Err(format!("Write error: {e}"));
        }
        received += chunk.len() as u64;
        let now = now_millis();
        if now - last > 150 {
            last = now;
            let _ = app.emit(
                "download-progress",
                serde_json::json!({ "key": key, "received": received, "total": total }),
            );
        }
    }
    drop(file);
    let _ = app.emit(
        "download-progress",
        serde_json::json!({ "key": key, "received": received, "total": total, "done": true }),
    );

    if cancel_is_set(&key) {
        let _ = fs::remove_file(&tmp);
        cancel_clear(&key);
        return Err("Cancelled".into());
    }

    // Run extraction/install on a blocking thread so an Abort can skip a mod that
    // is stuck or extremely slow to extract (the blocking work itself can't be
    // interrupted, but we stop waiting on it and move on, leaving the SPT folder
    // untouched since files are only copied after extraction completes).
    let src_tmp = tmp.clone();
    let inst_name = name.clone();
    let inst_root = root.clone();
    let inst_source = format!("{name} (mod list)");
    let handle = tokio::task::spawn_blocking(move || {
        build_install_path(&src_tmp, &inst_name, &inst_source, &inst_root)
    });
    let res: Result<InstallResult, String> = loop {
        if handle.is_finished() {
            break match handle.await {
                Ok(r) => r,
                Err(e) => Err(format!("Install task failed: {e}")),
            };
        }
        if cancel_is_set(&key) {
            handle.abort();
            cancel_clear(&key);
            let _ = fs::remove_file(&tmp);
            return Err("Cancelled".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    };
    let _ = fs::remove_file(&tmp);

    // Record the Forge id + version so update checks work, and drop any older
    // manifest for the same mod so we keep exactly one per Forge id.
    if let (Ok(install), Some(fid)) = (&res, id) {
        if let Ok(text) = fs::read_to_string(&install.manifest_path) {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) {
                v["forge_id"] = serde_json::json!(fid);
                if let Some(ver) = &version {
                    v["forge_version"] = serde_json::json!(ver);
                }
                let _ = fs::write(
                    &install.manifest_path,
                    serde_json::to_string_pretty(&v).unwrap_or(text),
                );
            }
        }
        for m in load_manifests(&root) {
            if m.forge_id == Some(fid) && m.path != PathBuf::from(&install.manifest_path) {
                let _ = fs::remove_file(&m.path);
            }
        }
    }
    cancel_clear(&key);
    res
}

// ---------------------------------------------------------------------------
// Download cancellation
// ---------------------------------------------------------------------------

static CANCELS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

fn cancel_is_set(key: &str) -> bool {
    CANCELS.lock().map(|s| s.contains(key)).unwrap_or(false)
}
fn cancel_clear(key: &str) {
    if let Ok(mut s) = CANCELS.lock() {
        s.remove(key);
    }
}

#[tauri::command]
fn cancel_download(key: String) {
    if let Ok(mut s) = CANCELS.lock() {
        s.insert(key);
    }
}

// ---------------------------------------------------------------------------
// Enable / disable + update checking
// ---------------------------------------------------------------------------

/// Move a mod's files between the live SPT tree and the disabled store.
/// Top-level mod folder/file that contains a given installed path.
fn mod_container(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.first() == Some(&"BepInEx") && parts.len() >= 3 {
        format!("{}/{}/{}", parts[0], parts[1], parts[2])
    } else if path.starts_with("SPT/user/mods/") && parts.len() >= 4 {
        format!("SPT/user/mods/{}", parts[3])
    } else {
        parts.first().map(|s| s.to_string()).unwrap_or_else(|| path.to_string())
    }
}

fn move_container(root: &Path, container: &str, enable: bool) -> bool {
    if container.contains("..") {
        return false;
    }
    let live = root.join(container);
    let disabled = root.join(".spt-mod-installer").join("disabled").join(container);
    if !live.starts_with(root) || !disabled.starts_with(root) {
        return false;
    }
    let (from, to) = if enable { (&disabled, &live) } else { (&live, &disabled) };
    if !from.exists() {
        return false; // already in the requested state
    }
    if let Some(parent) = to.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::rename(from, to).is_ok()
}

#[tauri::command]
fn toggle_mod(spt_root: String, rel_path: String, enable: bool) -> Result<(), String> {
    let root = PathBuf::from(&spt_root);
    if !root.is_dir() {
        return Err("SPT root folder does not exist.".into());
    }
    if rel_path.contains("..") || rel_path.trim().is_empty() {
        return Err("Invalid mod path.".into());
    }

    // If this mod was installed by the app, toggle EVERY folder it owns so a
    // combo mod's client and server halves move together.
    let manifests = load_manifests(&root);
    let mut containers: Vec<String> = Vec::new();
    if let Some(m) = manifest_for(&manifests, &rel_path) {
        for f in &m.files {
            let c = mod_container(f);
            if !containers.contains(&c) {
                containers.push(c);
            }
        }
    }
    if containers.is_empty() {
        containers.push(mod_container(&rel_path));
    }

    let mut moved = 0;
    for c in &containers {
        if move_container(&root, c, enable) {
            moved += 1;
        }
    }
    if moved == 0 {
        return Err("That mod's files were not found where expected.".into());
    }
    Ok(())
}

#[derive(Serialize)]
struct UpdateInfo {
    forge_id: u64,
    name: String,
    installed_version: String,
    latest_version: String,
    link: String,
    size: Option<u64>,
    changelog: Option<String>,
}

fn parse_ver(v: &str) -> Vec<u64> {
    v.split(|c: char| c == '.' || c == '-' || c == '+' || c == ' ')
        .filter_map(|part| {
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u64>().ok()
        })
        .collect()
}

/// True if `a` is a strictly newer version than `b`.
fn version_gt(a: &str, b: &str) -> bool {
    let pa = parse_ver(a);
    let pb = parse_ver(b);
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// Latest version metadata for update checks: (version, link, size, changelog).
async fn fetch_latest_full(
    client: &reqwest::Client,
    id: u64,
) -> Result<Option<(String, String, Option<u64>, Option<String>)>, String> {
    let url = format!(
        "https://forge.sp-tarkov.com/api/v0/mod/{id}/versions?sort=-created_at&per_page=1"
    );
    let mut attempt: u64 = 0;
    loop {
        attempt += 1;
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt < 4 {
                    tokio::time::sleep(std::time::Duration::from_millis(400 * attempt)).await;
                    continue;
                }
                return Err(e.to_string());
            }
        };
        let status = resp.status();
        if status.as_u16() == 429 || status.is_server_error() {
            if attempt < 6 {
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempt * attempt)).await;
                continue;
            }
            return Err(format!("rate limited (HTTP {})", status.as_u16()));
        }
        if !status.is_success() {
            return Err(format!("HTTP {}", status.as_u16()));
        }
        let text = resp.text().await.map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if let Some(first) = v.get("data").and_then(|d| d.as_array()).and_then(|a| a.first()) {
            let version = first.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let link = first.get("link").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let size = first.get("content_length").and_then(|x| x.as_u64());
            let changelog = first.get("description").and_then(|x| x.as_str()).map(|s| s.to_string());
            if version.is_empty() {
                return Ok(None);
            }
            return Ok(Some((version, link, size, changelog)));
        }
        return Ok(None);
    }
}

/// Check every app-installed mod (one manifest per Forge id) for a newer version.
#[tauri::command]
async fn check_updates(spt_root: String) -> Result<Vec<UpdateInfo>, String> {
    let root = PathBuf::from(&spt_root);
    if !root.is_dir() {
        return Err("SPT root folder does not exist.".into());
    }
    // One manifest per forge id (newest install wins).
    let mut latest_manifest: std::collections::HashMap<u64, (u128, String, String)> =
        std::collections::HashMap::new();
    for m in load_manifests(&root) {
        if let (Some(fid), Some(ver)) = (m.forge_id, m.forge_version.clone()) {
            let name = m
                .files
                .first()
                .cloned()
                .unwrap_or_default();
            let entry = latest_manifest.entry(fid).or_insert((0, String::new(), String::new()));
            if m.installed_at >= entry.0 {
                *entry = (m.installed_at, ver, name);
            }
        }
    }

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;

    let mut updates = Vec::new();
    for (fid, (_, installed_version, hint)) in latest_manifest {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        if let Ok(Some((latest, link, size, changelog))) = fetch_latest_full(&client, fid).await {
            if version_gt(&latest, &installed_version) {
                // derive a display name from the install path hint
                let parts: Vec<&str> = hint.split('/').collect();
                let name = if hint.starts_with("SPT/user/mods/") {
                    parts.get(3).copied()
                } else {
                    parts.get(2).copied()
                }
                .unwrap_or(hint.as_str())
                .to_string();
                updates.push(UpdateInfo {
                    forge_id: fid,
                    name,
                    installed_version,
                    latest_version: latest,
                    link,
                    size,
                    changelog,
                });
            }
        }
    }
    Ok(updates)
}

// ---------------------------------------------------------------------------
// Dependencies, SPT version, open-folder, disk size (Phase 4)
// ---------------------------------------------------------------------------

fn collect_deps(node: &serde_json::Value, have: &std::collections::HashSet<u64>,
                seen: &mut std::collections::HashSet<u64>, out: &mut Vec<ResolvedMod>) {
    if let Some(arr) = node.as_array() {
        for d in arr {
            let id = d.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
            if id != 0 && !have.contains(&id) && seen.insert(id) {
                let name = d.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let slug = d.get("slug").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let lcv = d.get("latest_compatible_version");
                let version = lcv.and_then(|x| x.get("version")).and_then(|x| x.as_str()).map(|s| s.to_string());
                let link = lcv.and_then(|x| x.get("link")).and_then(|x| x.as_str()).map(|s| s.to_string());
                let size = lcv.and_then(|x| x.get("content_length")).and_then(|x| x.as_u64());
                out.push(ResolvedMod {
                    id,
                    name: if name.is_empty() { prettify(&slug) } else { name },
                    slug,
                    version,
                    link,
                    size,
                    spt_constraint: None,
                    note: None,
                });
            }
            if let Some(deps) = d.get("dependencies") {
                collect_deps(deps, have, seen, out);
            }
        }
    }
}

/// Resolve the full dependency tree for a set of (mod_id, version) pairs and
/// return only the dependencies that are NOT already installed or already in
/// the set being installed (so nothing is installed twice).
#[tauri::command]
async fn resolve_dependencies(spt_root: String, mods: Vec<(u64, String)>) -> Result<Vec<ResolvedMod>, String> {
    if mods.is_empty() {
        return Ok(Vec::new());
    }
    let root = PathBuf::from(&spt_root);
    let mut have: std::collections::HashSet<u64> =
        load_manifests(&root).iter().filter_map(|m| m.forge_id).collect();
    for (id, _) in &mods {
        have.insert(*id);
    }
    let param = mods.iter().map(|(id, v)| format!("{id}:{v}")).collect::<Vec<_>>().join(",");
    let client = reqwest::Client::builder().user_agent(USER_AGENT).build().map_err(|e| e.to_string())?;
    let text = client
        .get("https://forge.sp-tarkov.com/api/v0/mods/dependencies")
        .query(&[("mods", param.as_str())])
        .send()
        .await
        .map_err(|e| format!("Dependency lookup failed: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    collect_deps(v.get("data").unwrap_or(&serde_json::Value::Null), &have, &mut seen, &mut out);
    // Only keep deps that actually have a download link.
    out.retain(|m| m.link.is_some());
    Ok(out)
}

/// Best-effort read of the installed SPT version from the server core config.
#[tauri::command]
fn detect_spt_version(spt_root: String) -> Option<String> {
    let root = PathBuf::from(&spt_root);
    let candidates = [
        root.join("SPT_Data").join("Server").join("configs").join("core.json"),
        root.join("SPT").join("SPT_Data").join("Server").join("configs").join("core.json"),
    ];
    for c in candidates {
        if let Ok(text) = fs::read_to_string(&c) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                for key in ["sptVersion", "akiVersion", "version"] {
                    if let Some(sv) = v.get(key).and_then(|x| x.as_str()) {
                        return Some(sv.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Open a mod's folder in the OS file manager (live or disabled location).
#[tauri::command]
fn open_mod_folder(spt_root: String, rel_path: String) -> Result<(), String> {
    let root = PathBuf::from(&spt_root);
    if rel_path.contains("..") || rel_path.trim().is_empty() {
        return Err("Invalid mod path.".into());
    }
    let mut target = root.join(&rel_path);
    if !target.exists() {
        target = root.join(".spt-mod-installer").join("disabled").join(&rel_path);
    }
    if !target.exists() {
        return Err("Mod files not found.".into());
    }
    let to_open = if target.is_dir() {
        target.clone()
    } else {
        target.parent().map(|p| p.to_path_buf()).unwrap_or(target.clone())
    };
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer").arg(&to_open).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(&to_open).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(&to_open).spawn().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Serialize)]
struct SearchResult {
    id: u64,
    name: String,
    slug: String,
    teaser: String,
    thumbnail: String,
    downloads: u64,
    fika: bool,
    author: String,
    category: String,
    featured: bool,
    version: Option<String>,
    link: Option<String>,
    size: Option<u64>,
    spt_constraint: Option<String>,
}

#[derive(Serialize)]
struct ModMeta {
    id: u64,
    thumbnail: String,
    teaser: String,
    author: String,
    downloads: u64,
    category: String,
}

fn parse_search_mod(m: &serde_json::Value) -> Option<SearchResult> {
    let id = m.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
    if id == 0 {
        return None;
    }
    let name = m.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let slug = m.get("slug").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let latest = m.get("versions").and_then(|x| x.as_array()).and_then(|a| a.first());
    Some(SearchResult {
        id,
        name: if name.is_empty() { prettify(&slug) } else { name },
        slug,
        teaser: m.get("teaser").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        thumbnail: m.get("thumbnail").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        downloads: m.get("downloads").and_then(|x| x.as_u64()).unwrap_or(0),
        fika: m.get("fika_compatibility").and_then(|x| x.as_bool()).unwrap_or(false),
        author: m.get("owner").and_then(|o| o.get("name")).and_then(|x| x.as_str()).unwrap_or("").to_string(),
        category: m.get("category").and_then(|c| c.get("title")).and_then(|x| x.as_str()).unwrap_or("").to_string(),
        featured: m.get("featured").and_then(|x| x.as_bool()).unwrap_or(false),
        version: latest.and_then(|v| v.get("version")).and_then(|x| x.as_str()).map(|s| s.to_string()),
        link: latest.and_then(|v| v.get("link")).and_then(|x| x.as_str()).map(|s| s.to_string()),
        size: latest.and_then(|v| v.get("content_length")).and_then(|x| x.as_u64()),
        spt_constraint: latest.and_then(|v| v.get("spt_version_constraint")).and_then(|x| x.as_str()).map(|s| s.to_string()),
    })
}

/// Search the Forge catalogue by name (fuzzy). Includes each mod's latest
/// version so the UI can show/sort by SPT compatibility and add without an
/// extra lookup.
#[tauri::command]
async fn search_mods(query: String) -> Result<Vec<SearchResult>, String> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::Client::builder().user_agent(USER_AGENT).build().map_err(|e| e.to_string())?;
    let text = client
        .get("https://forge.sp-tarkov.com/api/v0/mods")
        .query(&[
            ("filter[name]", query.as_str()),
            ("include", "versions"),
            ("per_page", "20"),
        ])
        .send()
        .await
        .map_err(|e| format!("Search failed: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for m in arr {
            if let Some(r) = parse_search_mod(m) {
                out.push(r);
            }
        }
    }
    Ok(out)
}

/// Download a thumbnail and return it as a base64 data URL (so it can be cached
/// and shown offline).
async fn download_thumb(client: &reqwest::Client, url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    let bytes = match client.get(url).send().await {
        Ok(r) => match r.bytes().await { Ok(b) => b, Err(_) => return String::new() },
        Err(_) => return String::new(),
    };
    let lower = url.to_lowercase();
    let mime = if lower.contains(".png") { "image/png" }
        else if lower.contains(".webp") { "image/webp" }
        else if lower.contains(".gif") { "image/gif" }
        else { "image/jpeg" };
    use base64::Engine;
    format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(&bytes))
}

/// Get display metadata (thumbnail data-url, author, downloads, category) for a
/// set of mod ids, cached on disk under .spt-mod-installer/meta-cache.json and
/// only re-fetched from the Forge once per day. Thumbnails are cached too, so
/// after the first load the Manage list is instant and works offline.
#[tauri::command]
async fn get_mod_meta(spt_root: String, ids: Vec<u64>) -> Result<Vec<ModMeta>, String> {
    let root = PathBuf::from(&spt_root);
    let cache_dir = root.join(".spt-mod-installer");
    let cache_path = cache_dir.join("meta-cache.json");
    let mut cache: serde_json::Value = fs::read_to_string(&cache_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !cache.is_object() {
        cache = serde_json::json!({});
    }

    let now = now_millis() as u64;
    let day: u64 = 24 * 60 * 60 * 1000;
    let stale: Vec<u64> = ids
        .iter()
        .copied()
        .filter(|id| {
            let key = id.to_string();
            let fresh = cache
                .get(key.as_str())
                .and_then(|e| e.get("fetched_at"))
                .and_then(|x| x.as_u64())
                .map(|f| now.saturating_sub(f) < day)
                .unwrap_or(false);
            !fresh
        })
        .collect();

    if !stale.is_empty() {
        let client = reqwest::Client::builder().user_agent(USER_AGENT).build().map_err(|e| e.to_string())?;
        let idparam = stale.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        if let Ok(resp) = client
            .get("https://forge.sp-tarkov.com/api/v0/mods")
            .query(&[("filter[id]", idparam.as_str()), ("per_page", "100")])
            .send()
            .await
        {
            if let Ok(text) = resp.text().await {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
                        for m in arr {
                            let id = m.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
                            if id == 0 {
                                continue;
                            }
                            let thumb_url = m.get("thumbnail").and_then(|x| x.as_str()).unwrap_or("");
                            let thumb_data = download_thumb(&client, thumb_url).await;
                            cache[id.to_string().as_str()] = serde_json::json!({
                                "thumbnail": thumb_data,
                                "teaser": m.get("teaser").and_then(|x| x.as_str()).unwrap_or(""),
                                "author": m.get("owner").and_then(|o| o.get("name")).and_then(|x| x.as_str()).unwrap_or(""),
                                "downloads": m.get("downloads").and_then(|x| x.as_u64()).unwrap_or(0),
                                "category": m.get("category").and_then(|c| c.get("title")).and_then(|x| x.as_str()).unwrap_or(""),
                                "fetched_at": now,
                            });
                        }
                    }
                }
            }
        }
        let _ = fs::create_dir_all(&cache_dir);
        let _ = fs::write(&cache_path, serde_json::to_string(&cache).unwrap_or_default());
    }

    let mut out = Vec::new();
    for id in &ids {
        if let Some(e) = cache.get(id.to_string().as_str()) {
            out.push(ModMeta {
                id: *id,
                thumbnail: e.get("thumbnail").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                teaser: e.get("teaser").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                author: e.get("author").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                downloads: e.get("downloads").and_then(|x| x.as_u64()).unwrap_or(0),
                category: e.get("category").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            });
        }
    }
    Ok(out)
}#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Portable: make the exe's own folder the working directory on launch.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let _ = std::env::set_current_dir(dir);
        }
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            inspect_zip,
            inspect_zip_bytes,
            install_zip,
            install_zip_bytes,
            validate_spt_root,
            default_spt_root,
            pick_spt_root,
            pick_zip,
            list_installed,
            uninstall_mod,
            resolve_modlist,
            lookup_mod,
            download_and_install,
            toggle_mod,
            check_updates,
            cancel_download,
            resolve_dependencies,
            detect_spt_version,
            open_mod_folder,
            search_mods,
            get_mod_meta,
            temp_new,
            temp_append,
            install_local
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ---------------------------------------------------------------------------
// Tests for the resolver (the riskiest logic). Run: cargo test
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn targets(names: &[&str], mod_name: &str) -> Vec<(String, String)> {
        let v: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        resolve_all(&v, mod_name)
            .mapped
            .into_iter()
            .map(|m| (m.target, m.category))
            .collect()
    }

    #[test]
    fn well_formed_combo() {
        let t = targets(&["SPT/user/mods/x/package.json", "BepInEx/plugins/y.dll"], "m");
        assert!(t.contains(&("SPT/user/mods/x/package.json".into(), "server".into())));
        assert!(t.contains(&("BepInEx/plugins/y.dll".into(), "client".into())));
    }

    #[test]
    fn wrapper_folder_is_stripped() {
        let t = targets(&["Mod-1.0/SPT/user/mods/x/package.json", "Mod-1.0/BepInEx/plugins/y.dll"], "m");
        assert!(t.contains(&("SPT/user/mods/x/package.json".into(), "server".into())));
        assert!(t.contains(&("BepInEx/plugins/y.dll".into(), "client".into())));
    }

    #[test]
    fn bare_user_gets_spt_prefix() {
        let t = targets(&["user/mods/x/package.json"], "m");
        assert_eq!(t, vec![("SPT/user/mods/x/package.json".into(), "server".into())]);
    }

    #[test]
    fn bare_plugins_gets_bepinex_prefix() {
        let t = targets(&["plugins/x.dll"], "m");
        assert_eq!(t, vec![("BepInEx/plugins/x.dll".into(), "client".into())]);
    }

    #[test]
    fn bare_patchers_gets_bepinex_prefix() {
        let t = targets(&["patchers/pre.dll"], "m");
        assert_eq!(t, vec![("BepInEx/patchers/pre.dll".into(), "client".into())]);
    }

    #[test]
    fn bare_server_folder() {
        let t = targets(&["Awesome/package.json", "Awesome/src/mod.js"], "Awesome");
        assert!(t.iter().all(|(p, c)| p.starts_with("SPT/user/mods/Awesome/") && c == "server"));
    }

    #[test]
    fn bare_server_at_root_uses_modname() {
        let t = targets(&["package.json", "src/mod.js"], "RootMod");
        assert!(t.contains(&("SPT/user/mods/RootMod/package.json".into(), "server".into())));
    }

    #[test]
    fn loose_dll_is_client() {
        let t = targets(&["cool.dll"], "Cool");
        assert_eq!(t, vec![("BepInEx/plugins/cool.dll".into(), "client".into())]);
    }

    #[test]
    fn bare_client_folder() {
        let t = targets(&["Cool/cool.dll", "Cool/cfg.json"], "Cool");
        assert!(t.contains(&("BepInEx/plugins/Cool/cool.dll".into(), "client".into())));
        assert!(t.contains(&("BepInEx/plugins/Cool/cfg.json".into(), "client".into())));
    }

    #[test]
    fn lowercase_bepinex_and_config() {
        let t = targets(&["bepinex/plugins/x.dll", "bepinex/config/x.cfg"], "m");
        assert!(t.contains(&("BepInEx/plugins/x.dll".into(), "client".into())));
        assert!(t.contains(&("BepInEx/config/x.cfg".into(), "client".into())));
    }

    #[test]
    fn backslashes_normalised() {
        let t = targets(&["Wrapper\\BepInEx\\plugins\\x.dll"], "m");
        assert_eq!(t, vec![("BepInEx/plugins/x.dll".into(), "client".into())]);
    }

    #[test]
    fn bare_exe_goes_to_root() {
        let t = targets(&["FikaSync.exe"], "FikaSync");
        assert_eq!(t, vec![("FikaSync.exe".into(), "client".into())]);
    }

    #[test]
    fn wrapped_exe_goes_to_root_by_name() {
        let t = targets(&["FikaSync/FikaSync.exe"], "FikaSync");
        assert_eq!(t, vec![("FikaSync.exe".into(), "client".into())]);
    }

    #[test]
    fn traversal_rejected() {
        let v = vec!["SPT/../../evil".to_string()];
        let r = resolve_all(&v, "m");
        assert!(r.mapped.is_empty());
        assert_eq!(r.unrecognized, vec!["SPT/../../evil".to_string()]);
    }

    #[test]
    fn bare_mods_folder_is_server() {
        let t = targets(&["mods/Solarint-SAIN-ServerMod/SAINServerMod.dll"], "m");
        assert_eq!(
            t,
            vec![("SPT/user/mods/Solarint-SAIN-ServerMod/SAINServerMod.dll".into(), "server".into())]
        );
    }

    #[test]
    fn sain_combo_is_mixed() {
        let t = targets(&["BepInEx/plugins/SAIN/SAIN.dll", "mods/X/server.dll"], "m");
        assert!(t.contains(&("BepInEx/plugins/SAIN/SAIN.dll".into(), "client".into())));
        assert!(t.contains(&("SPT/user/mods/X/server.dll".into(), "server".into())));
    }

    #[test]
    fn mods_does_not_hijack_bepinex_paths() {
        let t = targets(&["BepInEx/plugins/mymod/mods/cfg.json"], "m");
        assert_eq!(t, vec![("BepInEx/plugins/mymod/mods/cfg.json".into(), "client".into())]);
    }
}
