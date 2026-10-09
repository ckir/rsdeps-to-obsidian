use crate::ids::ModuleId;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// Reserved Windows device names (case-insensitive), matched against the
/// stem base (portion before the first `.`).
fn is_reserved_base(name: &str) -> bool {
    let base: &str = match name.find('.') {
        Some(i) => &name[..i],
        None => name,
    };
    matches!(
        base.to_ascii_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "COM4" | "COM5"
            | "COM6" | "COM7" | "COM8" | "COM9" | "LPT1" | "LPT2" | "LPT3"
            | "LPT4" | "LPT5" | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    )
}

/// Map a logical module ID to its generated Markdown filename: split on
/// `::`, encode each literal `_` as `___` within the segment, join with
/// `__`, append `.md`.
pub fn module_filename(module_id: &ModuleId) -> String {
    module_id
        .0
        .split("::")
        .map(|seg| seg.replace('_', "___"))
        .collect::<Vec<_>>()
        .join("__")
        + ".md"
}

/// Apply the Windows-safe-name policy (§14.3) to an encoded stem, in order:
/// (1) double a leading `_`, (2) prefix a reserved base with `_`,
/// (3) map a trailing space/period to `_`, (4) on collision with `taken`
/// (or a reserved result) prepend `_` until distinct. The result is
/// inserted into `taken`. Case is otherwise preserved; no hashes/counters.
pub fn windows_safe_name(encoded_stem: &str, taken: &mut HashSet<String>) -> String {
    let mut name = encoded_stem.to_string();
    if name.starts_with('_') {
        name.insert(0, '_');
    }
    if is_reserved_base(&name) {
        name.insert(0, '_');
    }
    while name.ends_with(' ') || name.ends_with('.') {
        name.pop();
        name.push('_');
    }
    while taken.contains(&name) || is_reserved_base(&name) {
        name.insert(0, '_');
    }
    taken.insert(name.clone());
    name
}

fn reject_symlink_chain(top: &Path, bottom: &Path) -> Result<(), String> {
    // Walk existing prefixes of `bottom` at or below `top`; any symlink
    // component is fatal. Prefixes above `top` are out of scope (the
    // caller's environment, e.g. /tmp on macOS) and are not inspected.
    let mut current = bottom.to_path_buf();
    loop {
        if !current.starts_with(top) {
            break;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err("unsafe output path".to_string());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("unsafe output path".to_string()),
        }
        if current == *top {
            break;
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    Ok(())
}

/// Resolve the output root: relative `raw` joins `cwd`; every existing
/// chain component is inspected and any symlink is fatal with
/// `Err("unsafe output path")`. An existing root is canonicalized; a
/// missing root canonicalizes the nearest existing non-symlink parent and
/// appends the lexical tail.
pub fn resolve_output_root(raw: &Path, cwd: &Path) -> Result<PathBuf, String> {
    let joined: PathBuf = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    };
    // Inspect every existing prefix from the filesystem root down: any
    // symlink component (never resolved through) is fatal.
    let mut prefix = PathBuf::new();
    for comp in joined.components() {
        prefix.push(comp.as_os_str());
        match std::fs::symlink_metadata(&prefix) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err("unsafe output path".to_string());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("unsafe output path".to_string()),
        }
    }
    if std::fs::symlink_metadata(&joined).is_ok() {
        return std::fs::canonicalize(&joined).map_err(|_| "unsafe output path".to_string());
    }
    // Missing: find the nearest existing ancestor (already verified to be
    // symlink-free above), canonicalize it, append the lexical tail with
    // `.`/`..` resolved lexically against the canonical parent.
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut ancestor: &Path = &joined;
    loop {
        match ancestor.parent() {
            Some(p) if !p.as_os_str().is_empty() => {
                if let Some(file) = ancestor.file_name() {
                    tail.push(file.to_os_string());
                }
                ancestor = p;
                if std::fs::symlink_metadata(ancestor).is_ok() {
                    break;
                }
            }
            _ => break,
        }
    }
    if std::fs::symlink_metadata(ancestor).is_err() {
        return Err("unsafe output path".to_string());
    }
    let mut resolved =
        std::fs::canonicalize(ancestor).map_err(|_| "unsafe output path".to_string())?;
    tail.reverse();
    for comp in &tail {
        let s = comp.to_string_lossy();
        if s == "." || s.is_empty() {
            continue;
        }
        if s == ".." {
            // `pop` on an absolute canonical base always succeeds; a `..`
            // that would escape above `/` is simply absorbed.
            resolved.pop();
            continue;
        }
        resolved.push(comp);
    }
    Ok(resolved)
}

/// Canonicalize `path` if it exists, else canonicalize its nearest
/// existing ancestor and append the lexical tail (resolving `.`/`..`).
fn canonicalize_allowing_missing(path: &Path) -> Result<PathBuf, String> {
    if std::fs::symlink_metadata(path).is_ok() {
        return std::fs::canonicalize(path).map_err(|_| "unsafe generated path".to_string());
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut ancestor: &Path = path;
    loop {
        match ancestor.parent() {
            Some(p) if !p.as_os_str().is_empty() => {
                if let Some(file) = ancestor.file_name() {
                    tail.push(file.to_os_string());
                }
                ancestor = p;
                if std::fs::symlink_metadata(ancestor).is_ok() {
                    break;
                }
            }
            _ => break,
        }
    }
    if std::fs::symlink_metadata(ancestor).is_err() {
        return Err("unsafe generated path".to_string());
    }
    let mut resolved =
        std::fs::canonicalize(ancestor).map_err(|_| "unsafe generated path".to_string())?;
    tail.reverse();
    for comp in &tail {
        let s = comp.to_string_lossy();
        if s == "." || s.is_empty() {
            continue;
        }
        if s == ".." {
            resolved.pop();
            continue;
        }
        resolved.push(comp);
    }
    Ok(resolved)
}

/// Validate a to-be-generated relative path `rel` against output `root`:
/// reject absolute paths and any `..`, reject symlink components at or
/// below `root`, and enforce lexical plus canonical component-aware
/// containment (never a string-prefix check).
pub fn validate_generated_path(root: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.is_empty() {
        return Err("unsafe generated path: empty relative path".to_string());
    }
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err("unsafe generated path: absolute path".to_string());
    }
    for comp in rel_path.components() {
        match comp {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("unsafe generated path: escapes output root".to_string())
            }
            _ => {}
        }
    }
    let joined = root.join(rel_path);
    // Lexical component-aware containment (always holds given the
    // rejections above, but enforced explicitly as defense in depth).
    if !joined.starts_with(root) {
        return Err("unsafe generated path: escapes output root".to_string());
    }
    // Symlink components at or below the root are fatal.
    reject_symlink_chain(root, &joined)
        .map_err(|_| "unsafe generated path: symlink component".to_string())?;
    // Canonical component-aware containment.
    let canon_root = canonicalize_allowing_missing(root)?;
    let canon_dest = canonicalize_allowing_missing(&joined)?;
    if !canon_dest.starts_with(&canon_root) {
        return Err("unsafe generated path: escapes output root".to_string());
    }
    Ok(joined)
}
