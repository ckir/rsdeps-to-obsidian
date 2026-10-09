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

use crate::domain::{Diagnostic, DiagnosticSeverity, Module, SourceSpan, Target};
use crate::ids::{make_module_id, ModuleId};
use std::collections::HashMap;
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

        let dir = parent_of(containing);
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
