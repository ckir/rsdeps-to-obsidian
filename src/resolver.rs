//! Syntactic module discovery and `use` extraction (Task 5, part 1).
//!
//! * [`parse_source_file`] reads and parses one Rust source file.
//! * [`discover_modules_for_target`] walks `mod` items from a target root,
//!   resolving file modules (`foo.rs` / `foo/mod.rs`, `#[path]`, inline).
//! * [`extract_uses`] expands `use` trees into [`RawUse`] entries.
//!
//! # Module identity
//!
//! [`ModuleId`]s follow Task 2's [`make_module_id`] shape: non-root modules
//! are `make_module_id(package_name, target.name, logical_path)` (e.g.
//! `api::api::handler`); the root module is exactly two segments,
//! `{package_name}::{target.name}` (e.g. `api::api`), with no trailing
//! separator. The package name comes from the target's `package_id`, which
//! `metadata::discover` sets to the Cargo package name. `qualified_name` is
//! the Rust logical path (`crate`, `crate::foo`).
//!
//! # `extract_uses` contract (for Task 6)
//!
//! Call it **once per file-backed module** with that module's parsed AST. It
//! also descends into inline `mod x { ... }` blocks and attributes their
//! `use` items to the descendant `ModuleId` (`owner-id::x::...`), so inline
//! modules (which own no file) must NOT be passed separately — their imports
//! are already covered by the owning file's walk. Glob imports (`use foo::*`)
//! yield no `RawUse`; ownership reporting for them belongs to Task 6
//! resolution. Only module-scope `use` items are collected; `use` items
//! inside function bodies are ignored. Only a bare `pub use` sets `is_pub`
//! (`pub(crate)` and friends count as private). `#[cfg]` attributes are
//! passed through untouched here; Task 6 emits the single run-level `CFG001`.

use crate::domain::{
    Dependency, DependencyKind, DependencyTarget, Diagnostic, DiagnosticSeverity, Module, Package,
    SourceSpan, Target, WorkspaceGraph,
};
use crate::graph::{dedup_dependencies, sort_graph};
use crate::ids::{make_external_id, make_module_id, ModuleId};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use syn::spanned::Spanned;

/// Input for whole-workspace analysis (Task 6 consumes `deps`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisInput {
    pub workspace_root: PathBuf,
    pub targets: Vec<Target>,
    pub deps: Vec<crate::metadata::ExternalDep>,
}

/// One syntactic `use` import before ownership resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawUse {
    pub from: ModuleId,
    pub path: String,
    pub is_pub: bool,
    pub alias: Option<String>,
    pub span: Option<SourceSpan>,
}

/// Read `path` and parse it as a Rust source file.
pub fn parse_source_file(path: &Path) -> Result<syn::File, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    syn::parse_file(&text).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

/// Discover every module of `target` by walking `mod` items from its root.
///
/// Returns `(modules, diagnostics)` in pre-order discovery order (root
/// first). Diagnostics: `MOD001` (unresolved / circular, Warning),
/// `MOD002` (ambiguous `foo.rs` vs `foo/mod.rs`, Warning, file wins),
/// `PATH001` (`#[path]` escapes the workspace root, Warning, skipped),
/// `IO001` (unreadable / unparseable file, Error, discovery continues).
pub fn discover_modules_for_target(
    target: &Target,
    workspace_root: &Path,
) -> (Vec<Module>, Vec<Diagnostic>) {
    let mut walker = Walker {
        target,
        target_root: target.root_source.clone(),
        workspace_root: best_canonical(workspace_root),
        modules: Vec::new(),
        diagnostics: Vec::new(),
        parsed: HashMap::new(),
        stack: Vec::new(),
    };
    let root_id = ModuleId(format!("{}::{}", target.package_id.0, target.name));
    let root_display = target.root_source.clone();
    let root_canon = best_canonical(&target.root_source);
    match parse_source_file(&target.root_source) {
        Ok(file) => {
            walker.push_module(
                root_id,
                target.name.clone(),
                "crate".to_string(),
                Some(root_canon.clone()),
                None,
            );
            walker.stack.push(root_canon.clone());
            walker.parsed.insert(root_canon.clone(), file);
            let items = walker.parsed_items(&root_canon);
            walker.walk_items(&items, &Vec::new(), &root_display);
            walker.stack.pop();
        }
        Err(message) => {
            walker.push_module(
                root_id,
                target.name.clone(),
                "crate".to_string(),
                Some(root_canon),
                None,
            );
            walker.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Error,
                code: "IO001".to_string(),
                message,
                source_span: None,
            });
        }
    }
    (walker.modules, walker.diagnostics)
}

/// Expand every module-scope `use` item in `file` into [`RawUse`] entries.
///
/// See the module-level contract: call once per file-backed module; inline
/// submodules are descended into and attributed to descendant `ModuleId`s.
pub fn extract_uses(module: &Module, file: &syn::File) -> Vec<RawUse> {
    let mut out = Vec::new();
    collect_uses(
        &file.items,
        &module.id,
        &Vec::new(),
        &module.source_path,
        &mut out,
    );
    out
}

// --- internals ---------------------------------------------------------------

struct Walker<'a> {
    target: &'a Target,
    target_root: PathBuf,
    workspace_root: PathBuf,
    modules: Vec<Module>,
    diagnostics: Vec<Diagnostic>,
    /// Canonical path -> parsed AST (each file parsed at most once).
    parsed: HashMap<PathBuf, syn::File>,
    /// Canonical ancestry of the current DFS path (cycle guard).
    stack: Vec<PathBuf>,
}

fn best_canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
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

fn parent_of(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Directory in which plain (non-`#[path]`) children of `containing` are
/// resolved, per rustc 2018 conventions: `mod.rs` files and the crate root
/// (target `root_source`) resolve children in their own parent directory;
/// any other file module `dir/foo.rs` resolves children in `dir/foo/`.
fn anchor_dir(containing: &Path, target_root: &Path) -> PathBuf {
    if containing == target_root || containing.file_name().is_some_and(|name| name == "mod.rs") {
        return parent_of(containing);
    }
    match containing.file_stem() {
        Some(stem) => parent_of(containing).join(stem),
        None => parent_of(containing),
    }
}

fn item_span(file: &Path, span: impl Spanned) -> SourceSpan {
    let span = span.span();
    SourceSpan {
        file: file.to_path_buf(),
        line_start: span.start().line as u32,
        line_end: span.end().line as u32,
    }
}

/// `#[path = "..."]` value on a `mod` item, if present and a string literal.
fn path_attr(item: &syn::ItemMod) -> Option<String> {
    for attr in &item.attrs {
        if !attr.path().is_ident("path") {
            continue;
        }
        if let syn::Meta::NameValue(name_value) = &attr.meta {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit),
                ..
            }) = &name_value.value
            {
                return Some(lit.value());
            }
        }
    }
    None
}

impl Walker<'_> {
    fn push_module(
        &mut self,
        id: ModuleId,
        name: String,
        qualified_name: String,
        source_path: Option<PathBuf>,
        source_span: Option<SourceSpan>,
    ) {
        self.modules.push(Module {
            id,
            target_id: self.target.id.clone(),
            name,
            qualified_name,
            source_path,
            source_span,
        });
    }

    fn child_id(&self, logical: &[String]) -> ModuleId {
        make_module_id(&self.target.package_id.0, &self.target.name, logical)
    }

    fn qualified(&self, logical: &[String]) -> String {
        if logical.is_empty() {
            "crate".to_string()
        } else {
            format!("crate::{}", logical.join("::"))
        }
    }

    /// Clone the cached AST items for `canon` (releases the borrow so the
    /// recursive walk can mutate the cache).
    fn parsed_items(&self, canon: &Path) -> Vec<syn::Item> {
        self.parsed
            .get(canon)
            .map(|f| f.items.clone())
            .unwrap_or_default()
    }

    fn walk_items(&mut self, items: &[syn::Item], logical: &[String], containing: &Path) {
        // Clone the slice's mods up front: recursion mutates self.
        let mods: Vec<syn::ItemMod> = items
            .iter()
            .filter_map(|item| match item {
                syn::Item::Mod(m) => Some(m.clone()),
                _ => None,
            })
            .collect();
        for m in &mods {
            self.walk_mod(m, logical, containing);
        }
    }

    fn walk_mod(&mut self, item: &syn::ItemMod, logical: &[String], containing: &Path) {
        let name = item.ident.to_string();
        let mut child_logical = logical.to_vec();
        child_logical.push(name.clone());
        let span = item_span(containing, item.span());

        if let Some((_, inner)) = &item.content {
            // Inline module: no own file; span points at the containing file.
            let inner = inner.clone();
            self.push_module(
                self.child_id(&child_logical),
                name,
                self.qualified(&child_logical),
                None,
                Some(span),
            );
            self.walk_items(&inner, &child_logical, containing);
            return;
        }

        if let Some(attr_path) = path_attr(item) {
            self.walk_path_attr(&name, &child_logical, &attr_path, &span, containing);
            return;
        }

        let dir = anchor_dir(containing, &self.target_root);
        let cand_file = dir.join(format!("{name}.rs"));
        let cand_mod = dir.join(&name).join("mod.rs");
        let file_exists = cand_file.is_file();
        let mod_exists = cand_mod.is_file();
        if !file_exists && !mod_exists {
            self.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "MOD001".to_string(),
                message: format!(
                    "unresolved module `{name}`: neither {} nor {} exist",
                    cand_file.display(),
                    cand_mod.display()
                ),
                source_span: Some(span),
            });
            return;
        }
        let mut chosen = cand_file;
        if file_exists && mod_exists {
            let other = cand_mod;
            let message = format!(
                "ambiguous module layout: both {} and {} exist; using {}",
                chosen.display(),
                other.display(),
                chosen.display()
            );
            self.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "MOD002".to_string(),
                message,
                source_span: Some(span.clone()),
            });
        } else if mod_exists {
            chosen = cand_mod;
        }
        self.enter_file(&name, &child_logical, &chosen, &span);
    }

    fn walk_path_attr(
        &mut self,
        name: &str,
        child_logical: &[String],
        attr_path: &str,
        span: &SourceSpan,
        containing: &Path,
    ) {
        let joined = parent_of(containing).join(attr_path);
        // Lexical containment first (works even when the target is missing).
        let normalized = lex_normalize(&joined);
        if !normalized.starts_with(&self.workspace_root) {
            self.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "PATH001".to_string(),
                message: format!(
                    "path attribute escapes workspace root: `{attr_path}` points outside {}",
                    self.workspace_root.display()
                ),
                source_span: Some(span.clone()),
            });
            return;
        }
        let canon = match std::fs::canonicalize(&normalized) {
            Ok(canon) => {
                // A symlink could still smuggle the file outside the root.
                if !canon.starts_with(&self.workspace_root) {
                    self.diagnostics.push(Diagnostic {
                        severity: DiagnosticSeverity::Warning,
                        code: "PATH001".to_string(),
                        message: format!(
                            "path attribute escapes workspace root: `{attr_path}` resolves outside {}",
                            self.workspace_root.display()
                        ),
                        source_span: Some(span.clone()),
                    });
                    return;
                }
                canon
            }
            Err(_) => {
                self.diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    code: "MOD001".to_string(),
                    message: format!(
                        "unresolved module `{name}`: path attribute `{attr_path}` does not resolve to a file"
                    ),
                    source_span: Some(span.clone()),
                });
                return;
            }
        };
        self.enter_file(name, child_logical, &canon, span);
    }

    /// Register the file-backed module for `child_logical` at `path` and
    /// recurse into it unless it is already on the DFS stack (cycle guard).
    fn enter_file(&mut self, name: &str, child_logical: &[String], path: &Path, span: &SourceSpan) {
        let canon = best_canonical(path);
        if self.stack.contains(&canon) {
            self.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "MOD001".to_string(),
                message: format!(
                    "circular module include: {} includes itself",
                    canon.display()
                ),
                source_span: Some(span.clone()),
            });
            return;
        }
        let id = self.child_id(child_logical);
        let qualified = self.qualified(child_logical);
        match parse_source_file(path) {
            Ok(file) => {
                self.push_module(id, name.to_string(), qualified, Some(canon.clone()), None);
                self.parsed.insert(canon.clone(), file);
                self.stack.push(canon.clone());
                let items = self.parsed_items(&canon);
                // Grandchild spans/files use the canonical path so they stay
                // consistent with the stored `source_path`.
                self.walk_items(&items, child_logical, &canon);
                self.stack.pop();
            }
            Err(message) => {
                self.push_module(id, name.to_string(), qualified, Some(canon), None);
                self.diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Error,
                    code: "IO001".to_string(),
                    message,
                    source_span: Some(span.clone()),
                });
            }
        }
    }
}

fn collect_uses(
    items: &[syn::Item],
    owner: &ModuleId,
    nested: &[String],
    span_file: &Option<PathBuf>,
    out: &mut Vec<RawUse>,
) {
    for item in items {
        match item {
            syn::Item::Use(use_item) => {
                let from = if nested.is_empty() {
                    owner.clone()
                } else {
                    ModuleId(format!("{}::{}", owner.0, nested.join("::")))
                };
                let is_pub = matches!(use_item.vis, syn::Visibility::Public(_));
                let span = span_file
                    .as_ref()
                    .map(|file| item_span(file, use_item.span()));
                let mut prefix = Vec::new();
                expand_tree(
                    &use_item.tree,
                    &mut prefix,
                    use_item.leading_colon.is_some(),
                    &from,
                    is_pub,
                    &span,
                    out,
                );
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    let mut deeper = nested.to_vec();
                    deeper.push(m.ident.to_string());
                    collect_uses(inner, owner, &deeper, span_file, out);
                }
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_tree(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    absolute: bool,
    from: &ModuleId,
    is_pub: bool,
    span: &Option<SourceSpan>,
    out: &mut Vec<RawUse>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            expand_tree(&path.tree, prefix, absolute, from, is_pub, span, out);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            let mut segments = prefix.clone();
            // `use foo::{self}` refers to `foo` itself.
            if name.ident != "self" || segments.is_empty() {
                segments.push(name.ident.to_string());
            }
            push_use(segments, absolute, None, from, is_pub, span, out);
        }
        syn::UseTree::Rename(rename) => {
            let mut segments = prefix.clone();
            if rename.ident != "self" || segments.is_empty() {
                segments.push(rename.ident.to_string());
            }
            push_use(
                segments,
                absolute,
                Some(rename.rename.to_string()),
                from,
                is_pub,
                span,
                out,
            );
        }
        // Glob imports carry no single ownership path; resolution (Task 6)
        // reports them. No edge here.
        syn::UseTree::Glob(_) => {}
        syn::UseTree::Group(group) => {
            for subtree in &group.items {
                expand_tree(subtree, prefix, absolute, from, is_pub, span, out);
            }
        }
    }
}

fn push_use(
    segments: Vec<String>,
    absolute: bool,
    alias: Option<String>,
    from: &ModuleId,
    is_pub: bool,
    span: &Option<SourceSpan>,
    out: &mut Vec<RawUse>,
) {
    if segments.is_empty() {
        return;
    }
    let mut path = segments.join("::");
    if absolute {
        path = format!("::{path}");
    }
    out.push(RawUse {
        from: from.clone(),
        path,
        is_pub,
        alias,
        span: span.clone(),
    });
}

// --- Task 6: ownership resolution + graph build --------------------------------
//
// `SynAnalyzer` runs the full pipeline: per-target module discovery, one
// `extract_uses` call per file-backed module (inline submodules are covered by
// their owner's walk, so they are never passed separately; one call per owner
// when a file backs several logical modules), ownership resolution of every
// `RawUse` against the owning target's module tree and `AnalysisInput.deps`,
// then dedup + total-order sort.
//
// Resolution rules (spec §§11.7/33, plus Post-MVP amendment A):
// * `crate`/`self`/`super`-rooted paths resolve against the owning target's
//   known module tree with LONGEST-PREFIX attribution: the full logical path
//   preferred, else the longest prefix naming a known module (a single-segment
//   remainder counts). The edge target is always a real discovered module,
//   never invented. No prefix match at all → `SCOPE001`, no edge. The target
//   owning `from` is found through the `Module.target_id` field plus the
//   `{package}::{target-name}` prefix — never by splitting the `ModuleId` to
//   guess the target boundary.
// * Bare (`foo::...`) paths first consult the workspace member packages: a
//   first segment equal to a member package NAME resolves the REMAINDER with
//   longest-prefix logic across EACH of that member's targets' trees (exactly
//   one target matches → Internal edge; zero or several → `SCOPE001`, no
//   edge). Otherwise the first segment resolves against `deps` (real Cargo
//   package name, never a rename alias; the `RawUse.alias` from `as` never
//   mints an ID) exactly as before, including the internal+external
//   `SCOPE001` ambiguity. Member-match AND external-match → `SCOPE001`, no
//   edge. `crate`-rooted paths never consult member/external tables;
//   `self`/`super` never leave the owning tree.
// * Anything else unresolvable (including globs, which `extract_uses` drops,
//   re-detected here by re-walking the parsed file) yields `SCOPE001`
//   `scope collision: <src> -> <path>; unresolved syntactic ownership` and no
//   edge. Missing dep name/version can never invent an ID.
// * `pub use` resolves to `ReExport`, plain `use` to `Use`; `mod` containment
//   contributes `Module` edges (parent → child), which dedup may collapse.
// * Any `#[cfg]` on an encountered `mod`/`use` (attrs re-walked here) yields
//   exactly one run-level `CFG001` warning; nothing is evaluated or skipped.

/// Whole-workspace syntactic analysis backend.
pub trait Analyzer {
    fn analyze(&self, input: &AnalysisInput) -> Result<WorkspaceGraph, String>;
}

/// The only MVP [`Analyzer`]: syn-based, no macro expansion, no cfg
/// evaluation, no type graph.
pub struct SynAnalyzer;

/// Per-target resolution context: the root-ID prefix plus the target's known
/// module tree.
struct TargetCtx {
    /// `{package}::{target-name}`: the root `ModuleId` and prefix of every
    /// module ID in this target.
    prefix: String,
    ids: HashSet<ModuleId>,
}

impl TargetCtx {
    fn of(target: &Target, modules: &[Module]) -> Self {
        let prefix = format!("{}::{}", target.package_id.0, target.name);
        let ids = modules.iter().map(|m| m.id.clone()).collect();
        Self { prefix, ids }
    }
}

/// The logical path of `from` with the `{package}::{target-name}` prefix
/// removed (`None` when `from` is foreign to this target).
fn logical_of(from: &ModuleId, prefix: &str) -> Option<Vec<String>> {
    if from.0 == prefix {
        return Some(Vec::new());
    }
    let rest = from.0.strip_prefix(&format!("{prefix}::"))?;
    Some(rest.split("::").map(str::to_string).collect())
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

/// Resolution outcome for one [`RawUse`]: an edge target, or a diagnostic
/// with no edge.
enum Resolution {
    Edge(DependencyTarget),
    Unresolved(Diagnostic),
}

/// Longest-prefix module attribution (amendment A1): the full `logical` path
/// preferred, else the longest strict prefix naming a known module of `ctx`.
/// A single-segment remainder counts; an empty `logical` names the target
/// root itself (as before). Returns `None` when not even the first segment
/// matches — the caller then reports `SCOPE001` with no edge.
fn longest_prefix_internal(ctx: &TargetCtx, logical: &[String]) -> Option<ModuleId> {
    if logical.is_empty() {
        let id = ModuleId(ctx.prefix.clone());
        return ctx.ids.contains(&id).then_some(id);
    }
    for len in (1..=logical.len()).rev() {
        let candidate = ModuleId(format!("{}::{}", ctx.prefix, logical[..len].join("::")));
        if ctx.ids.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Resolve one [`RawUse`] against its owning target's tree, the workspace
/// member targets (name → per-target contexts, for the bare member arm), and
/// the workspace external deps (name → dep entries; several entries share a
/// name when two resolved versions coexist).
fn resolve_use(
    raw: &RawUse,
    ctx: &TargetCtx,
    member_ctxs: &HashMap<&str, Vec<&TargetCtx>>,
    deps_by_name: &HashMap<&str, Vec<&crate::metadata::ExternalDep>>,
) -> Resolution {
    let display = raw.path.clone();
    let stripped = raw.path.strip_prefix("::").unwrap_or(&raw.path);
    let segments: Vec<&str> = stripped.split("::").collect();
    if segments.is_empty() || segments.iter().all(|s| s.is_empty()) {
        return Resolution::Unresolved(scope_diag(&raw.from, &display, &raw.span));
    }
    let unresolved = |suffix: Option<String>| {
        let message = match suffix {
            Some(extra) => format!("scope collision: {} -> {}; {extra}", raw.from.0, display),
            None => format!(
                "scope collision: {} -> {}; unresolved syntactic ownership",
                raw.from.0, display
            ),
        };
        Resolution::Unresolved(Diagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "SCOPE001".to_string(),
            message,
            source_span: raw.span.clone(),
        })
    };
    let internal = |logical: &[String]| {
        let candidate = if logical.is_empty() {
            ctx.prefix.clone()
        } else {
            format!("{}::{}", ctx.prefix, logical.join("::"))
        };
        let id = ModuleId(candidate);
        if ctx.ids.contains(&id) {
            Some(id)
        } else {
            None
        }
    };
    match segments[0] {
        "crate" => {
            let logical: Vec<String> = segments[1..].iter().map(|s| s.to_string()).collect();
            match longest_prefix_internal(ctx, &logical) {
                Some(id) => Resolution::Edge(DependencyTarget::Internal(id)),
                None => unresolved(None),
            }
        }
        "self" => {
            let mut logical = match logical_of(&raw.from, &ctx.prefix) {
                Some(logical) => logical,
                None => return unresolved(None),
            };
            logical.extend(segments[1..].iter().map(|s| s.to_string()));
            match longest_prefix_internal(ctx, &logical) {
                Some(id) => Resolution::Edge(DependencyTarget::Internal(id)),
                None => unresolved(None),
            }
        }
        "super" => {
            let ups = segments.iter().take_while(|s| **s == "super").count();
            let mut logical = match logical_of(&raw.from, &ctx.prefix) {
                Some(logical) => logical,
                None => return unresolved(None),
            };
            for _ in 0..ups {
                if logical.pop().is_none() {
                    return unresolved(None);
                }
            }
            logical.extend(segments[ups..].iter().map(|s| s.to_string()));
            match longest_prefix_internal(ctx, &logical) {
                Some(id) => Resolution::Edge(DependencyTarget::Internal(id)),
                None => unresolved(None),
            }
        }
        first => {
            let member_hit = member_ctxs.contains_key(first);
            match deps_by_name.get(first) {
                // Member-match AND external-match: inconclusive, no edge.
                Some(_) if member_hit => unresolved(None),
                Some(versions) if versions.len() == 1 && !member_hit => {
                    let logical: Vec<String> = segments.iter().map(|s| s.to_string()).collect();
                    let internal_match = internal(&logical);
                    let dep = versions[0];
                    let external = make_external_id(&dep.name, &dep.version);
                    match internal_match {
                        // Inconclusive tree position: both match, no edge.
                        Some(_) => unresolved(None),
                        None => Resolution::Edge(DependencyTarget::External(external)),
                    }
                }
                Some(_) => {
                    // Several resolved versions share this name: ownership is
                    // ambiguous, so the single mandated SCOPE001 text applies
                    // (no edge), exactly as for any other unresolvable path.
                    unresolved(None)
                }
                None if member_hit => {
                    // Bare member arm (amendment A2/A3): the remainder (after
                    // the member package name) resolves with longest-prefix
                    // logic across EACH of the member's targets' trees.
                    // Exactly one target yields a match → Internal edge to
                    // it; zero or several → SCOPE001, no edge.
                    let remainder: Vec<String> =
                        segments[1..].iter().map(|s| s.to_string()).collect();
                    let mut hits: Vec<ModuleId> = Vec::new();
                    if let Some(ctxs) = member_ctxs.get(first) {
                        for member in ctxs {
                            if let Some(id) = longest_prefix_internal(member, &remainder) {
                                hits.push(id);
                            }
                        }
                    }
                    if hits.len() == 1 {
                        Resolution::Edge(DependencyTarget::Internal(hits.swap_remove(0)))
                    } else {
                        unresolved(None)
                    }
                }
                None => {
                    let logical: Vec<String> = segments.iter().map(|s| s.to_string()).collect();
                    match internal(&logical) {
                        Some(id) => Resolution::Edge(DependencyTarget::Internal(id)),
                        None => unresolved(None),
                    }
                }
            }
        }
    }
}

/// Re-walk a parsed file for glob imports (`use foo::*`), which
/// [`extract_uses`] drops. Each glob occurrence yields one `SCOPE001`
/// diagnostic with no edge, attributed like `extract_uses` (inline
/// submodules map to descendant IDs).
fn collect_glob_diags(
    items: &[syn::Item],
    owner: &ModuleId,
    nested: &[String],
    span_file: &Option<PathBuf>,
    out: &mut Vec<Diagnostic>,
) {
    for item in items {
        match item {
            syn::Item::Use(use_item) => {
                let from = if nested.is_empty() {
                    owner.clone()
                } else {
                    ModuleId(format!("{}::{}", owner.0, nested.join("::")))
                };
                let span = span_file
                    .as_ref()
                    .map(|file| item_span(file, use_item.span()));
                let mut prefix = Vec::new();
                glob_tree(
                    &use_item.tree,
                    &mut prefix,
                    use_item.leading_colon.is_some(),
                    &from,
                    &span,
                    out,
                );
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    let mut deeper = nested.to_vec();
                    deeper.push(m.ident.to_string());
                    collect_glob_diags(inner, owner, &deeper, span_file, out);
                }
            }
            _ => {}
        }
    }
}

fn glob_tree(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    absolute: bool,
    from: &ModuleId,
    span: &Option<SourceSpan>,
    out: &mut Vec<Diagnostic>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            glob_tree(&path.tree, prefix, absolute, from, span, out);
            prefix.pop();
        }
        syn::UseTree::Glob(_) => {
            let mut path = prefix.join("::");
            if absolute {
                path = format!("::{path}");
            }
            let display = if path.is_empty() {
                "*".to_string()
            } else {
                format!("{path}::*")
            };
            out.push(scope_diag(from, &display, span));
        }
        syn::UseTree::Group(group) => {
            for subtree in &group.items {
                glob_tree(subtree, prefix, absolute, from, span, out);
            }
        }
        syn::UseTree::Name(_) | syn::UseTree::Rename(_) => {}
    }
}

fn has_cfg_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("cfg"))
}

/// Whether any encountered `mod`/`use` item (including inside inline modules)
/// carries `#[cfg]`.
fn file_has_cfg(items: &[syn::Item]) -> bool {
    items.iter().any(|item| match item {
        syn::Item::Use(use_item) => has_cfg_attr(&use_item.attrs),
        syn::Item::Mod(m) => {
            has_cfg_attr(&m.attrs)
                || m.content
                    .as_ref()
                    .is_some_and(|(_, inner)| file_has_cfg(inner))
        }
        _ => false,
    })
}

impl Analyzer for SynAnalyzer {
    fn analyze(&self, input: &AnalysisInput) -> Result<WorkspaceGraph, String> {
        // 1. Per-target discovery.
        let mut modules_all: Vec<Module> = Vec::new();
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let mut per_target: Vec<(Target, Vec<Module>)> = Vec::new();
        for target in &input.targets {
            let (modules, mut diags) = discover_modules_for_target(target, &input.workspace_root);
            diagnostics.append(&mut diags);
            per_target.push((target.clone(), modules.clone()));
            modules_all.extend(modules);
        }

        let mut ctxs: HashMap<crate::ids::TargetId, TargetCtx> = HashMap::new();
        for (target, modules) in &per_target {
            ctxs.insert(target.id.clone(), TargetCtx::of(target, modules));
        }
        let owner_to_target: HashMap<ModuleId, crate::ids::TargetId> = modules_all
            .iter()
            .map(|m| (m.id.clone(), m.target_id.clone()))
            .collect();
        let deps_by_name: HashMap<&str, Vec<&crate::metadata::ExternalDep>> = {
            let mut map: HashMap<&str, Vec<&crate::metadata::ExternalDep>> = HashMap::new();
            for dep in &input.deps {
                map.entry(dep.name.as_str()).or_default().push(dep);
            }
            map
        };
        // Bare member arm (amendment A2): member package name → each of that
        // package's targets' resolution contexts. All targets' module sets
        // already exist above, so cross-target resolution here is complete.
        let member_ctxs: HashMap<&str, Vec<&TargetCtx>> = {
            let mut map: HashMap<&str, Vec<&TargetCtx>> = HashMap::new();
            for target in &input.targets {
                if let Some(ctx) = ctxs.get(&target.id) {
                    map.entry(target.package_id.0.as_str())
                        .or_default()
                        .push(ctx);
                }
            }
            map
        };

        // 2. Uses + globs + cfg: one `extract_uses` call per file-backed
        // module (each canonical file parsed at most once; shared files get
        // one call per owner module with its distinct `from`).
        let mut raws: Vec<RawUse> = Vec::new();
        let mut cfg_seen = false;
        let mut parsed: HashMap<PathBuf, syn::File> = HashMap::new();
        for module in &modules_all {
            let Some(path) = module.source_path.clone() else {
                continue;
            };
            let fresh;
            let file = match parsed.get(&path) {
                Some(cached) => {
                    fresh = false;
                    cached.clone()
                }
                None => match parse_source_file(&path) {
                    Ok(file) => {
                        fresh = true;
                        parsed.insert(path.clone(), file.clone());
                        file
                    }
                    Err(message) => {
                        diagnostics.push(Diagnostic {
                            severity: DiagnosticSeverity::Error,
                            code: "IO001".to_string(),
                            message,
                            source_span: None,
                        });
                        continue;
                    }
                },
            };
            raws.extend(extract_uses(module, &file));
            collect_glob_diags(
                &file.items,
                &module.id,
                &[],
                &module.source_path,
                &mut diagnostics,
            );
            if fresh && file_has_cfg(&file.items) {
                cfg_seen = true;
            }
        }

        // 3. `mod` containment edges (parent → child, kind Module).
        let mut dependencies: Vec<Dependency> = Vec::new();
        for module in &modules_all {
            let Some(ctx) = ctxs.get(&module.target_id) else {
                continue;
            };
            if module.id.0 == ctx.prefix {
                continue;
            }
            let Some(logical) = logical_of(&module.id, &ctx.prefix) else {
                continue;
            };
            if logical.is_empty() {
                continue;
            }
            let parent = if logical.len() == 1 {
                ctx.prefix.clone()
            } else {
                format!(
                    "{}::{}",
                    ctx.prefix,
                    logical[..logical.len() - 1].join("::")
                )
            };
            dependencies.push(Dependency {
                from: ModuleId(parent),
                to: DependencyTarget::Internal(module.id.clone()),
                kind: DependencyKind::Module,
                source_span: module.source_span.clone(),
            });
        }

        // 4. Resolve every `RawUse` (`as` aliases never mint IDs).
        for raw in &raws {
            let Some(target_id) = owner_to_target.get(&raw.from) else {
                diagnostics.push(scope_diag(&raw.from, &raw.path, &raw.span));
                continue;
            };
            let Some(ctx) = ctxs.get(target_id) else {
                diagnostics.push(scope_diag(&raw.from, &raw.path, &raw.span));
                continue;
            };
            match resolve_use(raw, ctx, &member_ctxs, &deps_by_name) {
                Resolution::Edge(to) => dependencies.push(Dependency {
                    from: raw.from.clone(),
                    to,
                    kind: if raw.is_pub {
                        DependencyKind::ReExport
                    } else {
                        DependencyKind::Use
                    },
                    source_span: raw.span.clone(),
                }),
                Resolution::Unresolved(diag) => diagnostics.push(diag),
            }
        }

        // 5. Exactly one run-level CFG001 warning when any cfg was seen.
        // (No evaluation, no per-feature graphs; the graph reflects
        // unexpanded source, so cfg'd items above still resolved.)
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
        // the workspace manifest stands in for per-package manifest paths).
        let mut package_ids: Vec<_> = input.targets.iter().map(|t| t.package_id.clone()).collect();
        package_ids.sort();
        package_ids.dedup();
        let packages: Vec<Package> = package_ids
            .into_iter()
            .map(|id| Package {
                name: id.0.clone(),
                manifest_path: input.workspace_root.join("Cargo.toml"),
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
