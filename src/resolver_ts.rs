//! TypeScript module discovery and import resolution ([`TsAnalyzer`]).
//!
//! [`TsAnalyzer`] runs the whole-workspace pipeline for a TypeScript project:
//! per-target file discovery, tree-sitter import extraction, specifier
//! resolution, then the shared dedup + total-order sort.
//!
//! # Module identity
//!
//! Non-root [`ModuleId`]s are `make_module_id(package, target.name, segments)`
//! where `segments` is the tsconfig-relative file path with `/` → `::` and
//! the extension stripped (`src/routes/user.ts` → `::src::routes::user`);
//! the root module is exactly two segments, `{package}::{target.name}`.
//! `index.*` barrels denote their directory module (segment dropped,
//! mirroring Python `__init__.py`); directories without an index become
//! file-less namespace modules (mirroring Python PEP 420) so `mod`
//! containment chains stay complete. `Bin` targets are single root modules
//! each. Probe-shadowed siblings (`user.tsx` behind `user.ts`,
//! `user/index.ts` behind either) are skipped at walk time: at most one
//! module per id, mirroring the first-hit-wins probe (Rust `MOD002`
//! tradeoff — a shadowed file's imports go with it, and probe-time
//! `MOD004` already warns on use).
//!
//! # Resolution rules (brief-governed, spec §3)
//!
//! * Relative specifiers (`./`, `../`) with an explicit probed extension
//!   (`.ts/.tsx/.mts/.cts/.d.ts`) try the literal path first: an existing
//!   regular file resolves with no `MOD004` (unambiguous), otherwise the
//!   pinned probe below runs unchanged. All other relative specifiers
//!   probe the importing file's directory
//!   in the pinned order `.ts/.tsx/.mts/.cts`, `/index` barrels, then
//!   `.d.ts`, first-hit-wins; a later existing candidate yields one `MOD004`
//!   Warning naming the hit and the first loser. Relative specifiers NEVER
//!   consult `paths`. A hit on a file with no walked module (`.d.ts`,
//!   outside the walk, or walk-shadowed) is `SCOPE001`, as is a total miss.
//! * Bare specifiers consult `paths` first (keys longest-literal-prefix-first
//!   with lexical ties, first matching key wins, single `*` substitution,
//!   resolved against `baseUrl`, default the tsconfig dir), then the
//!   own-package arm, then the external rules.
//! * A bare specifier equal to the owning package name (compared verbatim —
//!   scoped `@org/web` kept whole, so the prefix test is `{pkg}/`) strips it
//!   and longest-prefix-resolves the remainder against the package's `Lib`
//!   trees: exactly one match → `Internal`, zero or several → `SCOPE001`
//!   (own namespace is never external). No `Lib` target in the package falls
//!   through to the external rules.
//! * Any other bare specifier resolves its scope-aware name (first segment,
//!   or `@scope/name` for scoped specifiers) through `AnalysisInput.deps`:
//!   exactly one pin → `External`, several → `SCOPE001` (mirrors the
//!   Rust/Python multi-version rule), none → `EXT001`.
//! * `node:`-prefixed specifiers and bare core-module names are silently
//!   skipped (no node, no diagnostic).
//! * `import ... from`, `export ... from` (incl. `export *`), `require()`
//!   with a single string-literal argument, and dynamic `import()` with a
//!   single string literal are all normal `Use` edges; `import type` is a
//!   normal edge too. Non-literal `require`/`import()` arguments are silently
//!   ignored. `.tsx` files parse with `LANGUAGE_TSX`, all other TS flavors
//!   with `LANGUAGE_TYPESCRIPT`.
//! * No `CFG001`: the single-run conditional-compilation diagnostic is
//!   Rust-only per spec §2, as with Python.
//! * The tsconfig is located per target by ancestor walk from `root_source`
//!   (nearest `tsconfig*.json`, `node_modules/` never considered) and
//!   loaded through the shared JSONC-tolerant `extends`-following loader
//!   ([`crate::tsconfig`]); a missing (or unreadable) tsconfig means
//!   defaults (id base = target root, no `paths` map). A partially
//!   followed `extends` chain yields one `TS001` Warning per target and
//!   the partial merge is used.

use crate::domain::{
    Dependency, DependencyKind, DependencyTarget, Diagnostic, DiagnosticSeverity, Module, Package,
    SourceSpan, Target, TargetKind, WorkspaceGraph,
};
use crate::graph::{dedup_dependencies, sort_graph};
use crate::ids::{make_external_id, make_module_id, ModuleId, TargetId};
use crate::resolver::{AnalysisInput, Analyzer};
use crate::ts_parse::{parse, query_nodes, MatchedNode};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// Pinned Node.js core-module silent-skip set.
///
/// Generation: Node.js `module.builtinModules` as documented under "Built-in
/// modules" (`nodejs.org/api/modules.html`), `node:`-stripped, deduplicated,
/// sorted. Sorted for `binary_search`; the unit test below pins sortedness.
const CORE_MODULES: &[&str] = &[
    "assert",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "crypto",
    "dgram",
    "dns",
    "domain",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "querystring",
    "readline",
    "repl",
    "stream",
    "string_decoder",
    "timers",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "v8",
    "vm",
    "worker_threads",
    "zlib",
];

/// Whether `first` (a bare specifier's first `/` segment) is a core module.
fn is_core(first: &str) -> bool {
    CORE_MODULES.binary_search(&first).is_ok()
}

/// Top-level query: import/export sources plus every call expression.
/// Calls need a snippet re-parse (see [`extract_imports`]) because the flat
/// capture list cannot pair a callee with its arguments when non-literal
/// calls (e.g. `require(x)`) interleave.
const TOP_QUERY: &str = "[(import_statement source: (string) @imp) (export_statement source: (string) @exp) (call_expression) @call]";
/// The callee of the snippet's root call only (anchored so nested calls
/// cannot leak in: the snippet is exactly one call expression).
const SNIP_FUNC_QUERY: &str =
    "(program (expression_statement (call_expression function: (identifier) @func)))";
/// Dynamic-`import` marker on the snippet's root call.
const SNIP_DYN_QUERY: &str =
    "(program (expression_statement (call_expression function: (import) @dyn)))";
/// String-literal arguments of the snippet's root call.
const SNIP_STR_QUERY: &str =
    "(program (expression_statement (call_expression arguments: (arguments (string) @s))))";
/// All named arguments of the snippet's root call (forces single-argument).
const SNIP_ARG_QUERY: &str =
    "(program (expression_statement (call_expression arguments: (arguments (_) @arg))))";

/// Extension probe order (pinned): file probes, then `/index` barrels, then
/// type-only declarations. First-hit-wins; a later hit is the `MOD004` loser.
const PROBE_SUFFIXES: [&str; 10] = [
    ".ts",
    ".tsx",
    ".mts",
    ".cts",
    "/index.ts",
    "/index.tsx",
    "/index.mts",
    "/index.cts",
    ".d.ts",
    "/index.d.ts",
];

/// One syntactic TypeScript import before specifier resolution.
struct TsUse {
    from: ModuleId,
    /// The raw specifier text as written (`./lib`, `@/deep/mod`, `lodash`).
    spec: String,
    /// Lexically normalized importing file's directory (relative probes).
    dir: PathBuf,
    span: Option<SourceSpan>,
}

/// A discovered module plus its tsconfig-relative path segments.
struct Entry {
    segments: Vec<String>,
    module: Module,
}

/// One sorted `paths` entry: the literal `prefix` (before the first `*`),
/// the `suffix` (after it), and the raw target patterns.
struct PathEntry {
    key: String,
    prefix: String,
    suffix: String,
    has_star: bool,
    targets: Vec<String>,
}

/// The per-target tsconfig view: id base, `baseUrl`, sorted `paths`, and
/// the first unfollowed `extends` reference (if any — reported as `TS001`
/// where the walk left the chain incomplete).
struct TsConfig {
    base_dir: PathBuf,
    base_url: PathBuf,
    paths: Vec<PathEntry>,
    unfollowed: Option<String>,
}

fn root_id(target: &Target) -> ModuleId {
    ModuleId(format!("{}::{}", target.package_id.0, target.name))
}

/// Lexically resolve `.` / `..` without touching the filesystem.
fn lex_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
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

fn scope_diag(from: &ModuleId, path: &str, span: &Option<SourceSpan>) -> Diagnostic {
    Diagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "SCOPE001".to_string(),
        message: format!(
            "scope collision: {} -> {path}; unresolved syntactic ownership",
            from.0
        ),
        source_span: span.clone(),
    }
}

fn ext_diag(from: &ModuleId, path: &str, span: &Option<SourceSpan>) -> Diagnostic {
    Diagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "EXT001".to_string(),
        message: format!(
            "unresolved external: {} -> {path}; no pinned version",
            from.0
        ),
        source_span: span.clone(),
    }
}

fn mod004_diag(hit: &Path, loser: &Path, span: &Option<SourceSpan>) -> Diagnostic {
    Diagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "MOD004".to_string(),
        message: format!(
            "ambiguous ts layout: both {} and {} exist; using {}",
            hit.display(),
            loser.display(),
            hit.display()
        ),
        source_span: span.clone(),
    }
}

fn io_diag(path: &Path, detail: &str) -> Diagnostic {
    Diagnostic {
        severity: DiagnosticSeverity::Error,
        code: "IO001".to_string(),
        message: format!("failed to read {}: {detail}", path.display()),
        source_span: None,
    }
}

/// The nearest `tsconfig*.json` at or above `start` (`tsconfig.json` wins a
/// shared dir, else the lexicographically smallest name), never inside a
/// `node_modules/` dir.
fn find_tsconfig(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        start.to_path_buf()
    };
    loop {
        let in_modules = dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == "node_modules");
        if !in_modules {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut names: Vec<String> = entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.is_file())
                    .filter_map(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .filter(|n| n.starts_with("tsconfig") && n.ends_with(".json"))
                            .map(str::to_string)
                    })
                    .collect();
                names.sort();
                names.dedup();
                if names.contains(&"tsconfig.json".to_string()) {
                    return Some(dir.join("tsconfig.json"));
                }
                if let Some(first) = names.into_iter().next() {
                    return Some(dir.join(first));
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Load the per-target [`TsConfig`] via the shared JSONC-tolerant
/// `extends`-following loader ([`crate::tsconfig::load_merged_tsconfig`]):
/// base dir = entry tsconfig dir (id base), `baseUrl` defaulting to it,
/// `paths` sorted longest-literal-prefix-first with lexical ties. A
/// missing tsconfig means defaults (id base = target root, no paths); an
/// unreadable one also means defaults (discovery already fails closed on
/// malformed tsconfigs upstream, so this path is best-effort only). A
/// partially-followed chain is kept as `unfollowed` for the `TS001`
/// diagnostic at the analyze site.
fn load_tsconfig(path: Option<&Path>, target_root: &Path) -> TsConfig {
    let Some(path) = path else {
        return TsConfig {
            base_dir: target_root.to_path_buf(),
            base_url: target_root.to_path_buf(),
            paths: Vec::new(),
            unfollowed: None,
        };
    };
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| target_root.to_path_buf());
    let fallback = || TsConfig {
        base_dir: dir.clone(),
        base_url: dir.clone(),
        paths: Vec::new(),
        unfollowed: None,
    };
    let Ok(merged) = crate::tsconfig::load_merged_tsconfig(path) else {
        return fallback();
    };
    let base_url = merged.base_url.unwrap_or_else(|| dir.clone());
    let mut paths: Vec<PathEntry> = Vec::new();
    for (key, list) in &merged.paths {
        match key.find('*') {
            Some(i) => paths.push(PathEntry {
                key: key.clone(),
                prefix: key[..i].to_string(),
                suffix: key[i + 1..].to_string(),
                has_star: true,
                targets: list.clone(),
            }),
            None => paths.push(PathEntry {
                key: key.clone(),
                prefix: key.clone(),
                suffix: String::new(),
                has_star: false,
                targets: list.clone(),
            }),
        }
    }
    paths.sort_by(|a, b| {
        b.prefix
            .len()
            .cmp(&a.prefix.len())
            .then_with(|| a.key.cmp(&b.key))
    });
    TsConfig {
        base_dir: dir,
        base_url,
        paths,
        unfollowed: merged.unfollowed,
    }
}

/// Sorted direct-child files of `dir` (determinism); unreadable dirs yield none.
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

/// Sorted direct-child dirs of `dir`, excluding symlinked dirs (cycle guard)
/// and `node_modules/` (never walked).
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
                && p.file_name().and_then(|n| n.to_str()) != Some("node_modules")
        })
        .collect();
    dirs.sort();
    dirs
}

fn entry_module(target: &Target, segments: &[String], file: Option<PathBuf>) -> Module {
    if segments.is_empty() {
        Module {
            id: root_id(target),
            target_id: target.id.clone(),
            name: target.name.clone(),
            qualified_name: target.name.clone(),
            source_path: file,
            source_span: None,
        }
    } else {
        Module {
            id: make_module_id(&target.package_id.0, &target.name, segments),
            target_id: target.id.clone(),
            name: segments.last().cloned().unwrap_or_default(),
            qualified_name: segments.join("/"),
            source_path: file,
            source_span: None,
        }
    }
}

/// Recursively walk the target root: every `.ts/.tsx/.mts/.cts` file
/// (except `.d.ts`, skipped silently) becomes a module, EXCEPT files
/// shadowed by a same-identity sibling earlier in probe order — at most one
/// module per id. `index.*` barrels denote the directory module. Shadowing
/// mirrors the probe walk (Rust `MOD002` tradeoff): a shadowed file's
/// imports go with it, and probe-time `MOD004` already warns on use, so no
/// new diagnostic is emitted here.
fn walk_lib(target: &Target, base: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    walk_dir(target, base, &target.root_source, &mut entries);
    if !entries.iter().any(|e| e.segments.is_empty()) {
        entries.push(Entry {
            segments: Vec::new(),
            module: entry_module(target, &[], Some(target.root_source.clone())),
        });
    }
    // Namespace ancestors: every proper non-empty prefix of a discovered
    // path that has no module of its own is a file-less directory module,
    // so `mod` containment edges below always find their parent.
    let mut known: HashSet<Vec<String>> = entries.iter().map(|e| e.segments.clone()).collect();
    let mut namespaces: Vec<Vec<String>> = Vec::new();
    for entry in &entries {
        for len in 1..entry.segments.len() {
            let prefix = entry.segments[..len].to_vec();
            if known.insert(prefix.clone()) {
                namespaces.push(prefix);
            }
        }
    }
    for segments in namespaces {
        entries.push(Entry {
            module: entry_module(target, &segments, None),
            segments,
        });
    }
    entries
}

/// The shadow group and probe rank of a walked file name: `None` for
/// skipped (`.d.ts`) and unhandled names, else the group (`None` = the
/// directory's own `index.*` barrel, `Some(stem)` = a stem group shared
/// with a `stem/` subdirectory barrel) and the rank within it (extension
/// order `.ts/.tsx/.mts/.cts`; subdirectory barrels sort after every
/// same-dir file, matching [`PROBE_SUFFIXES`]).
fn file_group(file_name: &str) -> Option<(Option<String>, u8)> {
    if file_name.ends_with(".d.ts") {
        return None;
    }
    for (rank, ext) in [".ts", ".tsx", ".mts", ".cts"].iter().enumerate() {
        if let Some(stem) = file_name.strip_suffix(ext) {
            if stem.is_empty() {
                return None;
            }
            if stem == "index" {
                return Some((None, rank as u8));
            }
            return Some((Some(stem.to_string()), rank as u8));
        }
    }
    None
}

fn walk_dir(target: &Target, base: &Path, dir: &Path, entries: &mut Vec<Entry>) {
    walk_dir_inner(target, base, dir, entries, false);
}

/// Walk one directory: the lowest probe rank per shadow group wins and
/// every other same-group file is skipped entirely (R1). `skip_barrel` is
/// set when the parent directory already kept a same-stem file that
/// shadows this directory's `index.*` barrel (any same-dir file outranks
/// any subdirectory barrel, since barrel ranks start at 4): the barrel is
/// skipped but the directory is still descended into for its other files.
fn walk_dir_inner(
    target: &Target,
    base: &Path,
    dir: &Path,
    entries: &mut Vec<Entry>,
    skip_barrel: bool,
) {
    let rel: Vec<String> = dir
        .strip_prefix(base)
        .ok()
        .map(|r| {
            r.components()
                .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let files = sorted_files(dir);
    let mut best: HashMap<Option<String>, u8> = HashMap::new();
    for file in &files {
        let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some((group, rank)) = file_group(name) {
            best.entry(group)
                .and_modify(|kept| *kept = (*kept).min(rank))
                .or_insert(rank);
        }
    }
    for file in &files {
        let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((group, rank)) = file_group(name) else {
            continue;
        };
        if group.is_none() && skip_barrel {
            continue;
        }
        if rank > best[&group] {
            continue;
        }
        let mut segments = rel.clone();
        if let Some(segment) = group {
            segments.push(segment);
        }
        entries.push(Entry {
            module: entry_module(target, &segments, Some(file.clone())),
            segments,
        });
    }
    for sub in sorted_dirs(dir) {
        let shadowed = sub
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|stem| best.contains_key(&Some(stem.to_string())));
        walk_dir_inner(target, base, &sub, entries, shadowed);
    }
}

/// A `Bin` target is a single root module for its file. A `root_source`
/// that is not a readable file (missing-module entries from discovery land
/// here) fails closed with an IO-family Error diagnostic; the module is
/// still registered and analysis continues.
fn walk_bin(target: &Target) -> (Vec<Entry>, Vec<Diagnostic>) {
    let entries = vec![Entry {
        segments: Vec::new(),
        module: entry_module(target, &[], Some(target.root_source.clone())),
    }];
    let mut diags = Vec::new();
    if !target.root_source.is_file() {
        let detail = match std::fs::symlink_metadata(&target.root_source) {
            Ok(_) => "not a regular file".to_string(),
            Err(e) => e.to_string(),
        };
        diags.push(io_diag(&target.root_source, &detail));
    }
    (entries, diags)
}

/// The tree-sitter language for `path` by file extension (`TSX` for `.tsx`,
/// TypeScript for the rest); `None` for non-TS files (compiled `.js`
/// outputs are sources-never-parsed, skipped silently).
fn language_for(path: &Path) -> Option<tree_sitter::Language> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("tsx") => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        Some("ts") | Some("mts") | Some("cts") => {
            Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        }
        _ => None,
    }
}

/// Strip one pair of matching `'`/`"` quotes; `None` for anything else.
fn unquote(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    if text.len() >= 2
        && (bytes[0] == b'\'' || bytes[0] == b'"')
        && bytes[text.len() - 1] == bytes[0]
    {
        Some(text[1..text.len() - 1].to_string())
    } else {
        None
    }
}

/// Span for one extracted specifier: the captured string node's rows,
/// 1-based for `SourceSpan`.
fn spec_span(path: &Path, node: &MatchedNode) -> SourceSpan {
    SourceSpan {
        file: path.to_path_buf(),
        line_start: node.start_row as u32 + 1,
        line_end: node.end_row as u32 + 1,
    }
}

/// Extract every specifier of one file into [`TsUse`]s.
///
/// Import/export sources come straight from the top-level query; each call
/// expression is re-parsed as a snippet and accepted only when its ROOT call
/// is `require` (identifier callee) or dynamic `import()` with exactly one
/// named argument that is a string literal. Anything else — member calls,
/// non-literal arguments, multi-argument calls — is silently ignored.
fn extract_imports(from: &ModuleId, path: &Path) -> Result<Vec<TsUse>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let language = language_for(path)
        .ok_or_else(|| format!("failed to read {}: not a TypeScript file", path.display()))?;
    let tree = parse(language.clone(), &text)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
    let top = query_nodes(&tree, &language, &text, TOP_QUERY)
        .map_err(|e| format!("typescript import query failed for {}: {e}", path.display()))?;
    let dir = path.parent().map(lex_normalize).unwrap_or_default();
    let mut out = Vec::new();
    for node in &top {
        match node.capture.as_str() {
            "imp" | "exp" => {
                let Some(spec) = unquote(&node.text) else {
                    continue;
                };
                out.push(TsUse {
                    from: from.clone(),
                    spec,
                    dir: dir.clone(),
                    span: Some(spec_span(path, node)),
                });
            }
            _ => {
                if let Some(use_) = extract_call(from, path, &dir, node, &language)? {
                    out.push(use_);
                }
            }
        }
    }
    Ok(out)
}

/// Decide one call expression: `Some(TsUse)` for literal `require` / dynamic
/// `import()`, `None` (silent) for everything else.
fn extract_call(
    from: &ModuleId,
    path: &Path,
    dir: &Path,
    node: &MatchedNode,
    language: &tree_sitter::Language,
) -> Result<Option<TsUse>, String> {
    let snippet = node.text.clone();
    let snippet_tree = parse(language.clone(), &snippet)
        .map_err(|e| format!("failed to re-parse import in {}: {e}", path.display()))?;
    let query = |q: &str| {
        query_nodes(&snippet_tree, language, &snippet, q)
            .map_err(|e| format!("typescript import query failed for {}: {e}", path.display()))
    };
    let is_require = query(SNIP_FUNC_QUERY)?
        .first()
        .is_some_and(|f| f.text == "require");
    let is_dynamic = !query(SNIP_DYN_QUERY)?.is_empty();
    if !is_require && !is_dynamic {
        return Ok(None);
    }
    let strings = query(SNIP_STR_QUERY)?;
    let args = query(SNIP_ARG_QUERY)?;
    if strings.len() != 1 || args.len() != 1 {
        return Ok(None);
    }
    let Some(spec) = unquote(&strings[0].text) else {
        return Ok(None);
    };
    Ok(Some(TsUse {
        from: from.clone(),
        spec,
        dir: dir.to_path_buf(),
        span: Some(SourceSpan {
            file: path.to_path_buf(),
            line_start: node.start_row as u32 + 1,
            line_end: node.end_row as u32 + 1,
        }),
    }))
}

/// Resolution outcome for one [`TsUse`]: an edge target, silence, or a
/// diagnostic with no edge.
enum Resolution {
    Edge(DependencyTarget),
    Unresolved(Diagnostic),
    Skip,
}

/// Probe `base` in [`PROBE_SUFFIXES`] order: the first existing file plus the
/// first later existing file (the `MOD004` loser, if any).
fn probe(base: &Path) -> Option<(PathBuf, Option<PathBuf>)> {
    let mut hit: Option<PathBuf> = None;
    for suffix in PROBE_SUFFIXES {
        let mut raw = base.as_os_str().to_owned();
        raw.push(suffix);
        let candidate = lex_normalize(Path::new(&raw));
        if candidate.is_file() {
            match hit {
                None => hit = Some(candidate),
                Some(first) => return Some((first, Some(candidate))),
            }
        }
    }
    hit.map(|h| (h, None))
}

/// The `paths` remainder for `spec` under `entry` (`None` = no match).
fn paths_match(entry: &PathEntry, spec: &str) -> Option<String> {
    if entry.has_star {
        if !spec.starts_with(&entry.prefix) {
            return None;
        }
        if !entry.suffix.is_empty() && !spec.ends_with(&entry.suffix) {
            return None;
        }
        let end = spec.len() - entry.suffix.len();
        if end < entry.prefix.len() {
            return None;
        }
        Some(spec[entry.prefix.len()..end].to_string())
    } else if spec == entry.key {
        Some(String::new())
    } else {
        None
    }
}

/// Whether `spec` is relative (`./`, `../`, `.`, `..`); only these probe the
/// importing file's directory, and they NEVER consult `paths`.
fn is_relative(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".."
}

/// The dependency name of a bare specifier: `@scope/name` for scoped
/// specifiers, else the first `/` segment.
fn dep_name(spec: &str) -> String {
    if let Some(rest) = spec.strip_prefix('@') {
        let mut parts = rest.splitn(3, '/');
        match (parts.next(), parts.next()) {
            (Some(scope), Some(name)) if !scope.is_empty() && !name.is_empty() => {
                return format!("@{scope}/{name}");
            }
            _ => return spec.to_string(),
        }
    }
    spec.split('/').next().unwrap_or(spec).to_string()
}

/// Longest-prefix module attribution: the full `segments` path preferred,
/// else the longest strict prefix of at least one segment naming a known
/// module. Returns the match plus its length.
fn longest_prefix(
    tree: &HashMap<Vec<String>, ModuleId>,
    segments: &[String],
) -> (Option<ModuleId>, usize) {
    for len in (1..=segments.len()).rev() {
        if let Some(id) = tree.get(&segments[..len]) {
            return (Some(id.clone()), len);
        }
    }
    (None, 0)
}

/// Resolve one probed file to an edge: a walked module → `Internal`, anything
/// else (`.d.ts`, outside the walk) → `SCOPE001`.
fn resolve_hit(raw: &TsUse, hit: &Path, by_file: &HashMap<PathBuf, ModuleId>) -> Resolution {
    match by_file.get(&lex_normalize(hit)) {
        Some(id) => Resolution::Edge(DependencyTarget::Internal(id.clone())),
        None => Resolution::Unresolved(scope_diag(&raw.from, &raw.spec, &raw.span)),
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_use(
    raw: &TsUse,
    cfg: &TsConfig,
    pkg: &str,
    lib_trees: &[&HashMap<Vec<String>, ModuleId>],
    by_file: &HashMap<PathBuf, ModuleId>,
    deps_by_name: &HashMap<&str, Vec<&crate::metadata::ExternalDep>>,
    diags: &mut Vec<Diagnostic>,
) -> Resolution {
    if is_relative(&raw.spec) {
        // Explicit-extension specifiers (`./user.ts`) try the literal path
        // first: an existing regular file resolves with no `MOD004`
        // (unambiguous); otherwise the pinned probe sequence runs unchanged.
        let literal_spec = raw.spec.trim_end_matches('/');
        if [".ts", ".tsx", ".mts", ".cts", ".d.ts"]
            .iter()
            .any(|ext| literal_spec.ends_with(ext))
        {
            let literal = lex_normalize(&raw.dir.join(literal_spec));
            if literal.is_file() {
                return resolve_hit(raw, &literal, by_file);
            }
        }
        let base = lex_normalize(&raw.dir.join(&raw.spec));
        return match probe(&base) {
            Some((hit, loser)) => {
                if let Some(loser) = loser {
                    diags.push(mod004_diag(&hit, &loser, &raw.span));
                }
                resolve_hit(raw, &hit, by_file)
            }
            None => Resolution::Unresolved(scope_diag(&raw.from, &raw.spec, &raw.span)),
        };
    }
    if raw.spec.starts_with("node:") {
        return Resolution::Skip;
    }
    let first = raw.spec.split('/').next().unwrap_or("");
    if is_core(first) {
        return Resolution::Skip;
    }
    // Bare specifiers consult `paths` first: the first matching key wins and
    // its target patterns are probed in order (with `MOD004` as usual); a
    // total miss falls through to the own-package/external rules below.
    for entry in &cfg.paths {
        let Some(remainder) = paths_match(entry, &raw.spec) else {
            continue;
        };
        let mut found: Option<PathBuf> = None;
        for target in &entry.targets {
            let substituted = if entry.has_star {
                target.replacen('*', &remainder, 1)
            } else {
                target.clone()
            };
            // `paths` values are project-relative (resolved against
            // `baseUrl`); a stray leading `/` is stripped rather than
            // allowed to escape the project.
            let base = lex_normalize(&cfg.base_url.join(substituted.trim_start_matches('/')));
            if let Some((hit, loser)) = probe(&base) {
                if let Some(loser) = loser {
                    diags.push(mod004_diag(&hit, &loser, &raw.span));
                }
                found = Some(hit);
                break;
            }
        }
        if let Some(hit) = found {
            return resolve_hit(raw, &hit, by_file);
        }
        break;
    }
    // Bare own-package-name specifiers strip the verbatim package prefix and
    // longest-prefix-resolve the remainder against the package's `Lib`
    // trees: exactly one match → `Internal`, zero or several → `SCOPE001`
    // (own namespace is never external). With no `Lib` target in the package
    // this arm cannot run and resolution falls through to externals.
    let remainder: Option<Vec<String>> = if raw.spec == pkg {
        Some(Vec::new())
    } else {
        raw.spec
            .strip_prefix(&format!("{pkg}/"))
            .map(|rest| rest.split('/').map(str::to_string).collect())
    };
    if let Some(remainder) = remainder {
        if !lib_trees.is_empty() {
            let mut hits: Vec<ModuleId> = Vec::new();
            for tree in lib_trees {
                let hit = if remainder.is_empty() {
                    tree.get(&Vec::new()).cloned()
                } else {
                    longest_prefix(tree, &remainder).0
                };
                if let Some(id) = hit {
                    hits.push(id);
                }
            }
            if hits.len() == 1 {
                return Resolution::Edge(DependencyTarget::Internal(hits.swap_remove(0)));
            }
            return Resolution::Unresolved(scope_diag(&raw.from, &raw.spec, &raw.span));
        }
    }
    let name = dep_name(&raw.spec);
    match deps_by_name.get(name.as_str()) {
        Some(versions) if versions.len() == 1 => {
            let dep = versions[0];
            Resolution::Edge(DependencyTarget::External(make_external_id(
                &dep.name,
                &dep.version,
            )))
        }
        // Several pins share this name: ownership is ambiguous, so the
        // single SCOPE001 text applies (mirrors Rust/Python), no edge.
        Some(_) => Resolution::Unresolved(scope_diag(&raw.from, &raw.spec, &raw.span)),
        None => Resolution::Unresolved(ext_diag(&raw.from, &raw.spec, &raw.span)),
    }
}

/// The TypeScript whole-workspace [`Analyzer`]: per-target walk → extract →
/// resolve → dedup → sort, calling the shared `graph`/`diag` helpers.
pub struct TsAnalyzer;

impl Analyzer for TsAnalyzer {
    fn analyze(&self, input: &AnalysisInput) -> Result<WorkspaceGraph, String> {
        // 1. Per-target discovery (tsconfig located + loaded once per target).
        let mut modules_all: Vec<Module> = Vec::new();
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let mut segments_of: HashMap<ModuleId, Vec<String>> = HashMap::new();
        let mut trees: HashMap<TargetId, HashMap<Vec<String>, ModuleId>> = HashMap::new();
        let mut by_file: HashMap<PathBuf, ModuleId> = HashMap::new();
        let mut owner_to_target: HashMap<ModuleId, TargetId> = HashMap::new();
        let mut cfgs: HashMap<TargetId, TsConfig> = HashMap::new();
        for target in &input.targets {
            let tsconfig_path = find_tsconfig(&target.root_source);
            let cfg = load_tsconfig(tsconfig_path.as_deref(), &target.root_source);
            if let Some(unfollowed) = &cfg.unfollowed {
                diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    code: "TS001".to_string(),
                    message: format!(
                        "unfollowed tsconfig extends: {unfollowed}; paths may be incomplete"
                    ),
                    source_span: None,
                });
            }
            let entries = match target.kind {
                TargetKind::Lib => {
                    let walked = walk_lib(target, &cfg.base_dir);
                    (walked, Vec::new())
                }
                TargetKind::Bin => walk_bin(target),
            };
            let (entries, mut diags) = entries;
            diagnostics.append(&mut diags);
            let mut tree: HashMap<Vec<String>, ModuleId> = HashMap::new();
            for entry in entries {
                tree.entry(entry.segments.clone())
                    .or_insert_with(|| entry.module.id.clone());
                // Overlapping target roots can walk one file twice under two
                // ids; first target wins so resolution stays deterministic.
                // File-less namespace modules register no path (probe hits
                // are always files, so a dir key could never resolve).
                if let Some(file) = entry.module.source_path.as_ref() {
                    by_file
                        .entry(lex_normalize(file))
                        .or_insert_with(|| entry.module.id.clone());
                }
                segments_of
                    .entry(entry.module.id.clone())
                    .or_insert_with(|| entry.segments.clone());
                owner_to_target.insert(entry.module.id.clone(), target.id.clone());
                modules_all.push(entry.module);
            }
            trees.insert(target.id.clone(), tree);
            cfgs.insert(target.id.clone(), cfg);
        }
        let deps_by_name: HashMap<&str, Vec<&crate::metadata::ExternalDep>> = {
            let mut map: HashMap<&str, Vec<&crate::metadata::ExternalDep>> = HashMap::new();
            for dep in &input.deps {
                map.entry(dep.name.as_str()).or_default().push(dep);
            }
            map
        };

        // 2. Extraction: one pass per file-backed TypeScript module.
        // Directories and file-less namespace modules carry no imports;
        // compiled outputs (`.js`) are never parsed; unreadable files are
        // IO001 and discovery continues.
        let mut uses: Vec<TsUse> = Vec::new();
        for module in &modules_all {
            let Some(path) = module.source_path.clone() else {
                continue;
            };
            if !path.is_file() || language_for(&path).is_none() {
                continue;
            }
            match extract_imports(&module.id, &path) {
                Ok(mut found) => uses.append(&mut found),
                Err(message) => diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Error,
                    code: "IO001".to_string(),
                    message,
                    source_span: None,
                }),
            }
        }

        // 3. `mod` containment edges (parent → child, kind Module).
        let mut dependencies: Vec<Dependency> = Vec::new();
        for module in &modules_all {
            let Some(target_id) = owner_to_target.get(&module.id) else {
                continue;
            };
            let Some(tree) = trees.get(target_id) else {
                continue;
            };
            let Some(segments) = segments_of.get(&module.id) else {
                continue;
            };
            if segments.is_empty() {
                continue;
            }
            let parent = if segments.len() == 1 {
                let Some(target) = input.targets.iter().find(|t| &t.id == target_id) else {
                    continue;
                };
                root_id(target)
            } else {
                let Some(parent_id) = tree.get(&segments[..segments.len() - 1]) else {
                    continue;
                };
                parent_id.clone()
            };
            dependencies.push(Dependency {
                from: parent,
                to: DependencyTarget::Internal(module.id.clone()),
                kind: DependencyKind::Module,
                source_span: None,
            });
        }

        // 4. Resolve every `TsUse` against the probe walk plus its owning
        // target's tsconfig. No CFG001 walk: the single-run
        // conditional-compilation diagnostic is Rust-only per spec §2.
        for raw in &uses {
            let Some(target_id) = owner_to_target.get(&raw.from) else {
                diagnostics.push(scope_diag(&raw.from, &raw.spec, &raw.span));
                continue;
            };
            let Some(owner) = input.targets.iter().find(|t| &t.id == target_id) else {
                diagnostics.push(scope_diag(&raw.from, &raw.spec, &raw.span));
                continue;
            };
            let Some(cfg) = cfgs.get(target_id) else {
                diagnostics.push(scope_diag(&raw.from, &raw.spec, &raw.span));
                continue;
            };
            let lib_trees: Vec<&HashMap<Vec<String>, ModuleId>> = input
                .targets
                .iter()
                .filter(|t| t.kind == TargetKind::Lib && t.package_id == owner.package_id)
                .filter_map(|t| trees.get(&t.id))
                .collect();
            match resolve_use(
                raw,
                cfg,
                &owner.package_id.0,
                &lib_trees,
                &by_file,
                &deps_by_name,
                &mut diagnostics,
            ) {
                Resolution::Edge(to) => dependencies.push(Dependency {
                    from: raw.from.clone(),
                    to,
                    kind: DependencyKind::Use,
                    source_span: raw.span.clone(),
                }),
                Resolution::Unresolved(diag) => diagnostics.push(diag),
                Resolution::Skip => {}
            }
        }

        // 5. Packages are synthesized from the input targets (identity only;
        // the workspace manifest stands in for per-package manifest paths,
        // restored to the real ones by `run()`).
        let mut package_ids: Vec<_> = input.targets.iter().map(|t| t.package_id.clone()).collect();
        package_ids.sort();
        package_ids.dedup();
        let packages: Vec<Package> = package_ids
            .into_iter()
            .map(|id| Package {
                name: id.0.clone(),
                manifest_path: input.workspace_root.join("package.json"),
                id,
            })
            .collect();

        let mut graph = WorkspaceGraph {
            packages,
            targets: input.targets.clone(),
            modules: modules_all,
            dependencies: dedup_dependencies(dependencies),
            diagnostics,
        };
        sort_graph(&mut graph);
        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_list_sorted_and_spotted() {
        assert!(!CORE_MODULES.is_empty(), "core list must not be empty");
        let mut sorted = CORE_MODULES.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            sorted, CORE_MODULES,
            "CORE_MODULES must stay sorted for binary_search"
        );
        assert_eq!(
            CORE_MODULES.len(),
            CORE_MODULES.iter().collect::<HashSet<_>>().len(),
            "CORE_MODULES must not contain duplicates"
        );
        for member in ["fs", "path", "crypto"] {
            assert!(is_core(member), "{member} must be core-skipped");
        }
        for non_member in ["lodash", "express", "nosuchpkg", "fs-extra"] {
            assert!(!is_core(non_member), "{non_member} must not be core");
        }
    }
}
