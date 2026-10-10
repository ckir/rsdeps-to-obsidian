//! C# module discovery and `using` resolution ([`CsAnalyzer`]).
//!
//! [`CsAnalyzer`] runs the whole-workspace pipeline for a C# project:
//! per-target file discovery, tree-sitter `using`/namespace extraction,
//! longest-prefix resolution, then the shared dedup + total-order sort.
//! Analysis is file-only: project files are read, never executed, and
//! nothing ever shells out to `dotnet`.
//!
//! # Module identity
//!
//! Each `namespace_declaration` / `file_scoped_namespace_declaration` block
//! is one module; the dotted path becomes `::`-joined [`ModuleId`] segments
//! (`MyApp.Services` -> `{pkg}::{tgt}::MyApp::Services`). Lexically nested
//! namespace blocks nest their paths (an inner `B` inside `A` is `A::B`).
//! A namespace repeated in one project (partial classes or not) merges to
//! ONE node (`source_path` = first sorted file, `source_span` = the first
//! declaration in that file); two namespaces in one file are distinct IDs
//! sharing the file with distinct spans. Merged namespace nodes resolve the
//! FIRST sorted file's usings only (later files' usings are not unioned).
//! Namespace-less files (top-level
//! statements) fall back to one stem module from the project-relative path
//! segments + stem (TypeScript precedent). The root module is exactly two
//! segments, `{package}::{target.name}`.
//!
//! Cross-file span navigation is lossy by construction: a merged module
//! points at its first sorted file, so declarations in later files share
//! the node's identity but not its span (the schema carries one span per
//! module). `Lib` and `Bin` targets walk identically (an `Exe` project has
//! no single entry file; all its modules belong to its tree).
//!
//! # Resolution context and rules (brief-governed, spec section 9)
//!
//! Each module resolves its file's explicit `using` directives. The
//! project-wide explicit `global using` set (collected in walk phase 1
//! across the target's files) and the pinned per-TFM implicit sets (union
//! only of the target's TFMs) are retained in the walk structs as context,
//! but they are gated to silent: ambient entries never produce edges and
//! never produce diagnostics (spec section 9.2). Only explicit per-file
//! usings yield edges or diagnostics, so one unresolvable `global using`
//! cannot flood every module with edges or `SCOPE001`.
//!
//! Resolution is longest-prefix throughout, in order: (a) the own target's
//! declared namespaces; (a2) the same-package fallback (the FULL unmodified
//! segments against the package's OTHER targets' trees, Lib and Bin alike:
//! C# namespaces are global-rooted, so no package prefix is ever stripped;
//! exactly one matching target -> `Internal`, several -> `SCOPE001`
//! conclusively, zero -> fall through); (b) directly-referenced projects'
//! namespaces (one hop, no transitivity; one edge per owning package, so a
//! multi-TFM package still yields one edge; the same namespace in 2+
//! referenced packages -> `SCOPE001`, no edge); (c) `PackageReference`
//! package names by longest prefix anchored at the first segment
//! (case-insensitive): exactly one pin -> `External`, several ->
//! `SCOPE001`, a declared-but-unpinned name -> `EXT001`; (d) the BCL `System*` skip list -> silent skip,
//! consulted before external lookup, after steps a-b. A first segment
//! matching no declared package name at all -> `SCOPE001`. `using static`
//! resolves by its full type path through the same order; aliases resolve
//! by full path with the alias diagnostics-only (never an ID, mirroring
//! Rust/Python/TypeScript). `#if` regions parse inclusively with NO
//! evaluation; one run-level `CFG001` when any `#if` is encountered.
//!
//! The per-target csproj re-read (ancestor walk from `root_source`, the
//! TypeScript tsconfig re-read precedent — no struct changes) supplies the
//! `ProjectReference` list, the `ImplicitUsings` flag, the TFM list, the
//! declared package names, and explicit `Compile` lists. A missing csproj
//! means defaults (no refs, no implicits, recursive walk); a malformed one
//! is fail-closed (`IO001`, defaults, analysis continues). The walk covers
//! `*.cs` recursively under the target root minus `obj/`+`bin/` (skipped
//! always, even when an explicit `Compile` list names them); an explicit
//! `Compile` `Include`/`Remove` pair instead selects exactly those files (a
//! missing literal include is `IO001`, an empty glob match is silent).
//! `ProjectReference`s pointing outside the analyzed targets (dangling or
//! out-of-root) contribute no namespaces and no diagnostic: their usings
//! fall through the order above like any other miss.

use crate::diag::sort_diagnostics;
use crate::domain::{
    Dependency, DependencyKind, DependencyTarget, Diagnostic, DiagnosticSeverity, Module, Package,
    SourceSpan, Target, WorkspaceGraph,
};
use crate::graph::{dedup_dependencies, sort_graph};
use crate::ids::{make_external_id, make_module_id, ModuleId, TargetId};
use crate::resolver::{AnalysisInput, Analyzer};
use crate::ts_parse::{parse, query_nodes};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// SDK default implicit-usings set for `Microsoft.NET.Sdk`.
///
/// Generation: the `<Using Include>` items under the `ImplicitUsings`
/// `enable`/`true` `ItemGroup` in the SDK source file
/// `src/Tasks/Microsoft.NET.Build.Tasks/targets/Microsoft.NET.Sdk.CSharp.props`
/// (`github.com/dotnet/sdk`), restricted to the net6.0-net9.0 range: those
/// TFMs all resolve below version 11.0, so the `System.Net.Http.Json`
/// (net11+ only) condition leaves exactly these seven. Sorted for the unit
/// test below; resolution only needs membership, and every pinned name is
/// BCL-silent in practice.
const IMPLICIT_USINGS_NET: &[&str] = &[
    "System",
    "System.Collections.Generic",
    "System.IO",
    "System.Linq",
    "System.Net.Http",
    "System.Threading",
    "System.Threading.Tasks",
];

/// `System.Net.Http.Json`, implicit only for net11.0+ per the same SDK
/// conditions (`TargetFrameworkIdentifier == .NETCoreApp` and
/// `TargetFrameworkVersion >= 11.0`).
const IMPLICIT_USINGS_NET11_EXTRA: &[&str] = &["System.Net.Http.Json"];

/// The pinned implicit set for one TFM string (verbatim, e.g. `net8.0`):
/// the SDK default above, minus `System.Net.Http` for .NET Framework
/// targets, plus the net11+ extra where the SDK conditions hold.
fn implicit_usings_for_tfm(tfm: &str) -> Vec<&'static str> {
    let lower = tfm.to_ascii_lowercase();
    let is_netfx = lower.starts_with("net4") || lower == "net35" || lower == "net40";
    let mut out: Vec<&'static str> = IMPLICIT_USINGS_NET
        .iter()
        .filter(|u| !is_netfx || **u != "System.Net.Http")
        .copied()
        .collect();
    if lower.starts_with("net") && !lower.starts_with("netstandard") && !is_netfx {
        let version = lower
            .trim_start_matches("net")
            .split('-')
            .next()
            .unwrap_or("");
        let major: u32 = version.split('.').next().unwrap_or("").parse().unwrap_or(0);
        if major >= 11 {
            out.extend(IMPLICIT_USINGS_NET11_EXTRA.iter().copied());
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Pinned BCL silent-skip roots: any `using` whose first dotted segment is
/// `System` is skipped silently (never an edge, never a diagnostic),
/// consulted after steps a-b and before external lookup (Python-stdlib
/// precedent: first-segment membership, exact case — C# is case-sensitive).
/// Sorted for `binary_search`; the unit test below pins sortedness.
const BCL_PREFIXES: &[&str] = &["System"];

/// Whether `first` (a using's first dotted segment) is BCL-silent.
fn is_bcl(first: &str) -> bool {
    BCL_PREFIXES.binary_search(&first).is_ok()
}

/// Namespace declaration nodes in source order (block + file-scoped).
const NS_QUERY: &str = "[(namespace_declaration) @ns (file_scoped_namespace_declaration) @fs]";
/// The dotted name of one namespace-declaration snippet.
const NS_NAME_QUERY: &str =
    "[(namespace_declaration name: (_) @name) (file_scoped_namespace_declaration name: (_) @name)]";
/// Every `using` directive (plain, static, alias, global, wherever nested).
const USE_QUERY: &str = "(using_directive) @use";
/// Any `#if` region (parsed inclusively; presence alone reports `CFG001`).
const CFG_QUERY: &str = "(preproc_if) @cfg";

/// One syntactic C# `using` before ownership resolution.
struct CsUse {
    from: ModuleId,
    /// Display path: dotted as written (`App.Utils`, `System.Math`).
    path: String,
    /// Dotted segments of the full path (for `using static`, the full type
    /// path; for aliases, the full right-hand path).
    segments: Vec<String>,
    /// Captured `as` alias; diagnostics-only, never mints IDs (mirrors
    /// Rust/Python/TypeScript).
    alias: Option<String>,
    /// `true` for the module's own file directives (edge or diagnostic);
    /// `false` for ambient (global/implicit) context, which the analyzer
    /// skips outright: silent, never an edge, never a diagnostic.
    explicit: bool,
    span: Option<SourceSpan>,
}

/// One namespace block: dotted segments plus source rows for nesting and
/// first-declaration spans.
struct NsDecl {
    segments: Vec<String>,
    start_row: usize,
    end_row: usize,
    /// `true` for `file_scoped_namespace_declaration` (owns the rest of the
    /// file for nesting); `false` for block declarations.
    file_scoped: bool,
}

/// Raw `using_directive` text before attribution.
struct RawUsing {
    is_global: bool,
    alias: Option<String>,
    path: String,
    span: SourceSpan,
}

/// One parsed file: namespace blocks, raw usings, and the `#if` flag.
struct FileData {
    decls: Vec<NsDecl>,
    usings: Vec<RawUsing>,
    has_cfg: bool,
}

fn language() -> tree_sitter::Language {
    tree_sitter_c_sharp::LANGUAGE.into()
}

fn root_id(target: &Target) -> ModuleId {
    ModuleId(format!("{}::{}", target.package_id.0, target.name))
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

fn io_diag(path: &Path, detail: &str) -> Diagnostic {
    Diagnostic {
        severity: DiagnosticSeverity::Error,
        code: "IO001".to_string(),
        message: format!("failed to read {}: {detail}", path.display()),
        source_span: None,
    }
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

/// Canonicalize when the path exists, else lexical normalization, so
/// reference matching never fails on a missing dir.
fn best_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| lex_normalize(path))
}

/// Resolve one MSBuild `\\`-tolerant relative reference against `dir`.
fn join_ref(dir: &Path, raw: &str) -> PathBuf {
    lex_normalize(&dir.join(raw.replace('\\', "/")))
}

/// Parse one `using_directive` text: `(is_global, alias, path)`. Returns
/// `None` when no dotted path survives (unparseable residue stays silent;
/// tree-sitter error recovery may hand over fragments).
fn parse_using(text: &str) -> Option<(bool, Option<String>, String)> {
    let stripped = text.trim().strip_suffix(';').unwrap_or(text.trim());
    let mut rest = stripped.trim();
    let mut is_global = false;
    if let Some(after) = rest
        .strip_prefix("global")
        .filter(|after| after.starts_with(char::is_whitespace))
    {
        is_global = true;
        rest = after.trim();
    }
    rest = rest
        .strip_prefix("using")
        .filter(|after| {
            after.is_empty() || after.starts_with(char::is_whitespace) || after.starts_with('=')
        })
        .map(str::trim)?;
    if let Some(after) = rest
        .strip_prefix("static")
        .filter(|after| after.starts_with(char::is_whitespace))
    {
        // `using static` carries no separate flag: it resolves by its full
        // type path through the same order as every other using.
        rest = after.trim();
    }
    let (alias, path) = match rest.split_once('=') {
        Some((left, right)) => {
            let left = left.trim();
            if left.is_empty() {
                (None, right.trim().to_string())
            } else {
                (Some(left.to_string()), right.trim().to_string())
            }
        }
        None => (None, rest.to_string()),
    };
    let path = path.strip_prefix("global::").unwrap_or(&path);
    if path.is_empty() {
        return None;
    }
    Some((is_global, alias, path.to_string()))
}

fn split_dotted(path: &str) -> Vec<String> {
    path.split('.')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse one `.cs` file: namespace blocks (with rows for nesting/merging),
/// raw usings (with rows for spans), and the `#if` flag.
fn extract_file(path: &Path) -> Result<FileData, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let lang = language();
    let tree = parse(lang.clone(), &text)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
    let mut decls = Vec::new();
    for node in query_nodes(&tree, &lang, &text, NS_QUERY)
        .map_err(|e| format!("csharp namespace query failed for {}: {e}", path.display()))?
    {
        // Re-parse the single declaration: the name query then applies to
        // exactly this block, with the block's own name first.
        let snippet = node.text.clone();
        let snippet_tree = parse(lang.clone(), &snippet)
            .map_err(|e| format!("failed to re-parse namespace in {}: {e}", path.display()))?;
        let query = |q: &str| {
            query_nodes(&snippet_tree, &lang, &snippet, q)
                .map_err(|e| format!("csharp namespace query failed for {}: {e}", path.display()))
        };
        if let Some(name) = query(NS_NAME_QUERY)?.first() {
            let segments = split_dotted(&name.text);
            if !segments.is_empty() {
                decls.push(NsDecl {
                    segments,
                    start_row: node.start_row,
                    end_row: node.end_row,
                    file_scoped: node.capture == "fs",
                });
            }
        }
    }
    let mut usings = Vec::new();
    for node in query_nodes(&tree, &lang, &text, USE_QUERY)
        .map_err(|e| format!("csharp using query failed for {}: {e}", path.display()))?
    {
        if let Some((is_global, alias, path_text)) = parse_using(&node.text) {
            usings.push(RawUsing {
                is_global,
                alias,
                path: path_text,
                span: SourceSpan {
                    file: path.to_path_buf(),
                    line_start: node.start_row as u32 + 1,
                    line_end: node.end_row as u32 + 1,
                },
            });
        }
    }
    let has_cfg = !query_nodes(&tree, &lang, &text, CFG_QUERY)
        .map_err(|e| format!("csharp preproc query failed for {}: {e}", path.display()))?
        .is_empty();
    Ok(FileData {
        decls,
        usings,
        has_cfg,
    })
}

/// Lowercased element/attribute name (MSBuild XML is case-insensitive).
fn lower(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_lowercase()
}

/// One attribute value by lowercased key.
fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Per-target csproj view from the re-read: referenced project paths, the
/// `ImplicitUsings` flag, the TFM list, declared package names (any
/// `PackageReference`, pinned or not — the `EXT001` arm needs the
/// declared-but-unpinned ones), and explicit `Compile` lists.
struct CsProjCtx {
    dir: PathBuf,
    project_refs: Vec<PathBuf>,
    implicit_on: bool,
    tfms: Vec<String>,
    package_names: Vec<String>,
    compile: Option<(Vec<String>, Vec<String>)>,
}

impl CsProjCtx {
    fn defaults(dir: PathBuf) -> Self {
        Self {
            dir,
            project_refs: Vec::new(),
            implicit_on: false,
            tfms: Vec::new(),
            package_names: Vec::new(),
            compile: None,
        }
    }
}

/// The nearest `*.csproj` at or above `start` (lexicographically smallest
/// wins a shared dir), or `start` itself when it is a csproj file.
fn find_csproj(start: &Path) -> Option<PathBuf> {
    if start.is_file()
        && start
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("csproj"))
    {
        return Some(start.to_path_buf());
    }
    let mut dir = if start.is_file() {
        start.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        start.to_path_buf()
    };
    loop {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut names: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.is_file()
                        && p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| e.eq_ignore_ascii_case("csproj"))
                })
                .collect();
            names.sort();
            if let Some(first) = names.into_iter().next() {
                return Some(first);
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Re-read one csproj for the resolver's fields. Only text captures count
/// (`<ImplicitUsings>`, `<TargetFramework(s)>`; self-closed empties carry
/// no text); `Compile` honors `Include` + `Remove` (`Update` needs SDK-glob
/// evaluation and is ignored); `ProjectReference` honors `Include` only;
/// `PackageReference` `Include` + `Update` both declare names.
/// Well-formedness failures are `Err` (the caller fails closed).
fn read_csproj(path: &Path) -> Result<CsProjCtx, String> {
    use quick_xml::events::Event;
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut buf = Vec::new();
    let mut ctx = CsProjCtx::defaults(dir);
    let mut capture: Option<&str> = None;
    let mut capture_buf = String::new();
    let mut includes: Vec<String> = Vec::new();
    let mut removes: Vec<String> = Vec::new();
    let mut saw_root = false;
    // Shared attribute handling for `<X ...>` and `<X ... />` alike (only
    // text captures differ: self-closed empties carry no text).
    let element = |ctx: &mut CsProjCtx,
                   includes: &mut Vec<String>,
                   removes: &mut Vec<String>,
                   name: &str,
                   attrs: &[(String, String)]| {
        match name {
            "projectreference" => {
                if let Some(id) = attr(attrs, "include") {
                    if !id.trim().is_empty() {
                        ctx.project_refs.push(join_ref(&ctx.dir, id.trim()));
                    }
                }
            }
            "packagereference" => {
                if let Some(id) = attr(attrs, "include").or_else(|| attr(attrs, "update")) {
                    if !id.trim().is_empty() {
                        ctx.package_names.push(id.trim().to_string());
                    }
                }
            }
            "compile" => {
                if let Some(p) = attr(attrs, "include") {
                    if !p.trim().is_empty() {
                        includes.push(p.trim().to_string());
                    }
                }
                if let Some(p) = attr(attrs, "remove") {
                    if !p.trim().is_empty() {
                        removes.push(p.trim().to_string());
                    }
                }
            }
            _ => {}
        }
    };
    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(format!("invalid XML in {}: {e}", path.display())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                saw_root = true;
                let name = lower(e.local_name().as_ref());
                let mut attrs: Vec<(String, String)> = Vec::new();
                for attr in e.attributes() {
                    let attr =
                        attr.map_err(|e| format!("invalid XML in {}: {e}", path.display()))?;
                    let value = attr
                        .unescape_value()
                        .map_err(|e| format!("invalid XML in {}: {e}", path.display()))?
                        .into_owned();
                    attrs.push((lower(attr.key.as_ref()), value));
                }
                element(&mut ctx, &mut includes, &mut removes, &name, &attrs);
                if capture.is_none() {
                    match name.as_str() {
                        "implicitusings" => {
                            capture = Some("implicit");
                            capture_buf.clear();
                        }
                        "targetframeworks" => {
                            capture = Some("tfms");
                            capture_buf.clear();
                        }
                        "targetframework" => {
                            capture = Some("tfm");
                            capture_buf.clear();
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                saw_root = true;
                let name = lower(e.local_name().as_ref());
                let mut attrs: Vec<(String, String)> = Vec::new();
                for attr in e.attributes() {
                    let attr =
                        attr.map_err(|e| format!("invalid XML in {}: {e}", path.display()))?;
                    let value = attr
                        .unescape_value()
                        .map_err(|e| format!("invalid XML in {}: {e}", path.display()))?
                        .into_owned();
                    attrs.push((lower(attr.key.as_ref()), value));
                }
                element(&mut ctx, &mut includes, &mut removes, &name, &attrs);
            }
            Ok(Event::Text(e)) => {
                if capture.is_some() {
                    let text = e
                        .xml_content()
                        .map_err(|e| format!("invalid XML in {}: {e}", path.display()))?;
                    capture_buf.push_str(&text);
                }
            }
            Ok(Event::End(e)) => {
                let name = lower(e.local_name().as_ref());
                let done = match capture {
                    Some("implicit") => name == "implicitusings",
                    Some("tfms") => name == "targetframeworks",
                    Some("tfm") => name == "targetframework",
                    _ => false,
                };
                if done {
                    let value = capture_buf.trim().to_string();
                    match capture {
                        // Repeated properties are last-wins (the MSBuild
                        // semantic); duplicates are pathological either way.
                        Some("implicit") => {
                            ctx.implicit_on = value.eq_ignore_ascii_case("enable")
                                || value.eq_ignore_ascii_case("true");
                        }
                        Some("tfms") | Some("tfm") => {
                            ctx.tfms.extend(
                                value
                                    .split(';')
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty()),
                            );
                        }
                        _ => {}
                    }
                    capture = None;
                }
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
    ctx.tfms.sort();
    ctx.tfms.dedup();
    ctx.package_names.sort();
    ctx.package_names.dedup();
    if !includes.is_empty() || !removes.is_empty() {
        ctx.compile = Some((includes, removes));
    }
    Ok(ctx)
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

/// Sorted direct-child dirs of `dir`, excluding symlinked dirs (cycle
/// guard); `obj/`+`bin/` filtering is the caller's job.
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

/// Whether `path` is (or sits under) a build-output dir: skipped always,
/// even when an explicit `Compile` list names it.
fn is_output_dir(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("obj") || s.eq_ignore_ascii_case("bin"))
    })
}

/// Recursively collect `*.cs` under `dir` (sorted, deterministic),
/// skipping `obj/`+`bin/` subtrees and symlinked dirs.
fn collect_cs(dir: &Path, out: &mut Vec<PathBuf>) {
    for file in sorted_files(dir) {
        if file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("cs"))
            && !is_output_dir(&file)
        {
            out.push(file);
        }
    }
    for sub in sorted_dirs(dir) {
        if !is_output_dir(&sub) {
            collect_cs(&sub, out);
        }
    }
}

/// Match MSBuild `Include`/`Remove` patterns (`**` spans dirs, `*`/`?`
/// stay in one segment; backslashes already normalized to `/`).
fn glob_match(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        return (0..=path.len()).any(|i| glob_match(&pattern[1..], &path[i..]));
    }
    if path.is_empty() || !segment_match(pattern[0], path[0]) {
        return false;
    }
    glob_match(&pattern[1..], &path[1..])
}

/// One case-sensitive segment with `*`/`?` wildcards.
fn segment_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Expand explicit `Compile` lists against `base` (the csproj dir):
/// literals must exist, globs match silently-empty, `Remove` filters, and
/// `obj/`+`bin/` never survive. Missing literals are `IO001`.
fn expand_compile(
    base: &Path,
    includes: &[String],
    removes: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let mut all = Vec::new();
    collect_cs(base, &mut all);
    let rel_of = |f: &Path| {
        f.strip_prefix(base)
            .ok()
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    };
    let matches_any = |rel: &str, patterns: &[String]| {
        patterns.iter().any(|raw| {
            let norm = raw.replace('\\', "/");
            let norm = norm.trim_start_matches("./");
            if norm.contains(['*', '?']) {
                glob_match(
                    &norm.split('/').collect::<Vec<_>>(),
                    &rel.split('/').collect::<Vec<_>>(),
                )
            } else {
                lex_normalize(Path::new(norm))
                    .to_string_lossy()
                    .replace('\\', "/")
                    == *rel
            }
        })
    };
    let mut picked: Vec<PathBuf> = Vec::new();
    for raw in includes {
        let norm = raw.replace('\\', "/");
        if !norm.contains(['*', '?']) {
            let candidate = lex_normalize(&base.join(norm.trim_start_matches("./")));
            if is_output_dir(&candidate) {
                continue;
            }
            if candidate.is_file() {
                picked.push(candidate);
            } else {
                let detail = match std::fs::symlink_metadata(&candidate) {
                    Ok(_) => "not a regular file".to_string(),
                    Err(e) => e.to_string(),
                };
                diags.push(io_diag(&candidate, &detail));
            }
            continue;
        }
        for file in &all {
            if matches_any(&rel_of(file), std::slice::from_ref(raw)) {
                picked.push(file.clone());
            }
        }
    }
    picked.retain(|f| !matches_any(&rel_of(f), removes));
    picked.sort();
    picked.dedup();
    picked
}

/// A discovered module plus where its facts came from.
struct Entry {
    segments: Vec<String>,
    module: Module,
    file: Option<PathBuf>,
}

/// The per-target walk: merged entries, project globals, per-target
/// implicits, per-file raw usings, the `#if` flag, and the re-read csproj
/// view (refs + declared names for phase 2).
struct TargetWalk {
    entries: Vec<Entry>,
    globals: Vec<(String, Vec<String>, SourceSpan)>,
    implicits: HashSet<Vec<String>>,
    file_usings: Vec<(PathBuf, Vec<RawUsing>)>,
    has_cfg: bool,
    ctx: CsProjCtx,
}

fn entry_module(
    target: &Target,
    segments: &[String],
    file: Option<PathBuf>,
    span: Option<SourceSpan>,
) -> Module {
    if segments.is_empty() {
        Module {
            id: root_id(target),
            target_id: target.id.clone(),
            name: target.name.clone(),
            qualified_name: target.name.clone(),
            source_path: file,
            source_span: span,
        }
    } else {
        Module {
            id: make_module_id(&target.package_id.0, &target.name, segments),
            target_id: target.id.clone(),
            name: segments.last().cloned().unwrap_or_default(),
            qualified_name: segments.join("."),
            source_path: file,
            source_span: span,
        }
    }
}

/// Relative dotted segments + stem for a namespace-less file (verbatim;
/// no `index` special-casing: C# has no barrel convention).
fn stem_segments(base: &Path, file: &Path) -> Vec<String> {
    let mut segments: Vec<String> = file
        .strip_prefix(base)
        .ok()
        .and_then(|r| r.parent())
        .map(|p| {
            p.components()
                .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if let Some(stem) = file.file_stem().and_then(|s| s.to_str()) {
        segments.push(stem.to_string());
    }
    segments
}

fn walk_target(target: &Target, diags: &mut Vec<Diagnostic>) -> TargetWalk {
    // The csproj re-read (once per target): refs, implicits flag, TFMs,
    // declared package names, explicit Compile lists.
    let csproj_path = find_csproj(&target.root_source);
    let csproj_dir = csproj_path
        .as_deref()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| target.root_source.clone());
    let ctx = match csproj_path {
        Some(path) => match read_csproj(&path) {
            Ok(ctx) => ctx,
            Err(message) => {
                diags.push(Diagnostic {
                    severity: DiagnosticSeverity::Error,
                    code: "IO001".to_string(),
                    message,
                    source_span: None,
                });
                CsProjCtx::defaults(csproj_dir.clone())
            }
        },
        None => CsProjCtx::defaults(csproj_dir.clone()),
    };

    // The target's own TFMs: a `-<tfm>` name suffix selects one, otherwise
    // the union of all listed TFMs applies; with no list at all the SDK
    // default still applies when the flag is on.
    let own_tfms: Vec<&str> = match ctx.tfms.iter().find(|t| {
        target.name.starts_with(target.package_id.0.as_str())
            && target.name.as_bytes().get(target.package_id.0.len()) == Some(&b'-')
            && target.name[target.package_id.0.len() + 1..] == **t
    }) {
        Some(one) => vec![one.as_str()],
        None => ctx.tfms.iter().map(String::as_str).collect(),
    };
    let mut implicits: HashSet<Vec<String>> = HashSet::new();
    if ctx.implicit_on {
        if own_tfms.is_empty() {
            for dotted in IMPLICIT_USINGS_NET {
                implicits.insert(split_dotted(dotted));
            }
        } else {
            for tfm in &own_tfms {
                for dotted in implicit_usings_for_tfm(tfm) {
                    implicits.insert(split_dotted(dotted));
                }
            }
        }
    }

    // Candidate files: explicit Compile lists win exactly; otherwise the
    // recursive `*.cs` glob minus `obj/`+`bin/`.
    let mut files: Vec<PathBuf> = Vec::new();
    if target.root_source.is_dir() {
        match &ctx.compile {
            Some((includes, removes)) => {
                files = expand_compile(&csproj_dir, includes, removes, diags);
            }
            None => collect_cs(&target.root_source, &mut files),
        }
        files.sort();
    } else {
        let detail = match std::fs::symlink_metadata(&target.root_source) {
            Ok(_) => "not a regular file".to_string(),
            Err(e) => e.to_string(),
        };
        diags.push(io_diag(&target.root_source, &detail));
    }

    let mut entries: Vec<Entry> = vec![Entry {
        segments: Vec::new(),
        module: entry_module(target, &[], Some(target.root_source.clone()), None),
        file: None,
    }];
    let mut globals: Vec<(String, Vec<String>, SourceSpan)> = Vec::new();
    let mut file_usings: Vec<(PathBuf, Vec<RawUsing>)> = Vec::new();
    let mut has_cfg = false;

    // Phase 1: parse each file once; merge namespace blocks per target
    // (files already sorted, so first insert wins path and span).
    let mut merged: HashMap<Vec<String>, (PathBuf, SourceSpan)> = HashMap::new();
    for file in &files {
        let data = match extract_file(file) {
            Ok(data) => data,
            Err(message) => {
                diags.push(Diagnostic {
                    severity: DiagnosticSeverity::Error,
                    code: "IO001".to_string(),
                    message,
                    source_span: None,
                });
                continue;
            }
        };
        if data.has_cfg {
            has_cfg = true;
        }
        // Nesting: each block nests under the smallest containing block
        // (sequential blocks pop; a file-scoped block owns the rest of the
        // file, so later top-level blocks nest under it per C# scoping).
        // Sorted by start, ties longest-first so containers precede.
        let mut decls = data.decls;
        decls.sort_by(|a, b| {
            a.start_row
                .cmp(&b.start_row)
                .then_with(|| b.end_row.cmp(&a.end_row))
        });
        let mut stack: Vec<(Vec<String>, usize)> = Vec::new();
        for decl in &decls {
            while stack.last().is_some_and(|(_, end)| *end < decl.start_row) {
                stack.pop();
            }
            let mut full: Vec<String> = stack.last().map(|(s, _)| s.clone()).unwrap_or_default();
            full.extend(decl.segments.iter().cloned());
            // A file-scoped block owns the rest of the file, so later
            // top-level blocks nest under it per C# scoping.
            let end = if decl.file_scoped {
                usize::MAX
            } else {
                decl.end_row
            };
            stack.push((full.clone(), end));
            merged.entry(full).or_insert_with(|| {
                (
                    file.clone(),
                    SourceSpan {
                        file: file.clone(),
                        line_start: decl.start_row as u32 + 1,
                        line_end: decl.end_row as u32 + 1,
                    },
                )
            });
        }
        if decls.is_empty() {
            // Namespace-less file: one stem module.
            let segments = stem_segments(&target.root_source, file);
            if !segments.is_empty() {
                entries.push(Entry {
                    module: entry_module(target, &segments, Some(file.clone()), None),
                    segments,
                    file: Some(file.clone()),
                });
            }
        }
        let (globals_here, own): (Vec<RawUsing>, Vec<RawUsing>) =
            data.usings.into_iter().partition(|u| u.is_global);
        for raw in globals_here {
            let segments = split_dotted(&raw.path);
            if !segments.is_empty() {
                globals.push((raw.path, segments, raw.span));
            }
        }
        file_usings.push((file.clone(), own));
    }
    let mut names: Vec<Vec<String>> = merged.keys().cloned().collect();
    names.sort();
    for segments in names {
        if let Some((file, span)) = merged.remove(&segments) {
            entries.push(Entry {
                module: entry_module(target, &segments, Some(file.clone()), Some(span)),
                segments,
                file: Some(file),
            });
        }
    }
    TargetWalk {
        entries,
        globals,
        implicits,
        file_usings,
        has_cfg,
        ctx,
    }
}

/// Resolution outcome for one [`CsUse`]: an edge target, silence, or a
/// diagnostic with no edge.
enum Resolution {
    Edge(DependencyTarget),
    Unresolved(Diagnostic),
    Skip,
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

#[allow(clippy::too_many_arguments)]
fn resolve_use(
    raw: &CsUse,
    own_tree: &HashMap<Vec<String>, ModuleId>,
    other_trees: &[&HashMap<Vec<String>, ModuleId>],
    ref_tables: &HashMap<String, HashMap<Vec<String>, ModuleId>>,
    dep_names: &[(Vec<String>, String)],
    deps_by_name: &HashMap<String, Vec<&crate::metadata::ExternalDep>>,
    declared_names: &HashSet<Vec<String>>,
) -> Resolution {
    // Aliases are diagnostics-only and never mint IDs.
    let _ = raw.alias.as_deref();
    if raw.segments.is_empty() {
        return Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span));
    }
    // (a) Own target's declared namespaces, longest-prefix.
    let (hit, _) = longest_prefix(own_tree, &raw.segments);
    if let Some(id) = hit {
        return Resolution::Edge(DependencyTarget::Internal(id));
    }
    // (a2) Same-package fallback: the FULL unmodified segments against the
    // package's OTHER targets' trees (Lib and Bin alike — C# namespaces are
    // global-rooted, so no package prefix is ever stripped). Exactly one
    // matching target -> `Internal`; several -> `SCOPE001` (conclusive:
    // same-package ambiguity is never external); zero -> fall through.
    if !other_trees.is_empty() {
        let mut hits: Vec<ModuleId> = Vec::new();
        for tree in other_trees {
            if let (Some(id), _) = longest_prefix(tree, &raw.segments) {
                hits.push(id);
            }
        }
        hits.sort_by(|a, b| a.0.cmp(&b.0));
        hits.dedup();
        if hits.len() == 1 {
            return Resolution::Edge(DependencyTarget::Internal(hits.swap_remove(0)));
        }
        if hits.len() > 1 {
            return Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span));
        }
    }
    // (b) Directly-referenced projects' namespaces (one hop): longest-prefix
    // per referenced package; exactly one owning package -> `Internal`, 2+
    // -> `SCOPE001` (anti-goal: never fabricate cross-project edges).
    if !ref_tables.is_empty() {
        let mut owners: HashMap<&str, ModuleId> = HashMap::new();
        for (pkg, table) in ref_tables {
            if let (Some(id), _) = longest_prefix(table, &raw.segments) {
                owners.entry(pkg.as_str()).or_insert(id);
            }
        }
        if owners.len() == 1 {
            let id = owners.into_values().next().expect("len == 1 checked");
            return Resolution::Edge(DependencyTarget::Internal(id));
        }
        if owners.len() > 1 {
            return Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span));
        }
    }
    // (d) BCL skip BEFORE external lookup.
    if is_bcl(&raw.segments[0]) {
        return Resolution::Skip;
    }
    // (c) `PackageReference` names: longest package-name prefix anchored at
    // the first segment (case-insensitive; "longest-prefix throughout").
    // Exactly one pinned name -> `External`; several -> `SCOPE001`; a
    // declared-but-unpinned name -> `EXT001`. Anything else is not a
    // package at all.
    let lowered: Vec<String> = raw
        .segments
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let mut matched: Vec<&str> = Vec::new();
    for (segments, name) in dep_names {
        if segments.len() <= lowered.len()
            && *segments == lowered[..segments.len()]
            && !matched.contains(&name.as_str())
        {
            matched.push(name);
        }
    }
    if matched.len() > 1 {
        return Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span));
    }
    if let Some(name) = matched.into_iter().next() {
        match deps_by_name.get(name) {
            Some(versions) if versions.len() == 1 => {
                let dep = versions[0];
                return Resolution::Edge(DependencyTarget::External(make_external_id(
                    &dep.name,
                    &dep.version,
                )));
            }
            _ => return Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span)),
        }
    }
    let declared_hit = declared_names
        .iter()
        .any(|segments| segments.len() <= lowered.len() && segments == &lowered[..segments.len()]);
    if declared_hit {
        Resolution::Unresolved(ext_diag(&raw.from, &raw.path, &raw.span))
    } else {
        Resolution::Unresolved(scope_diag(&raw.from, &raw.path, &raw.span))
    }
}

/// The C# whole-workspace [`Analyzer`]: per-target walk → extract →
/// resolve → dedup → sort, calling the shared `graph`/`diag` helpers.
pub struct CsAnalyzer;

impl Analyzer for CsAnalyzer {
    fn analyze(&self, input: &AnalysisInput) -> Result<WorkspaceGraph, String> {
        // 1. Per-target walk (csproj re-read once per target inside).
        let mut modules_all: Vec<Module> = Vec::new();
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let mut segments_of: HashMap<ModuleId, Vec<String>> = HashMap::new();
        let mut trees: HashMap<TargetId, HashMap<Vec<String>, ModuleId>> = HashMap::new();
        let mut owner_to_target: HashMap<ModuleId, TargetId> = HashMap::new();
        let mut file_of: HashMap<ModuleId, PathBuf> = HashMap::new();
        let mut cfg_seen = false;
        let mut walks: HashMap<TargetId, TargetWalk> = HashMap::new();
        for target in &input.targets {
            let walk = walk_target(target, &mut diagnostics);
            if walk.has_cfg {
                cfg_seen = true;
            }
            let mut tree: HashMap<Vec<String>, ModuleId> = HashMap::new();
            for entry in &walk.entries {
                tree.entry(entry.segments.clone())
                    .or_insert_with(|| entry.module.id.clone());
                segments_of
                    .entry(entry.module.id.clone())
                    .or_insert_with(|| entry.segments.clone());
                if let Some(file) = entry.file.clone() {
                    file_of.entry(entry.module.id.clone()).or_insert(file);
                }
                owner_to_target.insert(entry.module.id.clone(), target.id.clone());
                modules_all.push(entry.module.clone());
            }
            trees.insert(target.id.clone(), tree);
            walks.insert(target.id.clone(), walk);
        }
        // Workspace-global views: dep names (lowered segments + lowered full
        // name) and declared package names (union of every re-read csproj,
        // lowered segments).
        let deps_by_name: HashMap<String, Vec<&crate::metadata::ExternalDep>> = {
            let mut map: HashMap<String, Vec<&crate::metadata::ExternalDep>> = HashMap::new();
            for dep in &input.deps {
                map.entry(dep.name.to_ascii_lowercase())
                    .or_default()
                    .push(dep);
            }
            map
        };
        let dep_names: Vec<(Vec<String>, String)> = {
            let mut names: Vec<(Vec<String>, String)> = deps_by_name
                .keys()
                .map(|n| (split_dotted(n), n.clone()))
                .collect();
            names.sort();
            names.dedup();
            names
        };
        let declared_names: HashSet<Vec<String>> = walks
            .values()
            .flat_map(|w| {
                w.ctx
                    .package_names
                    .iter()
                    .map(|n| split_dotted(&n.to_ascii_lowercase()))
            })
            .collect();

        // Per-target reference tables (built once): each directly-referenced
        // project dir maps to its packages' namespace tables (candidate
        // targets sorted for determinism; refs outside the analyzed targets
        // contribute nothing). Plus the same-package OTHER targets for the
        // (a2) full-path fallback.
        let root_keys: HashMap<&TargetId, PathBuf> = input
            .targets
            .iter()
            .map(|t| (&t.id, best_key(&t.root_source)))
            .collect();
        let mut ref_of: HashMap<&TargetId, HashMap<String, HashMap<Vec<String>, ModuleId>>> =
            HashMap::new();
        let mut sib_of: HashMap<&TargetId, Vec<&TargetId>> = HashMap::new();
        for target in &input.targets {
            let mut ref_tables: HashMap<String, HashMap<Vec<String>, ModuleId>> = HashMap::new();
            if let Some(walk) = walks.get(&target.id) {
                for ref_path in &walk.ctx.project_refs {
                    let key = ref_path
                        .parent()
                        .map(best_key)
                        .unwrap_or_else(|| best_key(ref_path));
                    let mut candidates: Vec<&Target> = input
                        .targets
                        .iter()
                        .filter(|t| root_keys.get(&t.id).is_some_and(|k| *k == key))
                        .collect();
                    candidates.sort_by(|a, b| a.id.cmp(&b.id));
                    for candidate in candidates {
                        let Some(table) = trees.get(&candidate.id) else {
                            continue;
                        };
                        let entry = ref_tables
                            .entry(candidate.package_id.0.clone())
                            .or_default();
                        // Sorted so multi-target packages merge deterministically.
                        let mut segments: Vec<_> = table.iter().collect();
                        segments.sort_by(|a, b| a.0.cmp(b.0));
                        for (segments, id) in segments {
                            entry.entry(segments.clone()).or_insert_with(|| id.clone());
                        }
                    }
                }
            }
            ref_of.insert(&target.id, ref_tables);
            // Same-package OTHER targets (any kind) for the (a2) fallback.
            sib_of.insert(
                &target.id,
                input
                    .targets
                    .iter()
                    .filter(|t| t.package_id == target.package_id && t.id != target.id)
                    .map(|t| &t.id)
                    .collect(),
            );
        }

        // 2. Extraction: one `CsUse` per context entry of every file-backed
        // module — its file's explicit usings, the target globals, and the
        // target implicits (all BCL-silent in practice).
        let mut uses: Vec<CsUse> = Vec::new();
        for target in &input.targets {
            let Some(walk) = walks.get(&target.id) else {
                continue;
            };
            let usings_by_file: HashMap<&PathBuf, &Vec<RawUsing>> =
                walk.file_usings.iter().map(|(f, u)| (f, u)).collect();
            let mut push = |from: &ModuleId,
                            path: String,
                            segments: Vec<String>,
                            alias: Option<String>,
                            explicit: bool,
                            span: Option<SourceSpan>| {
                if !segments.is_empty() {
                    uses.push(CsUse {
                        from: from.clone(),
                        path,
                        segments,
                        alias,
                        explicit,
                        span,
                    });
                }
            };
            for module in &modules_all {
                if owner_to_target.get(&module.id) != Some(&target.id) {
                    continue;
                }
                let Some(file) = file_of.get(&module.id) else {
                    continue;
                };
                if let Some(own) = usings_by_file.get(file) {
                    for raw in own.iter() {
                        push(
                            &module.id,
                            raw.path.clone(),
                            split_dotted(&raw.path),
                            raw.alias.clone(),
                            true,
                            Some(raw.span.clone()),
                        );
                    }
                }
                for (path, segments, span) in &walk.globals {
                    push(
                        &module.id,
                        path.clone(),
                        segments.clone(),
                        None,
                        false,
                        Some(span.clone()),
                    );
                }
                let mut implicits: Vec<&Vec<String>> = walk.implicits.iter().collect();
                implicits.sort();
                for segments in implicits {
                    push(
                        &module.id,
                        segments.join("."),
                        (*segments).clone(),
                        None,
                        false,
                        None,
                    );
                }
            }
        }

        // 3. `mod` containment edges (parent → child, kind Module): the
        // longest strict dotted prefix naming a declared module of the same
        // target, else the target root. Stem modules parent at the root.
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
            let Some(owner) = input.targets.iter().find(|t| &t.id == target_id) else {
                continue;
            };
            let mut parent: Option<ModuleId> = None;
            for len in (1..segments.len()).rev() {
                if let Some(id) = tree.get(&segments[..len]) {
                    parent = Some(id.clone());
                    break;
                }
            }
            dependencies.push(Dependency {
                from: parent.unwrap_or_else(|| root_id(owner)),
                to: DependencyTarget::Internal(module.id.clone()),
                kind: DependencyKind::Module,
                source_span: None,
            });
        }

        // 4. Resolve every explicit `CsUse` against its owning target's tree,
        // the package's sibling-target trees, and the precomputed reference
        // tables. Ambient (global/implicit) entries never resolve: they stay
        // silent, producing neither edges nor diagnostics.
        for raw in &uses {
            if !raw.explicit {
                continue;
            }
            let Some(target_id) = owner_to_target.get(&raw.from) else {
                diagnostics.push(scope_diag(&raw.from, &raw.path, &raw.span));
                continue;
            };
            let Some(own_tree) = trees.get(target_id) else {
                diagnostics.push(scope_diag(&raw.from, &raw.path, &raw.span));
                continue;
            };
            let other_trees: Vec<&HashMap<Vec<String>, ModuleId>> = sib_of
                .get(target_id)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|id| trees.get(id))
                .collect();
            let empty_refs: HashMap<String, HashMap<Vec<String>, ModuleId>> = HashMap::new();
            let ref_tables = ref_of.get(target_id).unwrap_or(&empty_refs);
            match resolve_use(
                raw,
                own_tree,
                &other_trees,
                ref_tables,
                &dep_names,
                &deps_by_name,
                &declared_names,
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

        // 5. Exactly one run-level CFG001 warning when any `#if` was seen
        // (no evaluation; the graph reflects unexpanded source).
        if cfg_seen {
            diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "CFG001".to_string(),
                message: "conditional compilation detected - graph reflects unexpanded source"
                    .to_string(),
                source_span: None,
            });
        }

        // 6. Packages are synthesized from the input targets (identity only;
        // the per-package csproj stands in for manifest paths, restored to
        // the real ones by `run()`).
        let mut package_ids: Vec<_> = input.targets.iter().map(|t| t.package_id.clone()).collect();
        package_ids.sort();
        package_ids.dedup();
        let packages: Vec<Package> = package_ids
            .into_iter()
            .map(|id| Package {
                name: id.0.clone(),
                manifest_path: input.workspace_root.join(format!("{}.csproj", id.0)),
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
        sort_diagnostics(&mut graph.diagnostics);
        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_list_sorted_and_spotted() {
        assert!(
            !IMPLICIT_USINGS_NET.is_empty(),
            "implicit list must not be empty"
        );
        let mut sorted = IMPLICIT_USINGS_NET.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            sorted, IMPLICIT_USINGS_NET,
            "IMPLICIT_USINGS_NET must stay sorted"
        );
        assert_eq!(
            IMPLICIT_USINGS_NET.len(),
            IMPLICIT_USINGS_NET.iter().collect::<HashSet<_>>().len(),
            "IMPLICIT_USINGS_NET must not contain duplicates"
        );
        for member in ["System", "System.Collections.Generic", "System.Linq"] {
            assert!(
                IMPLICIT_USINGS_NET.contains(&member),
                "{member} must be implicit"
            );
        }
        assert!(
            !IMPLICIT_USINGS_NET.contains(&"System.Text.Json"),
            "System.Text.Json is not an SDK default implicit"
        );
        // Per-TFM variance straight from the SDK conditions.
        assert_eq!(implicit_usings_for_tfm("net8.0").len(), 7);
        assert_eq!(implicit_usings_for_tfm("net9.0").len(), 7);
        assert!(
            !implicit_usings_for_tfm("net48").contains(&"System.Net.Http"),
            "netfx drops System.Net.Http per the SDK condition"
        );
        assert!(
            implicit_usings_for_tfm("net11.0").contains(&"System.Net.Http.Json"),
            "net11+ gains System.Net.Http.Json per the SDK condition"
        );
        assert!(is_bcl("System"), "System must be BCL-skipped");
        assert!(!is_bcl("Newtonsoft"), "Newtonsoft must not be BCL");
        let mut prefixes = BCL_PREFIXES.to_vec();
        prefixes.sort_unstable();
        assert_eq!(prefixes, BCL_PREFIXES, "BCL_PREFIXES must stay sorted");
    }
}
