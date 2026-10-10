//! C# project discovery.
//!
//! [`discover_cs`] maps a .NET workspace directory to a
//! [`DiscoveredWorkspace`] with one [`Package`] per discovered `*.csproj`,
//! `Lib`/`Bin` [`Target`]s per project, and versioned [`ExternalDep`]s from
//! exact pins and lockfiles. Discovery is file-only: it never executes
//! project code and never shells out to `dotnet`.
//!
//! Rules (brief-governed, spec sections 2, 5, 6, 9.7):
//! - Root: an explicit `manifest_path` wins (its parent dir); otherwise the
//!   walk starts at the workspace input dir and goes DOWN (never above the
//!   input root): the nearest `*.sln` level supplies member projects, else
//!   the nearest `*.csproj` level supplies the root; with no marker at all
//!   the root is the input dir itself. The root is the dir holding the
//!   nearest marker (shared by every marker at that depth when they agree,
//!   else the input dir); projects outside the root are out of scope.
//! - Members: every `*.csproj` referenced by a nearest-level `.sln`
//!   (`Project(...) = "Name", "path\to\Proj.csproj", ...` lines; backslashes
//!   tolerated; non-csproj entries ignored; dangling references skipped),
//!   unioned across the slns at that level and deduped, PLUS every loose
//!   `*.csproj` under the root (each its own package). Same-name packages
//!   across dirs resolve first-sorted-path-wins with no diagnostic
//!   (TypeScript tsconfig precedent).
//! - Package name: `<AssemblyName>` when present, else the csproj file stem.
//!   `manifest_path` is the csproj path.
//! - Lib targets: one per library project, or one per (project, TFM) with a
//!   `-<tfm>` suffix ONLY when `<TargetFrameworks>` holds several
//!   `;`-separated entries (split, trimmed, empties dropped, otherwise
//!   verbatim, sorted). `root_source` is the project dir. The TFM list is
//!   encoded in the target NAME only: the resolver re-reads the csproj per
//!   target (TypeScript tsconfig re-read precedent), so no struct changes.
//! - Bin targets: `OutputType=Exe` (case-insensitive, and literally `Exe`)
//!   yields `{pkg}::bin::{name}` with `root_source` set to the project dir;
//!   Exe projects get Bin targets ONLY (all their modules belong to the Bin
//!   tree; Rust `main.rs` precedent) while libraries get Lib targets ONLY.
//!   A multi-TFM Exe splits per TFM like Lib. Bin targets are emitted for
//!   every Exe project even when no `.cs` sources exist (missing-file
//!   targets are the resolver's fail-closed business, mirroring the Python
//!   miss-arm and TS bin-miss rules).
//! - Versions: exact-pin `Version` attributes on `PackageReference`, plus
//!   `resolved` entries from each project dir's `packages.lock.json`, plus
//!   `Version`/`VersionOverride` from `PackageVersion` (and central
//!   `PackageReference Update`) entries in every `Directory.Packages.props`
//!   from the project dir up to the root, plus legacy per-project
//!   `packages.config` entries. Floating/range/undefined versions yield no
//!   entry (the resolver reports those as unresolved downstream).
//!   `Directory.Build.props` is deliberately NEVER read (spec section 9.7
//!   documented non-goal): versions defined only there stay absent.
//! - Malformed consumed XML/JSON manifests are hard path-tagged errors
//!   (fail-closed beats a silently wrong graph, mirroring the tsconfig
//!   rule); lock/config SHAPE quirks (missing tables, versionless entries)
//!   are skipped silently.

use crate::config::Config;
use crate::domain::{Package, Target, TargetKind};
use crate::ids::{make_target_id, PackageId};
use crate::metadata::{DiscoveredWorkspace, ExternalDep};
use quick_xml::events::{BytesStart, Event};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Lexically absolutize `path` against `cwd` and normalize `.`/`..`
/// without touching the filesystem.
fn absolutize(path: &Path, cwd: &Path) -> PathBuf {
    normalize(&if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    })
}

/// Lexically normalize `path` (resolve `.`/`..` without touching the
/// filesystem) so sln-relative references compare equal to walked paths.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
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

/// Recursively collect files with extension `ext` (case-insensitive) under
/// `dir`. `obj/`, `bin/`, `.git/`, and `.vs/` subtrees are never descended
/// into (build outputs and tooling caches, not sources).
fn collect_by_ext(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    for file in sorted_files(dir) {
        if file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        {
            out.push(file);
        }
    }
    for sub in sorted_dirs(dir) {
        let skip = sub.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
            n.eq_ignore_ascii_case("obj")
                || n.eq_ignore_ascii_case("bin")
                || n == ".git"
                || n == ".vs"
        });
        if !skip {
            collect_by_ext(&sub, ext, out);
        }
    }
}

/// Depth of `path` relative to `start` (components below `start`).
fn depth_under(path: &Path, start: &Path) -> usize {
    path.strip_prefix(start)
        .map(|r| r.components().count())
        .unwrap_or(usize::MAX)
}

/// Root for a nearest-marker level: the marker dir when every nearest
/// marker agrees, else the walk `start`.
fn level_root(nearest: &[&PathBuf], start: &Path) -> PathBuf {
    let mut dirs = nearest.iter().map(|p| {
        p.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| start.to_path_buf())
    });
    let Some(first) = dirs.next() else {
        return start.to_path_buf();
    };
    if dirs.all(|d| d == first) {
        first
    } else {
        start.to_path_buf()
    }
}

/// Lowercased element/attribute name (MSBuild XML is case-insensitive).
fn lower(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_lowercase()
}

enum XmlEvent {
    Start(String, Vec<(String, String)>),
    Empty(String, Vec<(String, String)>),
    Text(String),
    End(String),
}

/// Attributes of a start/empty element as lowercased `(key, value)` pairs.
fn attrs_of(element: &BytesStart, path: &Path) -> Result<Vec<(String, String)>, String> {
    let mut attrs = Vec::new();
    for attr in element.attributes() {
        let attr = attr.map_err(|e| format!("invalid XML in {}: {e}", path.display()))?;
        let value = attr
            .unescape_value()
            .map_err(|e| format!("invalid XML in {}: {e}", path.display()))?
            .into_owned();
        attrs.push((lower(attr.key.as_ref()), value));
    }
    Ok(attrs)
}

/// Pull `path` into a flat event list, enforcing well-formedness beyond
/// what `quick-xml` rejects on its own: mismatched end tags, unclosed
/// elements at EOF, and documents with no root element are all hard errors.
/// Text and attribute values are unescaped but otherwise verbatim.
fn read_xml_events(path: &Path) -> Result<Vec<XmlEvent>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut buf = Vec::new();
    let mut events = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut saw_root = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(format!("invalid XML in {}: {e}", path.display())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let name = lower(e.local_name().as_ref());
                saw_root = true;
                stack.push(name.clone());
                events.push(XmlEvent::Start(name, attrs_of(&e, path)?));
            }
            Ok(Event::Empty(e)) => {
                let name = lower(e.local_name().as_ref());
                saw_root = true;
                events.push(XmlEvent::Empty(name, attrs_of(&e, path)?));
            }
            Ok(Event::End(e)) => {
                let name = lower(e.local_name().as_ref());
                match stack.pop() {
                    Some(open) if open == name => {}
                    _ => {
                        return Err(format!(
                            "invalid XML in {}: mismatched </{name}>",
                            path.display()
                        ));
                    }
                }
                events.push(XmlEvent::End(name));
            }
            Ok(Event::Text(e)) => {
                let text = e
                    .xml_content()
                    .map_err(|e| format!("invalid XML in {}: {e}", path.display()))?;
                events.push(XmlEvent::Text(text.into_owned()));
            }
            Ok(_) => {}
        }
        buf.clear();
    }
    if !saw_root {
        return Err(format!(
            "invalid XML in {}: no root element",
            path.display()
        ));
    }
    if let Some(open) = stack.pop() {
        return Err(format!(
            "invalid XML in {}: unclosed <{open}>",
            path.display()
        ));
    }
    Ok(events)
}

/// One attribute value by lowercased key.
fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

struct CsProjInfo {
    assembly_name: Option<String>,
    tfms_plural: Option<String>,
    output_type: Option<String>,
    package_refs: Vec<(String, String)>,
}

/// The fields discovery needs from one csproj: `<AssemblyName>`,
/// `<TargetFrameworks>` (plural only; a single TFM needs no name suffix),
/// `<OutputType>`, and `PackageReference` `Include`/`Version` attributes.
/// Only the first `<AssemblyName>`/`<TargetFrameworks>`/`<OutputType>`
/// occurrence counts. Child `<Version>` elements are NOT read (attributes
/// only, per the brief).
fn parse_csproj(path: &Path) -> Result<CsProjInfo, String> {
    let mut info = CsProjInfo {
        assembly_name: None,
        tfms_plural: None,
        output_type: None,
        package_refs: Vec::new(),
    };
    let mut capture: Option<&str> = None;
    let mut buf = String::new();
    for event in read_xml_events(path)? {
        match event {
            XmlEvent::Start(name, attrs) => {
                if name == "packagereference" {
                    if let Some(id) = attr(&attrs, "include").or_else(|| attr(&attrs, "update")) {
                        if !id.trim().is_empty() {
                            let version = attr(&attrs, "version").unwrap_or("").to_string();
                            info.package_refs.push((id.trim().to_string(), version));
                        }
                    }
                }
                if capture.is_none() {
                    if name == "assemblyname" && info.assembly_name.is_none() {
                        capture = Some("assembly");
                        buf.clear();
                    } else if name == "targetframeworks" && info.tfms_plural.is_none() {
                        capture = Some("tfms");
                        buf.clear();
                    } else if name == "outputtype" && info.output_type.is_none() {
                        capture = Some("output");
                        buf.clear();
                    }
                }
            }
            XmlEvent::Empty(name, attrs) => {
                if name == "packagereference" {
                    if let Some(id) = attr(&attrs, "include").or_else(|| attr(&attrs, "update")) {
                        if !id.trim().is_empty() {
                            let version = attr(&attrs, "version").unwrap_or("").to_string();
                            info.package_refs.push((id.trim().to_string(), version));
                        }
                    }
                }
            }
            XmlEvent::Text(text) => {
                if capture.is_some() {
                    buf.push_str(&text);
                }
            }
            XmlEvent::End(name) => {
                let done = match capture {
                    Some("assembly") => name == "assemblyname",
                    Some("tfms") => name == "targetframeworks",
                    Some("output") => name == "outputtype",
                    _ => false,
                };
                if done {
                    let value = buf.trim().to_string();
                    match capture {
                        Some("assembly") => info.assembly_name = Some(value),
                        Some("tfms") => info.tfms_plural = Some(value),
                        Some("output") => info.output_type = Some(value),
                        _ => {}
                    }
                    capture = None;
                }
            }
        }
    }
    Ok(info)
}

/// Split a `<TargetFrameworks>` value on `;`: trimmed, empties dropped,
/// otherwise verbatim.
fn split_tfms(raw: &str) -> Vec<String> {
    raw.split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether `version` is an exact pin. Anything floating, ranged, or
/// undefined (`*`, brackets, parens, commas, whitespace, empty) is not.
fn is_exact_version(version: &str) -> bool {
    let version = version.trim();
    !version.is_empty()
        && !version
            .chars()
            .any(|c| matches!(c, '*' | '(' | ')' | '[' | ']' | ',' | ' ' | '\t'))
}

/// Member csproj paths (absolute, normalized) from one `.sln` file:
/// best-effort line scan of `Project(...) = "Name", "path", ...` entries.
/// Non-`.csproj` entries are ignored; references pointing at no file are
/// skipped silently (a stale solution entry is not a discovered project).
fn parse_sln_members(sln_path: &Path) -> Result<Vec<PathBuf>, String> {
    let text = std::fs::read_to_string(sln_path)
        .map_err(|e| format!("cannot read {}: {e}", sln_path.display()))?;
    let dir = sln_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut members = BTreeSet::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("Project(") else {
            continue;
        };
        let Some(eq) = rest.find('=') else {
            continue;
        };
        let mut parts = rest[eq + 1..].split(',');
        parts.next();
        let Some(raw) = parts.next() else {
            continue;
        };
        let raw = raw.trim().trim_matches('"');
        if raw.is_empty() || !raw.to_lowercase().ends_with(".csproj") {
            continue;
        }
        let joined = normalize(&dir.join(raw.replace('\\', "/")));
        if joined.is_file() {
            members.insert(joined);
        }
    }
    Ok(members.into_iter().collect())
}

/// `Directory.Packages.props` files from `project_dir` up to `root`
/// (inclusive, nearest first; never above the input root).
fn props_chain(project_dir: &Path, root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dir = project_dir.to_path_buf();
    loop {
        let candidate = dir.join("Directory.Packages.props");
        if candidate.is_file() {
            out.push(candidate);
        }
        if dir == *root || !dir.starts_with(root) {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    out
}

/// `(name, version)` pairs from central package management:
/// `<PackageVersion Include>` and central `<PackageReference Update>`
/// entries; `VersionOverride` wins over `Version` when both are present.
fn parse_packages_props(path: &Path) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for event in read_xml_events(path)? {
        match event {
            XmlEvent::Start(name, attrs) | XmlEvent::Empty(name, attrs) => {
                if name == "packageversion" || name == "packagereference" {
                    let id = attr(&attrs, "include").or_else(|| attr(&attrs, "update"));
                    let version =
                        attr(&attrs, "versionoverride").or_else(|| attr(&attrs, "version"));
                    if let (Some(id), Some(version)) = (id, version) {
                        if !id.trim().is_empty() && !version.trim().is_empty() {
                            out.push((id.trim().to_string(), version.trim().to_string()));
                        }
                    }
                }
            }
            XmlEvent::Text(_) | XmlEvent::End(_) => {}
        }
    }
    Ok(out)
}

/// `(id, version)` pairs from a legacy `packages.config` (`<package>`
/// elements). Entries missing id/version are skipped silently.
fn parse_packages_config(path: &Path) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for event in read_xml_events(path)? {
        match event {
            XmlEvent::Start(name, attrs) | XmlEvent::Empty(name, attrs) => {
                if name == "package" {
                    if let (Some(id), Some(version)) = (attr(&attrs, "id"), attr(&attrs, "version"))
                    {
                        if !id.trim().is_empty() && !version.trim().is_empty() {
                            out.push((id.trim().to_string(), version.trim().to_string()));
                        }
                    }
                }
            }
            XmlEvent::Text(_) | XmlEvent::End(_) => {}
        }
    }
    Ok(out)
}

/// `(name, resolved)` pairs from a NuGet `packages.lock.json` document
/// (`targets.<tfm>.<"Name/Version">.resolved`). Missing tables and entries
/// without a `resolved` string are skipped silently (shape best-effort).
fn lock_versions(value: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(targets) = value.get("targets").and_then(|t| t.as_object()) else {
        return out;
    };
    let mut tfms: Vec<&String> = targets.keys().collect();
    tfms.sort();
    for tfm in tfms {
        let Some(entries) = targets.get(tfm).and_then(|e| e.as_object()) else {
            continue;
        };
        let mut keys: Vec<&String> = entries.keys().collect();
        keys.sort();
        for key in keys {
            let Some((name, _)) = key.rsplit_once('/') else {
                continue;
            };
            let resolved = entries
                .get(key)
                .and_then(|e| e.get("resolved"))
                .and_then(|r| r.as_str());
            if let Some(resolved) = resolved {
                if !name.is_empty() && !resolved.is_empty() {
                    out.push((name.to_string(), resolved.to_string()));
                }
            }
        }
    }
    out
}

/// Parse a JSON document, tagging errors with the file path.
fn parse_json_file(path: &Path) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|e| format!("invalid JSON in {}: {e}", path.display()))
}

/// Discover the C# workspace for `config` (see module docs for rules).
pub fn discover_cs(config: &Config) -> Result<DiscoveredWorkspace, String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot determine current directory: {e}"))?;

    // Explicit `--manifest-path` wins: the walk starts at its parent dir.
    let start = match &config.manifest_path {
        Some(manifest) => {
            let abs = absolutize(manifest, &cwd);
            let parent = abs.parent().map(Path::to_path_buf).unwrap_or(abs);
            best_root(&parent, &cwd)
        }
        None => best_root(&config.workspace_input, &cwd),
    };

    let mut slns = Vec::new();
    collect_by_ext(&start, "sln", &mut slns);
    slns.sort();
    let mut found = Vec::new();
    collect_by_ext(&start, "csproj", &mut found);
    found.sort();

    // Root + member set per the root rule (see module docs): nearest-level
    // slns contribute their unioned members; loose csproj files under the
    // root join them (deduped by path).
    let mut projects = BTreeSet::new();
    let root = if !slns.is_empty() {
        let min = slns
            .iter()
            .map(|s| depth_under(s, &start))
            .min()
            .unwrap_or(0);
        let near: Vec<&PathBuf> = slns
            .iter()
            .filter(|s| depth_under(s, &start) == min)
            .collect();
        let root = level_root(&near, &start);
        for sln in &near {
            for member in parse_sln_members(sln)? {
                projects.insert(member);
            }
        }
        root
    } else if !found.is_empty() {
        let min = found
            .iter()
            .map(|c| depth_under(c, &start))
            .min()
            .unwrap_or(0);
        let near: Vec<&PathBuf> = found
            .iter()
            .filter(|c| depth_under(c, &start) == min)
            .collect();
        level_root(&near, &start)
    } else {
        start.clone()
    };
    for csproj in &found {
        if csproj.starts_with(&root) {
            projects.insert(csproj.clone());
        }
    }

    let mut packages = Vec::new();
    let mut targets = Vec::new();
    let mut seen_names: BTreeSet<String> = BTreeSet::new();
    let mut externals: BTreeSet<(String, String)> = BTreeSet::new();

    for csproj in &projects {
        let info = parse_csproj(csproj)?;
        let stem = csproj
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("project");
        let pkg_name = info
            .assembly_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(stem)
            .to_string();
        // Same-name packages across dirs: first sorted path wins, no
        // diagnostic (TypeScript tsconfig precedent).
        if !seen_names.insert(pkg_name.clone()) {
            continue;
        }
        let package_id = PackageId(pkg_name.clone());
        let project_dir = csproj
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.clone());

        // One target per (project, TFM) with a `-<tfm>` suffix ONLY when
        // multi-targeted; single-target projects keep the plain name. The
        // TFM list is encoded in the name only — the resolver re-reads the
        // csproj per target (TypeScript tsconfig re-read precedent).
        let mut tfms = info
            .tfms_plural
            .as_deref()
            .map(split_tfms)
            .unwrap_or_default();
        tfms.sort();
        tfms.dedup();
        let is_exe = info
            .output_type
            .as_deref()
            .is_some_and(|o| o.eq_ignore_ascii_case("exe"));
        // Exe projects yield Bin targets ONLY; libraries yield Lib ONLY.
        let kind = if is_exe {
            TargetKind::Bin
        } else {
            TargetKind::Lib
        };
        let target_names: Vec<String> = if tfms.len() > 1 {
            tfms.iter().map(|tfm| format!("{pkg_name}-{tfm}")).collect()
        } else {
            vec![pkg_name.clone()]
        };
        for name in &target_names {
            targets.push(Target {
                id: make_target_id(&pkg_name, kind.kind_token(), name)
                    .map_err(|e| format!("{}: {e}", csproj.display()))?,
                package_id: package_id.clone(),
                name: name.clone(),
                kind: kind.clone(),
                root_source: project_dir.clone(),
            });
        }
        packages.push(Package {
            id: package_id,
            name: pkg_name,
            manifest_path: csproj.clone(),
        });

        // Versions (exact pins only; anything else stays absent for the
        // resolver's unresolved diagnostic). `Directory.Build.props` is
        // deliberately NEVER read (spec section 9.7 documented non-goal).
        for (name, version) in &info.package_refs {
            if is_exact_version(version) {
                externals.insert((name.clone(), version.trim().to_string()));
            }
        }
        let lock_path = project_dir.join("packages.lock.json");
        if lock_path.is_file() {
            for (name, version) in lock_versions(&parse_json_file(&lock_path)?) {
                if is_exact_version(&version) {
                    externals.insert((name, version));
                }
            }
        }
        for props in props_chain(&project_dir, &root) {
            for (name, version) in parse_packages_props(&props)? {
                if is_exact_version(&version) {
                    externals.insert((name, version));
                }
            }
        }
        let legacy = project_dir.join("packages.config");
        if legacy.is_file() {
            for (name, version) in parse_packages_config(&legacy)? {
                if is_exact_version(&version) {
                    externals.insert((name, version));
                }
            }
        }
    }

    packages.sort_by(|a, b| a.id.cmp(&b.id));
    targets.sort_by(|a, b| {
        (&a.package_id, a.kind.kind_token(), &a.name).cmp(&(
            &b.package_id,
            b.kind.kind_token(),
            &b.name,
        ))
    });
    let external_deps = externals
        .into_iter()
        .map(|(name, version)| ExternalDep { name, version })
        .collect();

    Ok(DiscoveredWorkspace {
        root,
        packages,
        targets,
        external_deps,
    })
}
