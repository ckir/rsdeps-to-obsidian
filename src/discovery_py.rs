//! Python project discovery.
//!
//! [`discover_py`] maps a Python project directory to a
//! [`DiscoveredWorkspace`] with one [`Package`], one `Lib` [`Target`] at the
//! import root, one `Bin` [`Target`] per top-level script / `__main__`
//! module / `[project.scripts]` entry, and versioned [`ExternalDep`]s from
//! `==` pins and lockfiles. Discovery is file-only: it never executes
//! project code and never shells out to pip/poetry/uv.
//!
//! Rules (brief-governed, spec §2):
//! - Root: an explicit `manifest_path` wins (its parent dir); otherwise the
//!   nearest ancestor dir containing `pyproject.toml`, else the nearest
//!   ancestor containing `setup.py` / `setup.cfg` / `requirements*.txt`,
//!   else the workspace input dir itself.
//! - Package name: `[project] name` from `pyproject.toml`, fallback to the
//!   root dir name. `manifest_path`: `pyproject.toml` if present, else
//!   `setup.py` if present, else the root itself.
//! - Import root: `src/` when that dir exists (src-layout wins; flat `.py`
//!   files are ignored for the `Lib` target), else the root (flat-layout).
//! - Bins: every top-level (`root/*.py`) script outside packages (except
//!   `__init__.py`, which marks a package rather than a script) plus every
//!   `__main__.py` under the import root (named `__main__`; a second one
//!   with the same name is dropped, first sorted path wins) use the file
//!   stem as the target name. Each `[project.scripts]` entry contributes a
//!   Bin too: when `foo = "pkg.mod:main"` resolves to an existing module
//!   file (`<import-root>/pkg/mod.py` or `<import-root>/pkg/mod/__init__.py`)
//!   the Bin is named `foo` (overwriting a file-scan Bin on collision);
//!   otherwise it falls back to the module stem (`mod`) with a best-effort
//!   (possibly nonexistent) path that never replaces a file-scan Bin
//!   (first-real-wins). Entries whose dot-segments are not all valid Python
//!   identifiers are skipped silently. The table is parsed
//!   only, never executed.
//! - Versions: `==` pins from every `requirements*.txt` in the root and
//!   from `[project] dependencies`, plus `name`/`version` pairs from each
//!   present lockfile (`poetry.lock`, `pdm.lock`, `uv.lock`, all TOML
//!   `[[package]]` tables). Includes (`-r`), editables (`-e`), options
//!   (`--*`), comments, markers (`; ...`), bare names, and non-`==`
//!   specifiers yield no version; the resolver reports those as `EXT001`.
//! - Malformed `pyproject.toml` or lockfiles are hard errors (failing
//!   closed beats a silently wrong graph).

use crate::config::Config;
use crate::domain::{Package, Target, TargetKind};
use crate::ids::{make_target_id, PackageId};
use crate::metadata::{DiscoveredWorkspace, ExternalDep};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Lockfiles consulted for `name`/`version` pairs, in read order. All three
/// are TOML with `[[package]]` entries; when several are present their pairs
/// merge (deduplicated, sorted).
const LOCKFILES: [&str; 3] = ["poetry.lock", "pdm.lock", "uv.lock"];

/// A `requirements*.txt` file is any root entry whose name starts with
/// `requirements` and ends with `.txt` (`requirements.txt`,
/// `requirements-dev.txt`, ...), plus a bare `requirements` file.
fn is_requirements_file(name: &str) -> bool {
    name == "requirements" || (name.starts_with("requirements") && name.ends_with(".txt"))
}

/// Lexically absolutize `path` against `cwd` and normalize `.`/`..`
/// without touching the filesystem.
fn absolutize(path: &Path, cwd: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        out
    }
}

/// Canonicalize when the path exists (fixtures and real projects do);
/// otherwise fall back to lexical absolutization so discovery never fails
/// on a missing dir.
fn best_root(path: &Path, cwd: &Path) -> PathBuf {
    let abs = absolutize(path, cwd);
    std::fs::canonicalize(&abs).unwrap_or(abs)
}

/// Direct children of `dir` that are files, sorted by file name for
/// determinism. Unreadable dirs yield no entries; discovery never fails.
fn sorted_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files
}

/// Direct children of `dir` that are dirs, sorted by file name. Symlinked
/// dirs are excluded so the recursive `__main__.py` walk cannot cycle.
fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_dir()
                && !p
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_symlink())
        })
        .collect();
    dirs.sort();
    dirs
}

fn dir_has_pyproject(dir: &Path) -> bool {
    dir.join("pyproject.toml").is_file()
}

/// `setup.py`, `setup.cfg`, or any `requirements*.txt` directly under `dir`.
fn dir_has_setup_marker(dir: &Path) -> bool {
    if dir.join("setup.py").is_file() || dir.join("setup.cfg").is_file() {
        return true;
    }
    sorted_files(dir).iter().any(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_requirements_file)
    })
}

/// Nearest ancestor of `start` (inclusive) satisfying `pred`, or `None`.
fn nearest_ancestor(start: &Path, pred: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if pred(&dir) {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Parse a TOML document, tagging errors with the file path. (Note:
/// `toml::from_str` is the document parser; `Value::from_str` parses a
/// single TOML *value* and rejects table headers.)
fn parse_toml_file(path: &Path) -> Result<toml::Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    toml::from_str::<toml::Value>(&text)
        .map_err(|e| format!("invalid TOML in {}: {e}", path.display()))
}

fn table<'v>(value: &'v toml::Value, key: &str) -> Option<&'v toml::Table> {
    value.get(key)?.as_table()
}

fn str_field<'v>(table: &'v toml::Table, key: &str) -> Option<&'v str> {
    table.get(key)?.as_str()
}

/// Extract one `(name, version)` pin from a requirement string. Only `==`
/// pins qualify: options (`-r`, `-e`, `--*`), comments, markers (`; ...`),
/// extras (`name[...]`, stripped before matching), and non-`==`
/// specifiers yield `None`.
fn parse_pin(req: &str) -> Option<(String, String)> {
    let line = req.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
        return None;
    }
    // Inline comments and environment markers cannot contain `==` pins.
    let line = line.split('#').next().unwrap_or("").trim();
    let line = line.split(';').next().unwrap_or("").trim();
    let (name_part, version_part) = line.split_once("==")?;
    let name = name_part.trim();
    let name = name.split('[').next().unwrap_or("").trim();
    // The version runs to the next specifier separator (`,`, whitespace).
    let version = version_part
        .trim()
        .split([',', ' ', '\t'])
        .next()
        .unwrap_or("")
        .trim();
    if name.is_empty()
        || version.is_empty()
        || name
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '=' | '<' | '>' | '~' | ';' | ','))
        || version
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '=' | '<' | '>' | '~' | ';' | ','))
    {
        return None;
    }
    Some((name.to_string(), version.to_string()))
}

/// Join `physically continued` (`\`-suffixed) lines, then extract `==` pins.
/// `-r` includes are deliberately NOT followed (file-only, no graph walk).
fn pins_from_requirements_text(text: &str) -> Vec<(String, String)> {
    let mut logical = Vec::new();
    let mut current = String::new();
    for raw in text.lines() {
        let stripped = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(head) = stripped.strip_suffix('\\') {
            current.push_str(head);
            continue;
        }
        current.push_str(stripped);
        logical.push(std::mem::take(&mut current));
    }
    if !current.is_empty() {
        logical.push(current);
    }
    logical.iter().filter_map(|l| parse_pin(l)).collect()
}

/// Recursively collect `__main__.py` files under `dir`, sorted.
fn collect_main_modules(dir: &Path, out: &mut Vec<PathBuf>) {
    for file in sorted_files(dir) {
        if file.file_name().and_then(|n| n.to_str()) == Some("__main__.py") {
            out.push(file);
        }
    }
    for sub in sorted_dirs(dir) {
        collect_main_modules(&sub, out);
    }
}

/// Whether `segment` is a valid Python identifier (ASCII approximation:
/// non-empty, `[A-Za-z0-9_]`, not starting with a digit). The `[project.scripts]`
/// guess path joins dot-segments into a relative path, so anything else
/// (`/etc/passwd`, `..`, empty segments) could escape the import root and
/// must be skipped instead.
fn is_py_ident(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Resolve `pkg.mod` to `<import_root>/pkg/mod.py`, else
/// `<import_root>/pkg/mod/__init__.py`, else `None`.
fn resolve_module_file(import_root: &Path, dotted: &str) -> Option<PathBuf> {
    if dotted.is_empty() || dotted.split('.').any(|s| s.is_empty()) {
        return None;
    }
    let mut rel = PathBuf::new();
    for part in dotted.split('.') {
        rel.push(part);
    }
    let as_file = import_root.join(&rel).with_extension("py");
    if as_file.is_file() {
        return Some(as_file);
    }
    let as_pkg = import_root.join(&rel).join("__init__.py");
    if as_pkg.is_file() {
        return Some(as_pkg);
    }
    None
}

/// Discover the Python project for `config` (see module docs for rules).
pub fn discover_py(config: &Config) -> Result<DiscoveredWorkspace, String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot determine current directory: {e}"))?;

    // Explicit `--manifest-path` wins: the root is its parent dir.
    let root = match &config.manifest_path {
        Some(manifest) => {
            let abs = absolutize(manifest, &cwd);
            let parent = abs.parent().map(Path::to_path_buf).unwrap_or(abs);
            best_root(&parent, &cwd)
        }
        None => {
            let start = best_root(&config.workspace_input, &cwd);
            nearest_ancestor(&start, dir_has_pyproject)
                .or_else(|| nearest_ancestor(&start, dir_has_setup_marker))
                .unwrap_or(start)
        }
    };

    // Package name: `[project] name`, fallback to the root dir name.
    let pyproject_path = root.join("pyproject.toml");
    let pyproject: Option<toml::Value> = if pyproject_path.is_file() {
        Some(parse_toml_file(&pyproject_path)?)
    } else {
        None
    };
    let project_name: Option<String> = pyproject
        .as_ref()
        .and_then(|v| table(v, "project"))
        .and_then(|t| str_field(t, "name"))
        .map(str::to_string);
    let pkg_name = project_name.unwrap_or_else(|| {
        root.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("root")
            .to_string()
    });
    let package_id = PackageId(pkg_name.clone());

    let manifest_path = if pyproject_path.is_file() {
        pyproject_path
    } else {
        let setup_py = root.join("setup.py");
        if setup_py.is_file() {
            setup_py
        } else {
            root.clone()
        }
    };

    // Import root: `src/` wins when present, else the root itself.
    let src_dir = root.join("src");
    let import_root = if src_dir.is_dir() {
        src_dir
    } else {
        root.clone()
    };

    // One Lib target at the import root.
    let lib_id = make_target_id(&pkg_name, TargetKind::Lib.kind_token(), &pkg_name)?;
    let mut bins: BTreeMap<String, PathBuf> = BTreeMap::new();

    // Top-level `root/*.py` scripts outside packages (`__init__.py`
    // excluded: it marks a package, it is not a script).
    for file in sorted_files(&root) {
        let is_py = file.extension().and_then(|e| e.to_str()) == Some("py");
        let is_init = file.file_name().and_then(|n| n.to_str()) == Some("__init__.py");
        if !is_py || is_init {
            continue;
        }
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if !stem.is_empty() {
            bins.entry(stem).or_insert(file);
        }
    }

    // Every `__main__.py` under the import root (first sorted path wins
    // per name; today every one maps to the single name `__main__`).
    let mut mains = Vec::new();
    collect_main_modules(&import_root, &mut mains);
    for main in mains {
        bins.entry("__main__".to_string()).or_insert(main);
    }

    // `[project.scripts]`: script name when the module file exists, else
    // the module stem. Script names overwrite file-scan Bins on collision.
    if let Some(scripts) = pyproject
        .as_ref()
        .and_then(|v| table(v, "project"))
        .and_then(|t| t.get("scripts"))
        .and_then(|s| s.as_table())
    {
        let mut names: Vec<&String> = scripts.keys().collect();
        names.sort();
        for name in names {
            let Some(value) = scripts.get(name).and_then(|v| v.as_str()) else {
                continue;
            };
            let value = value.trim();
            // `pkg.mod:func`, `pkg.mod`, or `pkg.mod[extra]:func` — the
            // callable after `:` and any extras are naming-only.
            let dotted = value
                .split(':')
                .next()
                .unwrap_or("")
                .split('[')
                .next()
                .unwrap_or("")
                .trim();
            let stem = dotted.rsplit('.').next().unwrap_or("").to_string();
            if stem.is_empty() {
                continue;
            }
            // Unvalidated dot-segments could escape the import root on the
            // guess path (`/etc/passwd:main` joins absolute, `..` climbs
            // out), so entries with a non-identifier segment are skipped
            // silently — the same treatment malformed values already get.
            if !dotted.split('.').all(is_py_ident) {
                continue;
            }
            match resolve_module_file(&import_root, dotted) {
                Some(path) => {
                    bins.insert(name.clone(), path);
                }
                None => {
                    let guess = {
                        let mut rel = PathBuf::new();
                        for part in dotted.split('.') {
                            rel.push(part);
                        }
                        import_root.join(&rel).with_extension("py")
                    };
                    // First-real-wins: a live file-scan Bin on this stem is
                    // never replaced by a missing-module guess.
                    bins.entry(stem).or_insert(guess);
                }
            }
        }
    }

    let mut targets = Vec::new();
    targets.push(Target {
        id: lib_id,
        package_id: package_id.clone(),
        name: pkg_name.clone(),
        kind: TargetKind::Lib,
        root_source: import_root.clone(),
    });
    for (name, path) in &bins {
        targets.push(Target {
            id: make_target_id(&pkg_name, TargetKind::Bin.kind_token(), name)?,
            package_id: package_id.clone(),
            name: name.clone(),
            kind: TargetKind::Bin,
            root_source: path.clone(),
        });
    }
    targets.sort_by(|a, b| {
        (&a.package_id, a.kind.kind_token(), &a.name).cmp(&(
            &b.package_id,
            b.kind.kind_token(),
            &b.name,
        ))
    });

    // Versions: `==` pins from every `requirements*.txt` in the root plus
    // `[project] dependencies`, merged with lockfile pairs.
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    for file in sorted_files(&root) {
        let is_req = file
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_requirements_file);
        if !is_req {
            continue;
        }
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        for pair in pins_from_requirements_text(&text) {
            pairs.insert(pair);
        }
    }
    if let Some(deps) = pyproject
        .as_ref()
        .and_then(|v| table(v, "project"))
        .and_then(|t| t.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for dep in deps {
            if let Some(req) = dep.as_str() {
                if let Some(pair) = parse_pin(req) {
                    pairs.insert(pair);
                }
            }
        }
    }
    for lock in LOCKFILES {
        let path = root.join(lock);
        if !path.is_file() {
            continue;
        }
        let value = parse_toml_file(&path)?;
        let packages = value
            .get("package")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();
        for pkg in &packages {
            let entry = pkg.as_table();
            let name = entry.and_then(|t| str_field(t, "name"));
            let version = entry.and_then(|t| str_field(t, "version"));
            if let (Some(name), Some(version)) = (name, version) {
                pairs.insert((name.to_string(), version.to_string()));
            }
        }
    }
    let external_deps: Vec<ExternalDep> = pairs
        .into_iter()
        .map(|(name, version)| ExternalDep { name, version })
        .collect();

    Ok(DiscoveredWorkspace {
        root,
        packages: vec![Package {
            id: package_id,
            name: pkg_name,
            manifest_path,
        }],
        targets,
        external_deps,
    })
}
