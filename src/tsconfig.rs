//! Shared JSONC-tolerant tsconfig loader with `extends` following.
//!
//! [`load_merged_tsconfig`] reads one tsconfig file plus its `extends`
//! chain and merges the three consumed fields (`compilerOptions.baseUrl`,
//! `compilerOptions.paths`, `compilerOptions.rootDir`). Both the
//! discovery backend (needs `rootDir`) and the resolver backend (needs
//! `baseUrl` + `paths`) build on this module so the two never disagree
//! about what a tsconfig means.
//!
//! Parsing is JSONC-tolerant via `jsonc-parser` (comments and trailing
//! commas accepted) but otherwise strict JSON: every other looseness the
//! library offers (single-quoted strings, loose property names, missing
//! commas, hex/`NaN` numbers, extended escapes) stays off, so a genuinely
//! malformed file still fails closed with a path-tagged error. No
//! hand-rolled comment stripper (a naive one corrupts `//` inside string
//! literals).
//!
//! `extends` merge rules (TypeScript semantics for the consumed fields):
//! - The chain is followed through relative references (resolved against
//!   the referencing file's directory, with a `.json` suffix tried when
//!   the reference has no extension). Array-form `extends` applies in
//!   order: later entries override earlier ones; the extending file
//!   overrides every parent.
//! - Per-field child-wins: the nearest definition of `baseUrl`, `paths`,
//!   or `rootDir` scanning from the entry file outward wins wholesale
//!   (`paths` is replaced as a whole object, never merged per key).
//!   Wrong-typed values (non-string `baseUrl`, non-object `paths`) are not
//!   definitions: the next definition outward stands (best-effort shape
//!   skipping, matching the backends' existing misshapen-value handling).
//! - A relative path in a parent file resolves against that parent file's
//!   directory, not the entry file's: each consumed value is joined to its
//!   defining file's directory at merge time.
//! - Revisits terminate silently via a visited set (self-cycles and longer
//!   cycles end the walk, keeping the values collected so far).
//! - Anything that stops the walk early without a cycle — a missing or
//!   unparseable parent, a bare package-style reference (no
//!   `node_modules` lookup is attempted), or a wrong-typed `extends`
//!   value — records the first offender in [`MergedTsConfig::unfollowed`]
//!   and continues with the partial merge. Callers with a diagnostics
//!   channel report `TS001`; callers without one (discovery) use the
//!   partial values as-is.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// The merged tsconfig view consumed by both TypeScript backends.
#[derive(Debug)]
pub struct MergedTsConfig {
    /// Directory of the entry tsconfig file (id base / default join root).
    pub dir: PathBuf,
    /// Nearest `compilerOptions.baseUrl`, joined to its defining file's
    /// directory; `None` when no file in the chain defines one (callers
    /// fall back to [`MergedTsConfig::dir`]).
    pub base_url: Option<PathBuf>,
    /// Nearest `compilerOptions.paths` as `(key, raw target patterns)`
    /// pairs (wholesale child-wins); empty when no file defines any.
    pub paths: Vec<(String, Vec<String>)>,
    /// Nearest `compilerOptions.rootDir`, joined to its defining file's
    /// directory; `None` when no file in the chain defines one (callers
    /// fall back to [`MergedTsConfig::dir`]).
    pub root_dir: Option<PathBuf>,
    /// The first `extends` reference that could not be followed, if any
    /// (see module docs). `None` means the chain was followed cleanly.
    pub unfollowed: Option<String>,
}

/// JSONC-tolerant parse options: strict JSON plus comments and trailing
/// commas. Everything else the library would loosely accept stays off so
/// malformed files still fail closed.
fn parse_options() -> jsonc_parser::ParseOptions {
    jsonc_parser::ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}

/// Lexically join `rel` to `dir` and normalize `.`/`..` without touching
/// the filesystem. An absolute `rel` is normalized on its own.
fn join_lex(dir: &Path, rel: &str) -> PathBuf {
    let joined = {
        let candidate = Path::new(rel);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            dir.join(candidate)
        }
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalize for the visited set so the same file referenced through
/// different relative spellings still counts as one visit.
fn visit_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Read one tsconfig file (JSONC-tolerant), tagging errors with the path.
/// An empty file deserializes as `null` (per `jsonc-parser` semantics) and
/// is treated as an empty object, matching `tsc`.
fn read_tsconfig(path: &Path) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let value: serde_json::Value = jsonc_parser::parse_to_serde_value(&text, &parse_options())
        .map_err(|e| format!("invalid JSON in {}: {e}", path.display()))?;
    Ok(if value.is_null() {
        serde_json::Value::Object(Default::default())
    } else {
        value
    })
}

/// Collect the `extends` chain as `(file dir, parsed value)` layers,
/// root-most ancestor first and the entry file last. Structural trouble
/// (missing/unparseable parent, bare reference, wrong-typed value) records
/// [`MergedTsConfig::unfollowed`] once and ends that branch; cycles end
/// silently via `visited`.
fn collect_layers(
    path: &Path,
    layers: &mut Vec<(PathBuf, serde_json::Value)>,
    visited: &mut HashSet<PathBuf>,
    unfollowed: &mut Option<String>,
) -> Result<(), String> {
    if !visited.insert(visit_key(path)) {
        return Ok(());
    }
    let value = read_tsconfig(path)?;
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if let Some(extends) = value.get("extends") {
        let refs: Option<Vec<String>> = match extends {
            serde_json::Value::String(one) => Some(vec![one.clone()]),
            serde_json::Value::Array(many) => {
                let mut out = Vec::with_capacity(many.len());
                for item in many {
                    match item.as_str() {
                        Some(text) => out.push(text.to_string()),
                        None => {
                            if unfollowed.is_none() {
                                *unfollowed = Some(path.display().to_string());
                            }
                            layers.push((dir, value));
                            return Ok(());
                        }
                    }
                }
                Some(out)
            }
            _ => {
                if unfollowed.is_none() {
                    *unfollowed = Some(path.display().to_string());
                }
                None
            }
        };
        if let Some(refs) = refs {
            for reference in &refs {
                let is_relative = reference.starts_with('.')
                    || reference.starts_with('/')
                    || Path::new(reference).is_absolute();
                if !is_relative {
                    // Bare package-style references (`@tsconfig/node/...`)
                    // would need a `node_modules` lookup; out of scope.
                    if unfollowed.is_none() {
                        *unfollowed = Some(reference.clone());
                    }
                    continue;
                }
                let mut candidate = join_lex(&dir, reference);
                if !candidate.is_file() && candidate.extension().is_none() {
                    let mut with_json = candidate.as_os_str().to_owned();
                    with_json.push(".json");
                    candidate = PathBuf::from(with_json);
                }
                if !candidate.is_file() {
                    if unfollowed.is_none() {
                        *unfollowed = Some(candidate.display().to_string());
                    }
                    continue;
                }
                // An unparseable parent ends the walk with a hard error
                // only when it is the entry file's own problem; deeper
                // parents degrade to `unfollowed` (partial merge beats a
                // total failure for one bad ancestor).
                match read_tsconfig(&candidate) {
                    Ok(_) => collect_layers(&candidate, layers, visited, unfollowed)?,
                    Err(_) => {
                        if unfollowed.is_none() {
                            *unfollowed = Some(candidate.display().to_string());
                        }
                    }
                }
            }
        }
    }
    layers.push((dir, value));
    Ok(())
}

/// Load `path` plus its `extends` chain into the merged view. Hard errors
/// (unreadable file, truly-unparseable JSONC) are path-tagged strings;
/// chain trouble degrades to [`MergedTsConfig::unfollowed`].
pub fn load_merged_tsconfig(path: &Path) -> Result<MergedTsConfig, String> {
    let entry_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut layers: Vec<(PathBuf, serde_json::Value)> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut unfollowed: Option<String> = None;
    collect_layers(path, &mut layers, &mut visited, &mut unfollowed)?;
    // Layers are root-most-first, entry-last: later definitions override
    // earlier ones, so the entry file wins every field it defines.
    let mut base_url: Option<PathBuf> = None;
    let mut paths: Vec<(String, Vec<String>)> = Vec::new();
    let mut root_dir: Option<PathBuf> = None;
    for (layer_dir, value) in &layers {
        let compiler = value.get("compilerOptions");
        if let Some(text) = compiler
            .and_then(|c| c.get("baseUrl"))
            .and_then(|v| v.as_str())
        {
            base_url = Some(join_lex(layer_dir, text));
        }
        if let Some(map) = compiler
            .and_then(|c| c.get("paths"))
            .and_then(|v| v.as_object())
        {
            let mut merged: Vec<(String, Vec<String>)> = Vec::new();
            for (key, targets) in map {
                let Some(array) = targets.as_array() else {
                    continue;
                };
                let list: Vec<String> = array
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                if list.is_empty() {
                    continue;
                }
                merged.push((key.clone(), list));
            }
            // Wholesale child-wins: a present `paths` object replaces the
            // inherited one even when every entry filters out.
            paths = merged;
        }
        if let Some(text) = compiler
            .and_then(|c| c.get("rootDir"))
            .and_then(|v| v.as_str())
        {
            root_dir = Some(join_lex(layer_dir, text));
        }
    }
    Ok(MergedTsConfig {
        dir: entry_dir,
        base_url,
        paths,
        root_dir,
        unfollowed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).expect("fixture write");
        path
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_parse() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = write(
            tmp.path(),
            "tsconfig.json",
            "{\n  // leading comment\n  \"compilerOptions\": {\n    \"baseUrl\": \".\", // trailing comment\n    \"paths\": {\n      \"@/*\": [\"src/*\"],\n    },\n  },\n}\n",
        );
        let merged = load_merged_tsconfig(&path).expect("jsonc parses");
        assert_eq!(merged.base_url, Some(tmp.path().to_path_buf()));
        assert_eq!(
            merged.paths,
            vec![("@/*".to_string(), vec!["src/*".to_string()])]
        );
        assert_eq!(merged.root_dir, None);
        assert_eq!(merged.unfollowed, None);
    }

    #[test]
    fn extends_inherits_base_and_child_overrides_paths() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        write(
            tmp.path(),
            "tsconfig.base.json",
            "{\"compilerOptions\": {\"baseUrl\": \".\", \"rootDir\": \"./src\", \
             \"paths\": {\"@base/*\": [\"base/*\"], \"@over/*\": [\"base-over/*\"]}}}",
        );
        let child = write(
            tmp.path(),
            "tsconfig.json",
            "{\"extends\": \"./tsconfig.base.json\", \"compilerOptions\": \
             {\"paths\": {\"@over/*\": [\"over/*\"]}}}",
        );
        let merged = load_merged_tsconfig(&child).expect("chain loads");
        // `paths` is the child's wholesale (inherited `@base/*` is gone).
        assert_eq!(
            merged.paths,
            vec![("@over/*".to_string(), vec!["over/*".to_string()])]
        );
        // `baseUrl`/`rootDir` are inherited, joined to the base file's dir.
        assert_eq!(merged.base_url, Some(tmp.path().to_path_buf()));
        assert_eq!(merged.root_dir, Some(tmp.path().join("src")));
        assert_eq!(merged.unfollowed, None);
    }

    #[test]
    fn self_cycle_terminates_with_own_values() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = write(
            tmp.path(),
            "tsconfig.json",
            "{\"extends\": \"./tsconfig.json\", \
             \"compilerOptions\": {\"baseUrl\": \".\"}}",
        );
        let merged = load_merged_tsconfig(&path).expect("cycle terminates");
        assert_eq!(merged.base_url, Some(tmp.path().to_path_buf()));
        assert_eq!(merged.unfollowed, None);
    }

    #[test]
    fn missing_parent_marks_unfollowed_and_keeps_child() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = write(
            tmp.path(),
            "tsconfig.json",
            "{\"extends\": \"./nope.json\", \
             \"compilerOptions\": {\"baseUrl\": \".\"}}",
        );
        let merged = load_merged_tsconfig(&path).expect("partial merge");
        assert_eq!(merged.base_url, Some(tmp.path().to_path_buf()));
        assert!(merged.unfollowed.is_some(), "unfollowed: {merged:?}");
    }

    #[test]
    fn non_string_array_extends_keeps_own_values() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = write(
            tmp.path(),
            "tsconfig.json",
            "{\"extends\": [123], \
             \"compilerOptions\": {\"baseUrl\": \".\"}}",
        );
        let merged = load_merged_tsconfig(&path).expect("partial merge");
        assert_eq!(merged.base_url, Some(tmp.path().to_path_buf()));
        assert!(merged.unfollowed.is_some(), "unfollowed: {merged:?}");
    }

    #[test]
    fn truly_unparseable_still_fails_closed() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = write(
            tmp.path(),
            "tsconfig.json",
            "{\"compilerOptions\": {oops}\n",
        );
        let err = load_merged_tsconfig(&path).unwrap_err();
        assert!(
            err.contains("tsconfig.json"),
            "error names the bad file: {err}"
        );
    }
}
