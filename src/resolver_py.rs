//! Python module discovery and import resolution ([`PyAnalyzer`]).
//!
//! [`PyAnalyzer`] runs the whole-workspace pipeline for a Python project:
//! per-target module discovery, tree-sitter import extraction, ownership
//! resolution, then the shared dedup + total-order sort.
//!
//! # Module identity
//!
//! Non-root [`ModuleId`]s are `make_module_id(package, target.name, dotted)`
//! (`.` → `::`); the root module is exactly two segments,
//! `{package}::{target.name}`. `__init__.py` denotes its package module (no
//! extra segment); namespace dirs (no `__init__.py`) are modules with no
//! `source_path`. `Bin` targets are single root modules each.
//!
//! # Resolution rules (brief-governed, spec §2)
//!
//! * Absolute imports resolve against the owning target's module tree with
//!   longest-prefix attribution: the full dotted path preferred, else the
//!   longest strict prefix (a single-segment remainder counts) names the
//!   edge target — for plain `import` statements. `from`-imported names are
//!   item-level: anything short of the full path is `SCOPE001` (accepted
//!   over-approximation, pinned by test); `from m import *` is the exception
//!   and edges to the resolved `m` with no diagnostic.
//! * Relative imports anchor at the importer (`__init__.py` anchors at
//!   itself, else at the parent) then climb `level - 1`; climbing above the
//!   root is an unresolved diagnostic with no edge (never a fabricated
//!   parent).
//! * A first segment matching neither the tree nor the stdlib list resolves
//!   through `AnalysisInput.deps`: exactly one pin → `External`, several →
//!   `SCOPE001` (mirrors the Rust multi-version rule), none → `EXT001` —
//!   unless the import is relative (`level > 0`), which can never name an
//!   external and is always `SCOPE001` on a miss.
//! * An absolute import whose first segment is the owning target's own
//!   package name (compared normalized — lowercase, `-` → `_` — so a
//!   hyphenated distribution still owns its import namespace) strips it — when the owning tree itself misses — and
//!   longest-prefix-resolves the remainder against the package's `Lib`
//!   trees: exactly one match → `Internal`, zero or several → `SCOPE001`
//!   (own namespace is never external). Trying the owning tree first keeps
//!   verbatim paths primary (a nested tree may contain the full path) and
//!   can never shadow: the arm only runs on a total owning miss. No `Lib`
//!   target in the package falls through to the rule above.
//! * Stdlib first segments are silently skipped (no node, no diagnostic).
//! * `TYPE_CHECKING`-guarded imports are plain `import_statement` nodes, so
//!   they resolve normally by construction.
//! * No `CFG001`: the single-run conditional-compilation diagnostic is
//!   Rust-only per spec §2.
//! * Aliases (`as`) are diagnostics-only: they never mint IDs, mirroring
//!   Rust.

use crate::domain::{
    Dependency, DependencyKind, DependencyTarget, Diagnostic, DiagnosticSeverity, Module, Package,
    SourceSpan, Target, TargetKind, WorkspaceGraph,
};
use crate::graph::{dedup_dependencies, sort_graph};
use crate::ids::{make_external_id, make_module_id, ModuleId, TargetId};
use crate::resolver::{AnalysisInput, Analyzer};
use crate::ts_parse::{parse, query_nodes, MatchedNode};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Static stdlib first-segment list: the silent-skip set.
///
/// Generation: UNION of CPython 3.11–3.14 `sys.stdlib_module_names` — the
/// runtime list from CPython 3.14 (`python3 -c "import sys;
/// print(sorted(sys.stdlib_module_names))"`, 297 names) plus the modules
/// present in 3.11–3.13 but removed before 3.14, per typeshed
/// `stdlib/VERSIONS` ranges intersecting [3.11, 3.14]
/// (`https://github.com/python/typeshed/blob/main/stdlib/VERSIONS`):
/// gone in 3.12 (`3.0-3.11`: asynchat, asyncore, distutils, imp, smtpd),
/// gone in 3.13 (`3.0-3.12`: aifc, audioop, cgi, cgitb, chunk, crypt,
/// imghdr, lib2to3, mailcap, msilib, nis, nntplib, ossaudiodev, pipes,
/// sndhdr, spwd, sunau, telnetlib, uu, xdrlib, _msi) and gone in 3.14
/// (`_compression: 3.5-3.13`). Sorted for `binary_search`; the unit test
/// below pins sortedness. `__main__` / `typing_extensions` / `_typeshed`
/// (in VERSIONS but never runtime stdlib) are excluded, as is `binhex`
/// (already gone before 3.11).
const STDLIB: &[&str] = &[
    "__future__",
    "_abc",
    "_aix_support",
    "_android_support",
    "_apple_support",
    "_ast",
    "_ast_unparse",
    "_asyncio",
    "_bisect",
    "_blake2",
    "_bz2",
    "_codecs",
    "_codecs_cn",
    "_codecs_hk",
    "_codecs_iso2022",
    "_codecs_jp",
    "_codecs_kr",
    "_codecs_tw",
    "_collections",
    "_collections_abc",
    "_colorize",
    "_compat_pickle",
    "_compression",
    "_contextvars",
    "_csv",
    "_ctypes",
    "_curses",
    "_curses_panel",
    "_datetime",
    "_dbm",
    "_decimal",
    "_elementtree",
    "_frozen_importlib",
    "_frozen_importlib_external",
    "_functools",
    "_gdbm",
    "_hashlib",
    "_heapq",
    "_hmac",
    "_imp",
    "_interpchannels",
    "_interpqueues",
    "_interpreters",
    "_io",
    "_ios_support",
    "_json",
    "_locale",
    "_lsprof",
    "_lzma",
    "_markupbase",
    "_md5",
    "_msi",
    "_multibytecodec",
    "_multiprocessing",
    "_opcode",
    "_opcode_metadata",
    "_operator",
    "_osx_support",
    "_overlapped",
    "_pickle",
    "_posixshmem",
    "_posixsubprocess",
    "_py_abc",
    "_py_warnings",
    "_pydatetime",
    "_pydecimal",
    "_pyio",
    "_pylong",
    "_pyrepl",
    "_queue",
    "_random",
    "_remote_debugging",
    "_scproxy",
    "_sha1",
    "_sha2",
    "_sha3",
    "_signal",
    "_sitebuiltins",
    "_socket",
    "_sqlite3",
    "_sre",
    "_ssl",
    "_stat",
    "_statistics",
    "_string",
    "_strptime",
    "_struct",
    "_suggestions",
    "_symtable",
    "_sysconfig",
    "_thread",
    "_threading_local",
    "_tkinter",
    "_tokenize",
    "_tracemalloc",
    "_types",
    "_typing",
    "_uuid",
    "_warnings",
    "_weakref",
    "_weakrefset",
    "_winapi",
    "_wmi",
    "_zoneinfo",
    "_zstd",
    "abc",
    "aifc",
    "annotationlib",
    "antigravity",
    "argparse",
    "array",
    "ast",
    "asynchat",
    "asyncio",
    "asyncore",
    "atexit",
    "audioop",
    "base64",
    "bdb",
    "binascii",
    "bisect",
    "builtins",
    "bz2",
    "cProfile",
    "calendar",
    "cgi",
    "cgitb",
    "chunk",
    "cmath",
    "cmd",
    "code",
    "codecs",
    "codeop",
    "collections",
    "colorsys",
    "compileall",
    "compression",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "crypt",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "distutils",
    "doctest",
    "email",
    "encodings",
    "ensurepip",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "genericpath",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "graphlib",
    "grp",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "idlelib",
    "imaplib",
    "imghdr",
    "imp",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "lib2to3",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "mailbox",
    "mailcap",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "msilib",
    "msvcrt",
    "multiprocessing",
    "netrc",
    "nis",
    "nntplib",
    "nt",
    "ntpath",
    "nturl2path",
    "numbers",
    "opcode",
    "operator",
    "optparse",
    "os",
    "ossaudiodev",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pipes",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "pydoc_data",
    "pyexpat",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtpd",
    "smtplib",
    "sndhdr",
    "socket",
    "socketserver",
    "spwd",
    "sqlite3",
    "sre_compile",
    "sre_constants",
    "sre_parse",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "sunau",
    "symtable",
    "sys",
    "sysconfig",
    "syslog",
    "tabnanny",
    "tarfile",
    "telnetlib",
    "tempfile",
    "termios",
    "textwrap",
    "this",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "turtledemo",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uu",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "winreg",
    "winsound",
    "wsgiref",
    "xdrlib",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
    "zoneinfo",
];

/// Whether `first` (an import's first dotted segment) is stdlib.
fn is_stdlib(first: &str) -> bool {
    STDLIB.binary_search(&first).is_ok()
}

/// Statement-level query: every `import` / `from`-`import` with its capture
/// kind (`imp` vs `from`) and 0-based rows. Field detail comes from
/// per-statement snippet re-parses (see [`extract_imports`]).
const STMT_QUERY: &str = "[(import_statement) @imp (import_from_statement) @from]";
/// Plain names of one `import` statement, in source order.
const IMP_PLAIN_QUERY: &str = "(import_statement name: (dotted_name) @mod)";
/// Aliased names of one `import` statement: flat `mod, alias` pairs.
const IMP_ALIASED_QUERY: &str =
    "(import_statement name: (aliased_import name: (dotted_name) @mod alias: (identifier) @alias))";
/// Absolute module of one `from`-`import` (absent when relative).
const FROM_BASE_QUERY: &str = "(import_from_statement module_name: (dotted_name) @base)";
/// The leading dots of a relative `from`-`import`; text length is the level.
const FROM_DOTS_QUERY: &str = "(import_prefix) @dots";
/// The module after the dots of a relative `from`-`import`, if any.
const FROM_RELBASE_QUERY: &str = "(relative_import (dotted_name) @rel)";
/// Plain imported names of one `from`-`import`, in source order.
const FROM_ITEM_QUERY: &str = "(import_from_statement name: (dotted_name) @item)";
/// Aliased imported names: flat `item, alias` pairs.
const FROM_ITEM_ALIASED_QUERY: &str = "(import_from_statement name: (aliased_import name: (dotted_name) @item alias: (identifier) @alias))";
/// Star marker of `from m import *`.
const FROM_STAR_QUERY: &str = "(wildcard_import) @star";

/// One syntactic Python import before ownership resolution.
struct PyUse {
    from: ModuleId,
    /// Display path: dotted as written (`a.b`), with leading dots kept for
    /// relative forms (`.pkg.m`, `...x`).
    path: String,
    /// Anchored absolute segments; empty when the relative climb overflowed
    /// the root (unresolvable, never a fabricated parent).
    dotted: Vec<String>,
    /// `True` for `from`-imported names and star modules: only the full
    /// path edges, anything shorter is `SCOPE001`. Plain `import`s take the
    /// longest-prefix edge instead (amendment-A rule).
    item_ctx: bool,
    /// Relative-import dot count (`0` = absolute). Relative misses are
    /// `SCOPE001`, never `EXT001`: a relative import cannot name an external.
    level: usize,
    /// Captured `as` alias; diagnostics-only, never mints IDs (mirrors Rust).
    alias: Option<String>,
    span: Option<SourceSpan>,
}

/// A discovered module plus its dotted path under the import root.
struct Entry {
    dotted: Vec<String>,
    module: Module,
}

fn root_id(target: &Target) -> ModuleId {
    ModuleId(format!("{}::{}", target.package_id.0, target.name))
}

fn file_entry(target: &Target, dotted: &[String], file: PathBuf) -> Entry {
    let last = dotted.last().cloned().unwrap_or_default();
    Entry {
        dotted: dotted.to_vec(),
        module: Module {
            id: make_module_id(&target.package_id.0, &target.name, dotted),
            target_id: target.id.clone(),
            name: last,
            qualified_name: dotted.join("."),
            source_path: Some(file),
            source_span: None,
        },
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

/// Recursively walk the import root: `__init__.py` is its package module (no
/// extra segment), namespace dirs (no `__init__.py`) become modules in a
/// post-pass, `pkg.py` vs `pkg/__init__.py` resolves directory-wins with one
/// `MOD003` Warning, and a `pkg.py` vs namespace-dir collision resolves
/// file-wins (Rust `MOD002` precedent) with the directory skipped so no two
/// modules share an ID. Symlinked dirs are never descended into (cycle
/// guard); `__pycache__` holds no `.py` files and is skipped outright.
fn walk_lib(target: &Target) -> (Vec<Entry>, Vec<Diagnostic>) {
    let root = target.root_source.clone();
    let root_file = root.join("__init__.py");
    let mut entries = vec![Entry {
        dotted: Vec::new(),
        module: Module {
            id: root_id(target),
            target_id: target.id.clone(),
            name: target.name.clone(),
            qualified_name: target.name.clone(),
            source_path: Some(if root_file.is_file() {
                root_file
            } else {
                root.clone()
            }),
            source_span: None,
        },
    }];
    let mut diags = Vec::new();
    walk_dir(target, &root, &[], &mut entries, &mut diags);
    // Namespace ancestors: every proper non-empty prefix of a discovered
    // dotted path that has no module of its own is a namespace package
    // (PEP 420) with no file. The chain is then complete, so `mod`
    // containment edges below always find their parent.
    let mut known: HashSet<Vec<String>> = entries.iter().map(|e| e.dotted.clone()).collect();
    let mut namespaces: Vec<Vec<String>> = Vec::new();
    for entry in &entries {
        for len in 1..entry.dotted.len() {
            let prefix = entry.dotted[..len].to_vec();
            if known.insert(prefix.clone()) {
                namespaces.push(prefix);
            }
        }
    }
    for dotted in namespaces {
        let last = dotted.last().cloned().unwrap_or_default();
        entries.push(Entry {
            dotted: dotted.clone(),
            module: Module {
                id: make_module_id(&target.package_id.0, &target.name, &dotted),
                target_id: target.id.clone(),
                name: last,
                qualified_name: dotted.join("."),
                source_path: None,
                source_span: None,
            },
        });
    }
    (entries, diags)
}

fn walk_dir(
    target: &Target,
    dir: &Path,
    dotted: &[String],
    entries: &mut Vec<Entry>,
    diags: &mut Vec<Diagnostic>,
) {
    // Non-root dirs with `__init__.py` are file-backed package modules; the
    // root's own `__init__.py` already backs the root module above.
    if !dotted.is_empty() {
        let init = dir.join("__init__.py");
        if init.is_file() {
            entries.push(file_entry(target, dotted, init));
        }
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
    children.sort();
    // File pass first: ambiguity verdicts (MOD003 dir-wins, namespace
    // file-wins) must precede directory recursion so no ID is claimed twice.
    let mut skipped_dirs: HashSet<String> = HashSet::new();
    for child in &children {
        if !child.is_file() || child.extension().and_then(|e| e.to_str()) != Some("py") {
            continue;
        }
        let Some(fname) = child.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if fname == "__init__.py" {
            continue;
        }
        let stem = child
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let sibling = dir.join(&stem);
        let sibling_is_dir = sibling.is_dir()
            && !sibling
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink());
        if !sibling_is_dir {
            entries.push(file_entry(
                target,
                &[dotted, &[stem]].concat(),
                child.clone(),
            ));
            continue;
        }
        if sibling.join("__init__.py").is_file() {
            diags.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "MOD003".to_string(),
                message: format!(
                    "ambiguous python layout: both {} and {}/__init__.py exist; using package",
                    child.display(),
                    sibling.display()
                ),
                source_span: None,
            });
        } else {
            // Namespace collision (`pkg.py` vs `pkg/` without `__init__`):
            // the file wins, the directory is not walked.
            skipped_dirs.insert(stem.clone());
            entries.push(file_entry(
                target,
                &[dotted, &[stem]].concat(),
                child.clone(),
            ));
        }
    }
    for child in &children {
        if !child.is_dir()
            || child
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            || child.file_name().and_then(|n| n.to_str()) == Some("__pycache__")
        {
            continue;
        }
        let Some(name) = child.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if skipped_dirs.contains(name) {
            continue;
        }
        let mut child_dotted = dotted.to_vec();
        child_dotted.push(name.to_string());
        walk_dir(target, child, &child_dotted, entries, diags);
    }
}

/// A `Bin` target is a single root module for its file. A `root_source`
/// that is not a readable file (missing-module script entries from
/// discovery land here) fails closed: the module is still registered and
/// analysis continues, but an IO-family Error diagnostic is recorded. Only
/// metadata is inspected — the path is never executed, canonicalized, or
/// left to escape the workspace.
fn walk_bin(target: &Target) -> (Vec<Entry>, Vec<Diagnostic>) {
    let entries = vec![Entry {
        dotted: Vec::new(),
        module: Module {
            id: root_id(target),
            target_id: target.id.clone(),
            name: target.name.clone(),
            qualified_name: target.name.clone(),
            source_path: Some(target.root_source.clone()),
            source_span: None,
        },
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

/// Span for one extracted name: the statement's file row plus the name's
/// row inside the re-parsed snippet (the snippet starts on the statement's
/// first line, so the sum is the file row), 1-based for `SourceSpan`.
fn use_span(path: &Path, stmt: &MatchedNode, inner: &MatchedNode) -> SourceSpan {
    SourceSpan {
        file: path.to_path_buf(),
        line_start: (stmt.start_row + inner.start_row) as u32 + 1,
        line_end: (stmt.start_row + inner.end_row) as u32 + 1,
    }
}

fn split_dotted(text: &str) -> Vec<String> {
    text.split('.').map(str::to_string).collect()
}

/// Anchor a relative import at the importer: `__init__.py` anchors at its
/// own dotted path, other modules at their parent; then climb `level - 1`.
/// Climbing above the root (or a vacuous top-level result) yields an empty
/// path, which resolution reports without an edge.
fn anchor(
    importer: &[String],
    is_pkg: bool,
    level: usize,
    module: &[String],
    name: Option<&str>,
) -> Vec<String> {
    if level == 0 {
        let mut dotted = module.to_vec();
        if let Some(name) = name {
            dotted.push(name.to_string());
        }
        return dotted;
    }
    let mut base: Vec<String> = if is_pkg {
        importer.to_vec()
    } else if importer.is_empty() {
        Vec::new()
    } else {
        importer[..importer.len() - 1].to_vec()
    };
    for _ in 1..level {
        if base.pop().is_none() {
            return Vec::new();
        }
    }
    base.extend(module.iter().cloned());
    if let Some(name) = name {
        base.push(name.to_string());
    }
    if base.is_empty() {
        return Vec::new();
    }
    base
}

/// Extract every import of one file into [`PyUse`]s.
///
/// Statements are located with the shared [`query_nodes`] helper; each
/// statement's text is then re-parsed and queried for its fields, so names
/// can never leak across statements (not even `;`-separated ones on one
/// row). Extraction order is irrelevant: dedup + total sort at the end make
/// the graph deterministic regardless.
fn extract_imports(
    from: &ModuleId,
    importer: &[String],
    path: &Path,
) -> Result<Vec<PyUse>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let tree = parse(language.clone(), &text)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
    let stmts = query_nodes(&tree, &language, &text, STMT_QUERY)
        .map_err(|e| format!("python import query failed for {}: {e}", path.display()))?;
    let is_pkg = path.file_name().is_some_and(|n| n == "__init__.py");
    let mut out = Vec::new();
    for stmt in &stmts {
        // Re-parse the single statement: field queries below then apply to
        // exactly this statement, with rows relative to its first line.
        let snippet = stmt.text.clone();
        let snippet_tree = parse(language.clone(), &snippet)
            .map_err(|e| format!("failed to re-parse import in {}: {e}", path.display()))?;
        let query = |q: &str| {
            query_nodes(&snippet_tree, &language, &snippet, q)
                .map_err(|e| format!("python import query failed for {}: {e}", path.display()))
        };
        if stmt.capture == "imp" {
            for plain in query(IMP_PLAIN_QUERY)? {
                let module = split_dotted(&plain.text);
                out.push(PyUse {
                    from: from.clone(),
                    path: module.join("."),
                    dotted: anchor(importer, is_pkg, 0, &module, None),
                    item_ctx: false,
                    level: 0,
                    alias: None,
                    span: Some(use_span(path, stmt, &plain)),
                });
            }
            for pair in query(IMP_ALIASED_QUERY)?.chunks(2) {
                if let [dotted_node, alias_node] = pair {
                    let module = split_dotted(&dotted_node.text);
                    out.push(PyUse {
                        from: from.clone(),
                        path: module.join("."),
                        dotted: anchor(importer, is_pkg, 0, &module, None),
                        item_ctx: false,
                        level: 0,
                        alias: Some(alias_node.text.clone()),
                        span: Some(use_span(path, stmt, dotted_node)),
                    });
                }
            }
        } else {
            let dots = query(FROM_DOTS_QUERY)?;
            let level = dots.first().map(|d| d.text.len()).unwrap_or(0);
            let module = match query(FROM_BASE_QUERY)?.first() {
                Some(base) => split_dotted(&base.text),
                None => match query(FROM_RELBASE_QUERY)?.first() {
                    Some(rel) => split_dotted(&rel.text),
                    None => Vec::new(),
                },
            };
            let dots_prefix = ".".repeat(level);
            let rest = module.join(".");
            for plain in query(FROM_ITEM_QUERY)? {
                let item = format!(
                    "{rest}{dot}{}",
                    plain.text,
                    dot = if rest.is_empty() { "" } else { "." }
                );
                out.push(PyUse {
                    from: from.clone(),
                    path: format!("{dots_prefix}{item}"),
                    dotted: anchor(importer, is_pkg, level, &module, Some(&plain.text)),
                    item_ctx: true,
                    level,
                    alias: None,
                    span: Some(use_span(path, stmt, &plain)),
                });
            }
            for pair in query(FROM_ITEM_ALIASED_QUERY)?.chunks(2) {
                if let [item_node, alias_node] = pair {
                    let item = format!(
                        "{rest}{dot}{}",
                        item_node.text,
                        dot = if rest.is_empty() { "" } else { "." }
                    );
                    out.push(PyUse {
                        from: from.clone(),
                        path: format!("{dots_prefix}{item}"),
                        dotted: anchor(importer, is_pkg, level, &module, Some(&item_node.text)),
                        item_ctx: true,
                        level,
                        alias: Some(alias_node.text.clone()),
                        span: Some(use_span(path, stmt, item_node)),
                    });
                }
            }
            // `from m import *` edges to the resolved module itself.
            if let Some(star) = query(FROM_STAR_QUERY)?.first() {
                out.push(PyUse {
                    from: from.clone(),
                    path: format!("{dots_prefix}{rest}"),
                    dotted: anchor(importer, is_pkg, level, &module, None),
                    item_ctx: true,
                    level,
                    alias: None,
                    span: Some(use_span(path, stmt, star)),
                });
            }
        }
    }
    Ok(out)
}

/// Resolution outcome for one [`PyUse`]: an edge target, silence, or a
/// diagnostic with no edge.
enum Resolution {
    Edge(DependencyTarget),
    Unresolved(Diagnostic),
    Skip,
}

/// Longest-prefix module attribution: the full `dotted` path preferred,
/// else the longest strict prefix of at least one segment naming a known
/// module. Returns the match plus its length so callers can tell full
/// (edge) from strict (edge for plain imports, `SCOPE001` for items).
fn longest_prefix(
    tree: &HashMap<Vec<String>, ModuleId>,
    dotted: &[String],
) -> (Option<ModuleId>, usize) {
    for len in (1..=dotted.len()).rev() {
        if let Some(id) = tree.get(&dotted[..len]) {
            return (Some(id.clone()), len);
        }
    }
    (None, 0)
}

/// Guard-only package-name normalization (lowercase, `-` → `_`): a
/// hyphenated or uppercase distribution name (`my-pkg`) still owns the
/// matching import namespace (`my_pkg`). Applies ONLY to the own-package
/// guard comparison — display paths, diagnostics, `ModuleId`s, and edge
/// targets stay verbatim.
fn norm_pkg_name(s: &str) -> String {
    s.to_ascii_lowercase().replace('-', "_")
}

fn resolve_use(
    raw: &PyUse,
    pkg: &str,
    tree: &HashMap<Vec<String>, ModuleId>,
    lib_trees: &[&HashMap<Vec<String>, ModuleId>],
    deps_by_name: &HashMap<&str, Vec<&crate::metadata::ExternalDep>>,
) -> Resolution {
    // Aliases are diagnostics-only and never mint IDs.
    let _ = raw.alias.as_deref();
    if raw.dotted.is_empty() {
        return Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span));
    }
    let (hit, len) = longest_prefix(tree, &raw.dotted);
    // Own-package absolute imports strip the package segment and resolve
    // the remainder against the package's `Lib` trees — but only on a total
    // owning-tree miss, so verbatim paths (and amendment-A strict matches)
    // stay primary and a `Bin`-only root can never shadow: `longest_prefix`
    // never tries length 0, so a root-only tree matches no real remainder.
    // Exactly one `Lib` match → `Internal`; zero or several → `SCOPE001`
    // (own namespace is never external).
    if hit.is_none()
        && raw.level == 0
        && norm_pkg_name(&raw.dotted[0]) == norm_pkg_name(pkg)
        && !lib_trees.is_empty()
    {
        let remainder = &raw.dotted[1..];
        let mut hits: Vec<ModuleId> = Vec::new();
        for lib in lib_trees {
            let hit = if remainder.is_empty() {
                lib.get(&Vec::new()).cloned()
            } else {
                longest_prefix(lib, remainder).0
            };
            if let Some(id) = hit {
                hits.push(id);
            }
        }
        if hits.len() == 1 {
            return Resolution::Edge(DependencyTarget::Internal(hits.swap_remove(0)));
        }
        return Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span));
    }
    match hit {
        Some(id) if len == raw.dotted.len() => Resolution::Edge(DependencyTarget::Internal(id)),
        Some(id) if !raw.item_ctx => Resolution::Edge(DependencyTarget::Internal(id)),
        Some(_) => Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span)),
        None => {
            let first = raw.dotted[0].as_str();
            if is_stdlib(first) {
                return Resolution::Skip;
            }
            match deps_by_name.get(first) {
                Some(versions) if versions.len() == 1 => {
                    let dep = versions[0];
                    Resolution::Edge(DependencyTarget::External(make_external_id(
                        &dep.name,
                        &dep.version,
                    )))
                }
                // Several pins share this name: ownership is ambiguous, so
                // the single SCOPE001 text applies (mirrors Rust), no edge.
                Some(_) => Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span)),
                // A relative import can never name an external: its miss is
                // an ownership failure (SCOPE001), not a pin failure.
                None if raw.level > 0 => {
                    Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span))
                }
                None => Resolution::Unresolved(ext_diag(&raw.from, &raw.path, &raw.span)),
            }
        }
    }
}

/// The Python whole-workspace [`Analyzer`]: per-target walk → extract →
/// resolve → dedup → sort, calling the shared `graph`/`diag` helpers.
pub struct PyAnalyzer;

impl Analyzer for PyAnalyzer {
    fn analyze(&self, input: &AnalysisInput) -> Result<WorkspaceGraph, String> {
        // 1. Per-target discovery.
        let mut modules_all: Vec<Module> = Vec::new();
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let mut dotted_of: HashMap<ModuleId, Vec<String>> = HashMap::new();
        let mut trees: HashMap<TargetId, HashMap<Vec<String>, ModuleId>> = HashMap::new();
        let mut owner_to_target: HashMap<ModuleId, TargetId> = HashMap::new();
        for target in &input.targets {
            let (entries, mut diags) = match target.kind {
                TargetKind::Lib => walk_lib(target),
                TargetKind::Bin => walk_bin(target),
            };
            diagnostics.append(&mut diags);
            let mut tree: HashMap<Vec<String>, ModuleId> = HashMap::new();
            for entry in entries {
                tree.insert(entry.dotted.clone(), entry.module.id.clone());
                dotted_of.insert(entry.module.id.clone(), entry.dotted.clone());
                owner_to_target.insert(entry.module.id.clone(), target.id.clone());
                modules_all.push(entry.module);
            }
            trees.insert(target.id.clone(), tree);
        }
        let deps_by_name: HashMap<&str, Vec<&crate::metadata::ExternalDep>> = {
            let mut map: HashMap<&str, Vec<&crate::metadata::ExternalDep>> = HashMap::new();
            for dep in &input.deps {
                map.entry(dep.name.as_str()).or_default().push(dep);
            }
            map
        };

        // 2. Extraction: one pass per file-backed module. Directories and
        // missing files carry no imports; unreadable files are IO001 and
        // discovery continues.
        let mut uses: Vec<PyUse> = Vec::new();
        for module in &modules_all {
            let Some(path) = module.source_path.clone() else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            let dotted = dotted_of.get(&module.id).cloned().unwrap_or_default();
            match extract_imports(&module.id, &dotted, &path) {
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
            let Some(dotted) = dotted_of.get(&module.id) else {
                continue;
            };
            if dotted.is_empty() {
                continue;
            }
            let parent = if dotted.len() == 1 {
                let Some(target) = input.targets.iter().find(|t| &t.id == target_id) else {
                    continue;
                };
                root_id(target)
            } else {
                let Some(parent_id) = tree.get(&dotted[..dotted.len() - 1]) else {
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

        // 4. Resolve every `PyUse` against its owning target's tree, plus
        // the package's `Lib` trees for the own-package absolute arm.
        // No CFG001 walk: the single-run conditional-compilation
        // diagnostic is Rust-only per spec §2, so `TYPE_CHECKING` imports
        // above already resolved normally.
        for raw in &uses {
            let Some(target_id) = owner_to_target.get(&raw.from) else {
                diagnostics.push(scope_diag(&raw.from, &raw.path, &raw.span));
                continue;
            };
            let Some(tree) = trees.get(target_id) else {
                diagnostics.push(scope_diag(&raw.from, &raw.path, &raw.span));
                continue;
            };
            let Some(owner) = input.targets.iter().find(|t| &t.id == target_id) else {
                diagnostics.push(scope_diag(&raw.from, &raw.path, &raw.span));
                continue;
            };
            let lib_trees: Vec<&HashMap<Vec<String>, ModuleId>> = input
                .targets
                .iter()
                .filter(|t| t.kind == TargetKind::Lib && t.package_id == owner.package_id)
                .filter_map(|t| trees.get(&t.id))
                .collect();
            match resolve_use(raw, &owner.package_id.0, tree, &lib_trees, &deps_by_name) {
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
                manifest_path: input.workspace_root.join("pyproject.toml"),
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
    fn stdlib_list_sorted_and_spotted() {
        assert!(!STDLIB.is_empty(), "stdlib list must not be empty");
        let mut sorted = STDLIB.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, STDLIB, "STDLIB must stay sorted for binary_search");
        assert_eq!(
            STDLIB.len(),
            STDLIB.iter().collect::<HashSet<_>>().len(),
            "STDLIB must not contain duplicates"
        );
        for member in ["os", "sys", "json", "typing", "asyncio", "pathlib"] {
            assert!(is_stdlib(member), "{member} must be stdlib-skipped");
        }
        for non_member in ["requests", "flask", "web", "nosuchpkg"] {
            assert!(!is_stdlib(non_member), "{non_member} must not be stdlib");
        }
        // Union members removed before 3.14 stay skipped.
        for removed in [
            "aifc",
            "telnetlib",
            "imp",
            "smtpd",
            "distutils",
            "lib2to3",
            "uu",
            "_compression",
            "_msi",
        ] {
            assert!(
                is_stdlib(removed),
                "{removed} must survive from the 3.11–3.13 union"
            );
        }
    }
}
