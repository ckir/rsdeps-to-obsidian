//! TypeScript project discovery.
//!
//! [`discover_ts`] maps a TypeScript project directory to a
//! [`DiscoveredWorkspace`] with one [`Package`], one `Lib` [`Target`] per
//! `tsconfig*.json` file, one `Bin` [`Target`] per `package.json` `bin`
//! entry, and versioned [`ExternalDep`]s from lockfiles plus the top-level
//! `node_modules` scan. Discovery is file-only: it never executes project
//! code and never shells out to npm/yarn.
//!
//! Rules (brief-governed, spec §3):
//! - Root: an explicit `manifest_path` wins (its parent dir); otherwise the
//!   nearest ancestor dir containing `package.json`, else the nearest
//!   ancestor containing `tsconfig.json`, else the workspace input dir
//!   itself.
//! - Package name: the `package.json` `name` kept verbatim (scoped
//!   `@org/web` included — `/` is legal in IDs, only `::` is structural),
//!   fallback to the root dir name. `manifest_path`: `package.json` if
//!   present, else `tsconfig.json` if present, else the root itself.
//! - Lib targets: every `tsconfig.json` / `tsconfig.<stem>.json` file under
//!   the root, except anything under a `node_modules/` dir (other
//!   `tsconfig*.json` spellings are not tsconfigs and are ignored). The
//!   root `tsconfig.json` takes the package short name (the part after the
//!   last `/`, so `@org/web` yields target `web`); every
//!   `tsconfig.<stem>.json` takes `<stem>` (a non-root plain
//!   `tsconfig.json` shares the short name). Same-name collisions across
//!   dirs resolve first-sorted-path-wins with no diagnostic. `root_source`
//!   is `compilerOptions.rootDir` joined to the tsconfig dir when present
//!   (lexical join, existence not required), else the tsconfig dir.
//! - Bins: each `package.json` `bin` entry is a file path — the object form
//!   maps command name to path, and the bare-string form is one executable
//!   named after the package short name. Only `.js`/`.mjs`/`.cjs` values
//!   are executables (non-JS and non-string values are out, silently); a
//!   missing file is still emitted (the resolver fails closed downstream).
//! - Versions: lockfiles in read order (`package-lock.json`,
//!   `npm-shrinkwrap.json`, then `yarn.lock`) fill a name→version map
//!   first-wins; the top-level `node_modules/*/package.json` scan
//!   (`@scope/` children included — a scope dir is an install level, not
//!   nesting; nested `node_modules` ignored as a documented approximation)
//!   fills names the lockfiles missed. A name with no version anywhere is
//!   absent (the resolver reports it as `EXT001`).
//! - Malformed `package.json`, lockfile JSON, or any matched tsconfig is a
//!   hard path-tagged error (failing closed beats a silently wrong graph).
//!   Lockfile *shape* quirks (wrong-type tables, entries without versions)
//!   and unreadable/malformed `node_modules` copies are skipped silently:
//!   shape is best-effort, syntax is strict. `yarn.lock` is parsed
//!   best-effort (v1 `version "..."` lines; anything else ignored).
//! - Tsconfigs are parsed with `serde_json` only (per brief, no new deps),
//!   so JSONC conveniences (comments, trailing commas) are hard errors,
//!   not supported syntax.

use crate::config::Config;
use crate::domain::{Package, Target, TargetKind};
use crate::ids::{make_target_id, PackageId};
use crate::metadata::{DiscoveredWorkspace, ExternalDep};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Lockfiles consulted for `name`/`version` pairs, in read order.
/// First-wins per name across files.
const LOCKFILES: [&str; 2] = ["package-lock.json", "npm-shrinkwrap.json"];

/// Executable extensions for `bin` values. Anything else (shell scripts,
/// extensionless shims, compiled binaries) is out by design.
const BIN_EXTENSIONS: [&str; 3] = ["js", "mjs", "cjs"];

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

/// Direct children of `dir` that are files, sorted for determinism.
/// Unreadable dirs yield no entries; discovery never fails.
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

/// Direct children of `dir` that are dirs, sorted for determinism.
/// Symlinked dirs are excluded so the recursive walk cannot cycle.
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

/// Direct children of `dir` that are dirs (symlinks followed), sorted.
/// Only used for the shallow `node_modules` scan, which never recurses, so
/// cycles are impossible; linked installs (`npm link`, workspaces) still
/// count.
fn sorted_module_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn dir_has_package_json(dir: &Path) -> bool {
    dir.join("package.json").is_file()
}

fn dir_has_tsconfig(dir: &Path) -> bool {
    dir.join("tsconfig.json").is_file()
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

/// Parse a JSON document, tagging errors with the file path.
fn parse_json_file(path: &Path) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|e| format!("invalid JSON in {}: {e}", path.display()))
}

/// The package short name: the part after the last `/` (`@org/web` →
/// `web`; unscoped names are unchanged). Used for the root `Lib` target
/// and the bare-string `bin` form so scoped packages get usable names.
fn short_name(pkg: &str) -> &str {
    pkg.rsplit('/').next().unwrap_or(pkg)
}

/// Classify a file name as a tsconfig project file: `tsconfig.json` (the
/// root form, `None`) or `tsconfig.<stem>.json` (`Some(stem)`, stem
/// non-empty). Anything else matching the `tsconfig*` glob is ignored.
fn tsconfig_stem(file_name: &str) -> Option<Option<String>> {
    if file_name == "tsconfig.json" {
        return Some(None);
    }
    let rest = file_name.strip_prefix("tsconfig.")?;
    let stem = rest.strip_suffix(".json")?;
    if stem.is_empty() {
        return None;
    }
    Some(Some(stem.to_string()))
}

/// Recursively collect tsconfig project files under `dir`, sorted.
/// `node_modules/` subtrees are never descended into.
fn collect_tsconfigs(dir: &Path, out: &mut Vec<PathBuf>) {
    for file in sorted_files(dir) {
        let is_tsconfig = file
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| tsconfig_stem(n).is_some());
        if is_tsconfig {
            out.push(file);
        }
    }
    for sub in sorted_dirs(dir) {
        let is_modules = sub
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == "node_modules");
        if is_modules {
            continue;
        }
        collect_tsconfigs(&sub, out);
    }
}

/// Derive the installed package name from a `packages`-table key:
/// `node_modules/<name>` or `node_modules/@scope/<name>`. Anything deeper
/// is a nested install and out of scope.
fn lock_package_name(key: &str) -> Option<String> {
    let rest = key.strip_prefix("node_modules/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [name] if !name.is_empty() => Some((*name).to_string()),
        [scope, name] if scope.starts_with('@') && !scope[1..].is_empty() && !name.is_empty() => {
            Some(rest.to_string())
        }
        _ => None,
    }
}

/// Merge `name`/`version` pairs from an npm JSON lockfile (`package-lock`
/// or `npm-shrinkwrap`, both shapes) into `out`, first-wins per name.
/// Present-but-misshapen tables and versionless entries are skipped.
fn versions_from_npm_lock(value: &serde_json::Value, out: &mut BTreeMap<String, String>) {
    if let Some(packages) = value.get("packages").and_then(|p| p.as_object()) {
        let mut keys: Vec<&String> = packages.keys().collect();
        keys.sort();
        for key in keys {
            let Some(name) = lock_package_name(key) else {
                continue;
            };
            let version = packages
                .get(key)
                .and_then(|e| e.get("version"))
                .and_then(|v| v.as_str());
            if let Some(version) = version {
                out.entry(name).or_insert_with(|| version.to_string());
            }
        }
    }
    // Legacy (lockfileVersion 1) top-level `dependencies` table only;
    // per-entry nested `dependencies` are transitive copies, not roots.
    if let Some(deps) = value.get("dependencies").and_then(|d| d.as_object()) {
        let mut names: Vec<&String> = deps.keys().collect();
        names.sort();
        for name in names {
            let version = deps
                .get(name)
                .and_then(|e| e.get("version"))
                .and_then(|v| v.as_str());
            if let Some(version) = version {
                out.entry((*name).clone())
                    .or_insert_with(|| version.to_string());
            }
        }
    }
}

/// Derive the depended-on package name from one yarn.lock v1 header key
/// (`lodash@^4.17.20`, `@scope/util@^1.0.0`): the part before the last
/// `@` past position zero.
fn yarn_key_name(key: &str) -> Option<String> {
    let first = key.split(',').next()?.trim().trim_matches('"').trim();
    let idx = first.rfind('@')?;
    if idx == 0 {
        return None;
    }
    let name = first[..idx].trim_matches('"');
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// Merge `name`/`version` pairs from a yarn.lock v1 document into `out`,
/// first-wins per name. Line-oriented best-effort: unrecognized lines are
/// ignored, never errors.
fn versions_from_yarn_lock(text: &str, out: &mut BTreeMap<String, String>) {
    let mut pending: Option<String> = None;
    let mut version_taken = false;
    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            match line.strip_suffix(':') {
                Some(head) => {
                    pending = yarn_key_name(head);
                    version_taken = false;
                }
                None => pending = None,
            }
            continue;
        }
        if version_taken || pending.is_none() {
            continue;
        }
        let mut words = line.split_whitespace();
        if words.next() == Some("version") {
            if let Some(quoted) = words.next() {
                if let Some(version) = quoted.strip_prefix('"').and_then(|q| q.strip_suffix('"')) {
                    if !version.is_empty() {
                        if let Some(name) = pending.clone() {
                            out.entry(name).or_insert_with(|| version.to_string());
                        }
                        version_taken = true;
                    }
                }
            }
        }
    }
}

/// Merge `name`/`version` pairs from the top-level `node_modules` scan
/// into `out`, filling only names the lockfiles missed. `@scope/` children
/// are install roots (one extra level); anything deeper is nesting and is
/// never visited. Unreadable or malformed copies are skipped silently.
fn scan_node_modules(root: &Path, out: &mut BTreeMap<String, String>) {
    let mut read_copy = |dir: &Path| {
        let manifest = dir.join("package.json");
        if !manifest.is_file() {
            return;
        }
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        let name = value.get("name").and_then(|n| n.as_str());
        let version = value.get("version").and_then(|v| v.as_str());
        if let (Some(name), Some(version)) = (name, version) {
            out.entry(name.to_string())
                .or_insert_with(|| version.to_string());
        }
    };
    let modules = root.join("node_modules");
    for entry in sorted_module_dirs(&modules) {
        read_copy(&entry);
        let is_scope = entry
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('@'));
        if is_scope {
            for child in sorted_module_dirs(&entry) {
                read_copy(&child);
            }
        }
    }
}

/// Discover the TypeScript project for `config` (see module docs for rules).
pub fn discover_ts(config: &Config) -> Result<DiscoveredWorkspace, String> {
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
            nearest_ancestor(&start, dir_has_package_json)
                .or_else(|| nearest_ancestor(&start, dir_has_tsconfig))
                .unwrap_or(start)
        }
    };

    // Package name: `package.json` `name` verbatim, fallback to the root
    // dir name. A present-but-malformed manifest fails closed.
    let package_path = root.join("package.json");
    let manifest: Option<serde_json::Value> = if package_path.is_file() {
        Some(parse_json_file(&package_path)?)
    } else {
        None
    };
    let pkg_name = manifest
        .as_ref()
        .and_then(|v| v.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            root.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("root")
                .to_string()
        });
    let package_id = PackageId(pkg_name.clone());

    let manifest_path = if package_path.is_file() {
        package_path
    } else {
        let tsconfig_path = root.join("tsconfig.json");
        if tsconfig_path.is_file() {
            tsconfig_path
        } else {
            root.clone()
        }
    };

    // One Lib target per tsconfig project file. Every file is parsed up
    // front so a malformed tsconfig fails closed even where its target
    // would lose the same-name tiebreak; then first sorted path wins per
    // name with no diagnostic.
    let mut configs = Vec::new();
    collect_tsconfigs(&root, &mut configs);
    configs.sort();
    let mut parsed: Vec<(PathBuf, serde_json::Value)> = Vec::with_capacity(configs.len());
    for path in &configs {
        parsed.push((path.clone(), parse_json_file(path)?));
    }
    let mut libs: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (path, value) in &parsed {
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.clone());
        let stem = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(tsconfig_stem)
            .expect("collected paths are tsconfigs");
        let name = match stem {
            None => short_name(&pkg_name).to_string(),
            Some(stem) => stem,
        };
        let root_source = value
            .get("compilerOptions")
            .and_then(|c| c.get("rootDir"))
            .and_then(|r| r.as_str())
            .map(|r| absolutize(Path::new(r), &dir))
            .unwrap_or(dir);
        libs.entry(name).or_insert(root_source);
    }

    // Bin targets from the `package.json` `bin` table (string values are
    // file paths, and so are the bare-string form's); only JS executables
    // count, and missing files are emitted anyway for the resolver.
    let mut bins: BTreeMap<String, PathBuf> = BTreeMap::new();
    if let Some(manifest) = manifest.as_ref() {
        match manifest.get("bin") {
            Some(serde_json::Value::String(path)) => {
                let resolved = absolutize(Path::new(path), &root);
                let is_js = resolved
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| BIN_EXTENSIONS.contains(&e));
                if is_js {
                    bins.insert(short_name(&pkg_name).to_string(), resolved);
                }
            }
            Some(serde_json::Value::Object(table)) => {
                let mut names: Vec<&String> = table.keys().collect();
                names.sort();
                for name in names {
                    let path = table.get(name).and_then(|v| v.as_str());
                    let Some(path) = path else { continue };
                    let resolved = absolutize(Path::new(path), &root);
                    let is_js = resolved
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| BIN_EXTENSIONS.contains(&e));
                    if is_js {
                        bins.insert((*name).clone(), resolved);
                    }
                }
            }
            _ => {}
        }
    }

    let mut targets = Vec::new();
    for (name, path) in &libs {
        targets.push(Target {
            id: make_target_id(&pkg_name, TargetKind::Lib.kind_token(), name)?,
            package_id: package_id.clone(),
            name: name.clone(),
            kind: TargetKind::Lib,
            root_source: path.clone(),
        });
    }
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

    // Versions: lockfiles first (listed order, first-wins per name), then
    // the top-level `node_modules` scan for names the lockfiles missed.
    let mut versions: BTreeMap<String, String> = BTreeMap::new();
    for lock in LOCKFILES {
        let path = root.join(lock);
        if !path.is_file() {
            continue;
        }
        let value = parse_json_file(&path)?;
        versions_from_npm_lock(&value, &mut versions);
    }
    let yarn_path = root.join("yarn.lock");
    if yarn_path.is_file() {
        let text = std::fs::read_to_string(&yarn_path)
            .map_err(|e| format!("cannot read {}: {e}", yarn_path.display()))?;
        versions_from_yarn_lock(&text, &mut versions);
    }
    scan_node_modules(&root, &mut versions);
    let external_deps: Vec<ExternalDep> = versions
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
