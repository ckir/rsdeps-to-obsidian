//! Markdown + JSON rendering from the canonical [`WorkspaceGraph`] (Task 7).
//!
//! * [`render_all`] emits one module note per module, one external note per
//!   distinct external crate (only when `external` is set), and the
//!   `index.md` root note. Every note carries the handshake
//!   (`protocol_version` 1, `spec_version` 0.5.0, `schema_version` 3,
//!   `generator` cargo-obsidian-graph). Frontmatter field order is fixed:
//!   `graph_type, protocol_version, spec_version, schema_version, generator,
//!   module_id, target_id, package_id, [external_crate_id], [source]`.
//! * Filenames come from [`crate::paths::module_filename`] +
//!   [`crate::paths::windows_safe_name`] with ONE shared `taken` set threaded
//!   through every call (modules in `ModuleId` order, then externals in ID
//!   order) so collisions resolve across the whole output set. Wikilinks use
//!   the `[[stem|id]]` shape (stem = filename without `.md`) in dependency
//!   order; external dependencies link the same way whether or not their
//!   notes are emitted.
//! * [`render_graph_json`] serializes the handshake envelope plus sorted
//!   packages/targets/modules/dependencies (externals always included) and
//!   diagnostics, pretty-printed with a trailing newline. The graph is cloned
//!   and [`crate::graph::sort_graph`]-sorted first so output is deterministic
//!   for any input order.

use crate::diag::{sort_diagnostics, workspace_rel};
use crate::domain::{DependencyKind, DependencyTarget, WorkspaceGraph};
use crate::graph::{compare_dependencies, sort_graph};
use crate::ids::{ExternalCrateId, ModuleId};
use crate::paths::{module_filename, windows_safe_name};
use crate::{GENERATOR, PROTOCOL_VERSION, SCHEMA_VERSION, SPEC_VERSION};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// One generated file: output-root-relative path plus its full bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedFile {
    pub rel_path: String,
    pub content: String,
}

/// Map a module ID to its note filename, claiming the stem in `taken`.
pub fn module_note_filename(module_id: &ModuleId, taken: &mut HashSet<String>) -> String {
    let raw = module_filename(module_id);
    let stem = raw.strip_suffix(".md").unwrap_or(&raw);
    format!("{}.md", windows_safe_name(stem, taken))
}

/// Map an external crate ID (`name@version`) to its note filename, claiming
/// the stem in the shared `taken` set. The stem keeps the `rust-external-`
/// prefix so external notes are recognizable; `_` in the name is escaped as
/// `___` (mirroring [`module_filename`]) before the `@` join becomes `__`.
pub fn external_note_filename(
    external_id: &ExternalCrateId,
    taken: &mut HashSet<String>,
) -> String {
    let escaped = external_id.0.replace('_', "___").replace('@', "__");
    let stem = format!("rust-external-{escaped}");
    format!("{}.md", windows_safe_name(&stem, taken))
}

/// Assign deterministic filenames to every module and external crate in the
/// graph: modules in (`ModuleId`, `TargetId`) order, then distinct externals
/// in ID order, all through one shared `taken` set. Returns
/// (`(target_id, module_id)` -> filename, external_id -> filename).
///
/// The module map is keyed by module *identity* (`target_id`, `module_id`),
/// not by the id string: two modules may share one `ModuleId` string (e.g.
/// the lib root and a same-named bin root) while `ModuleId` strings stay
/// kind-token-free per spec §9.3, so keying by the id string would collapse
/// them to one entry (second filename overwrites the first, both notes share
/// one path, last-write-wins in commit). Every distinct identity claims its
/// own stem from `taken`, so filenames are injective. When all `ModuleId`
/// strings are distinct the extra `TargetId` tiebreak never fires and the
/// assignment is byte-identical to id-only ordering.
pub fn assign_filenames(
    graph: &WorkspaceGraph,
) -> (HashMap<(String, String), String>, HashMap<String, String>) {
    let mut taken: HashSet<String> = HashSet::new();
    taken.insert("index".to_string());
    let mut modules: Vec<&crate::domain::Module> = graph.modules.iter().collect();
    modules.sort_by(|a, b| (&a.id, &a.target_id).cmp(&(&b.id, &b.target_id)));
    let mut module_files = HashMap::new();
    for m in modules {
        module_files.insert(
            (m.target_id.0.clone(), m.id.0.clone()),
            module_note_filename(&m.id, &mut taken),
        );
    }
    let mut external_ids: Vec<&ExternalCrateId> = Vec::new();
    for dep in &graph.dependencies {
        if let DependencyTarget::External(e) = &dep.to {
            if !external_ids.contains(&e) {
                external_ids.push(e);
            }
        }
    }
    external_ids.sort();
    let mut external_files = HashMap::new();
    for id in external_ids {
        external_files.insert(id.0.clone(), external_note_filename(id, &mut taken));
    }
    (module_files, external_files)
}

/// Workspace-relative `/`-normalized `source` value for a module's file, or
/// `None` when the module carries no path or the path escapes the workspace
/// root (omitted only if unsafe).
fn source_rel(graph_module: &crate::domain::Module, workspace_root: &Path) -> Option<String> {
    let path = graph_module
        .source_path
        .as_ref()
        .or_else(|| graph_module.source_span.as_ref().map(|s| &s.file))?;
    let rel = workspace_rel(path, workspace_root);
    if rel.is_empty() || rel == ".." || rel.starts_with("../") || rel.starts_with('/') {
        return None;
    }
    if rel.len() > 1 && rel.as_bytes()[1] == b':' {
        return None;
    }
    Some(rel)
}

fn handshake_frontmatter(graph_type: &str) -> String {
    format!(
        "graph_type: {graph_type}\nprotocol_version: {PROTOCOL_VERSION}\nspec_version: {SPEC_VERSION}\nschema_version: {SCHEMA_VERSION}\ngenerator: {GENERATOR}\n"
    )
}

fn short_name(module_id: &str) -> &str {
    module_id.rsplit("::").next().unwrap_or(module_id)
}

fn stem_of(filename: &str) -> &str {
    filename.strip_suffix(".md").unwrap_or(filename)
}

/// Render one module note. `outgoing`/`incoming` must already be in
/// dependency order.
#[allow(clippy::too_many_arguments)]
fn module_note(
    graph_module: &crate::domain::Module,
    package_id: &str,
    outgoing: &[String],
    incoming: &[String],
    workspace_root: &Path,
    filename: &str,
) -> RenderedFile {
    let mut front = handshake_frontmatter("rust-module");
    front.push_str(&format!(
        "module_id: {}\ntarget_id: {}\n",
        graph_module.id.0, graph_module.target_id.0
    ));
    if !package_id.is_empty() {
        front.push_str(&format!("package_id: {package_id}\n"));
    }
    if let Some(source) = source_rel(graph_module, workspace_root) {
        front.push_str(&format!("source: {source}\n"));
    }
    let section = |title: &str, links: &[String]| {
        if links.is_empty() {
            format!("## {title}\n\n- (none)\n")
        } else {
            let mut out = format!("## {title}\n\n");
            for link in links {
                out.push_str(&format!("- {link}\n"));
            }
            out
        }
    };
    let content = format!(
        "---\n{front}---\n\n# {}\n\n{}{}\n",
        short_name(&graph_module.id.0),
        section("Depends on", outgoing),
        section("Used by", incoming),
    );
    RenderedFile {
        rel_path: filename.to_string(),
        content,
    }
}

fn external_note(external_id: &str, used_by: &[String], filename: &str) -> RenderedFile {
    let mut front = handshake_frontmatter("rust-external-crate");
    front.push_str(&format!("external_crate_id: {external_id}\n"));
    let used_by_section = if used_by.is_empty() {
        "## Used by\n\n- (none)\n".to_string()
    } else {
        let mut out = "## Used by\n\n".to_string();
        for link in used_by {
            out.push_str(&format!("- {link}\n"));
        }
        out
    };
    let content = format!(
        "---\n{front}---\n\n# {external_id}\n\nExternal Cargo dependency.\n\n{used_by_section}\n"
    );
    RenderedFile {
        rel_path: filename.to_string(),
        content,
    }
}

fn root_note(graph: &WorkspaceGraph) -> RenderedFile {
    let mut packages: Vec<&str> = graph.packages.iter().map(|p| p.id.0.as_str()).collect();
    packages.sort();
    let mut targets: Vec<&str> = graph.targets.iter().map(|t| t.id.0.as_str()).collect();
    targets.sort();
    let mut diags = graph.diagnostics.clone();
    sort_diagnostics(&mut diags);
    let mut content = format!(
        "---\n{}---\n\n# Workspace dependencies\n",
        handshake_frontmatter("rust-dependency-root")
    );
    content.push_str("\n## Packages\n\n");
    if packages.is_empty() {
        content.push_str("- (none)\n");
    } else {
        for p in packages {
            content.push_str(&format!("- {p}\n"));
        }
    }
    content.push_str("\n## Targets\n\n");
    if targets.is_empty() {
        content.push_str("- (none)\n");
    } else {
        for t in targets {
            content.push_str(&format!("- {t}\n"));
        }
    }
    content.push_str("\n## Diagnostics\n\n");
    if diags.is_empty() {
        content.push_str("- (none)\n");
    } else {
        for d in &diags {
            let severity = match d.severity {
                crate::domain::DiagnosticSeverity::Info => "Info",
                crate::domain::DiagnosticSeverity::Warning => "Warning",
                crate::domain::DiagnosticSeverity::Error => "Error",
            };
            content.push_str(&format!("- [{severity}] {}: {}\n", d.code, d.message));
        }
    }
    RenderedFile {
        rel_path: "index.md".to_string(),
        content,
    }
}

/// Render every Markdown note: module notes in `ModuleId` order, external
/// notes (only when `external` is set) in crate-ID order, then `index.md`.
/// Output is deterministic for any input order.
pub fn render_all(
    graph: &WorkspaceGraph,
    workspace_root: &Path,
    external: bool,
) -> Vec<RenderedFile> {
    let mut sorted = graph.clone();
    sort_graph(&mut sorted);
    let (module_files, external_files) = assign_filenames(&sorted);

    // Wikilinks carry only the `ModuleId` string, so a shared id is
    // inherently ambiguous: resolve it to the first module in the same
    // (`ModuleId`, `TargetId`) assign order above (the `TargetId` tiebreak
    // keeps this deterministic for any input order). Link shape is unchanged.
    let mut link_files: HashMap<String, String> = HashMap::new();
    {
        let mut ordered: Vec<&crate::domain::Module> = sorted.modules.iter().collect();
        ordered.sort_by(|a, b| (&a.id, &a.target_id).cmp(&(&b.id, &b.target_id)));
        for m in ordered {
            link_files.entry(m.id.0.clone()).or_insert_with(|| {
                module_files
                    .get(&(m.target_id.0.clone(), m.id.0.clone()))
                    .cloned()
                    .unwrap_or_default()
            });
        }
    }

    let package_of: HashMap<&str, &str> = sorted
        .targets
        .iter()
        .map(|t| (t.id.0.as_str(), t.package_id.0.as_str()))
        .collect();

    let link_for = |to: &DependencyTarget| -> String {
        match to {
            DependencyTarget::Internal(m) => {
                let file = link_files.get(m.0.as_str()).cloned().unwrap_or_default();
                format!("[[{}|{}]]", stem_of(&file), m.0)
            }
            DependencyTarget::External(e) => {
                let file = external_files
                    .get(e.0.as_str())
                    .cloned()
                    .unwrap_or_default();
                format!("[[{}|{}]]", stem_of(&file), e.0)
            }
        }
    };

    let mut out = Vec::new();
    for m in &sorted.modules {
        let outgoing: Vec<String> = sorted
            .dependencies
            .iter()
            .filter(|d| d.from == m.id)
            .map(|d| link_for(&d.to))
            .collect();
        // `sort_graph` already ordered `dependencies`; the filter preserves
        // that order. Re-sort defensively is unnecessary; kept as-is.
        let incoming: Vec<String> = sorted
            .dependencies
            .iter()
            .filter(|d| match &d.to {
                DependencyTarget::Internal(target) => *target == m.id,
                DependencyTarget::External(_) => false,
            })
            .map(|d| {
                let file = link_files
                    .get(d.from.0.as_str())
                    .cloned()
                    .unwrap_or_default();
                format!("[[{}|{}]]", stem_of(&file), d.from.0)
            })
            .collect();
        let package_id = package_of
            .get(m.target_id.0.as_str())
            .copied()
            .unwrap_or("");
        let filename = module_files
            .get(&(m.target_id.0.clone(), m.id.0.clone()))
            .cloned()
            .unwrap_or_default();
        out.push(module_note(
            m,
            package_id,
            &outgoing,
            &incoming,
            workspace_root,
            &filename,
        ));
    }

    if external {
        let mut ids: Vec<&String> = external_files.keys().collect();
        ids.sort();
        for id in ids {
            let used_by: Vec<String> = sorted
                .dependencies
                .iter()
                .filter(|d| match &d.to {
                    DependencyTarget::External(e) => &e.0 == id,
                    DependencyTarget::Internal(_) => false,
                })
                .map(|d| {
                    let file = link_files
                        .get(d.from.0.as_str())
                        .cloned()
                        .unwrap_or_default();
                    format!("[[{}|{}]]", stem_of(&file), d.from.0)
                })
                .collect();
            let filename = external_files.get(id.as_str()).cloned().unwrap_or_default();
            out.push(external_note(id, &used_by, &filename));
        }
    }

    out.push(root_note(&sorted));
    out
}

// --- JSON rendering ---

fn kind_token(kind: &DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Module => "module",
        DependencyKind::Use => "use",
        DependencyKind::ReExport => "re_export",
    }
}

#[derive(Serialize)]
struct JsonSpan {
    file: String,
    line_start: u32,
    line_end: u32,
}

fn json_span(span: &Option<crate::domain::SourceSpan>) -> Option<JsonSpan> {
    span.as_ref().map(|s| JsonSpan {
        file: s.file.to_string_lossy().into_owned(),
        line_start: s.line_start,
        line_end: s.line_end,
    })
}

#[derive(Serialize)]
struct JsonPackage {
    id: String,
    name: String,
    manifest_path: String,
}

#[derive(Serialize)]
struct JsonTarget {
    id: String,
    package_id: String,
    name: String,
    kind: String,
    root_source: String,
}

#[derive(Serialize)]
struct JsonModule {
    id: String,
    target_id: String,
    name: String,
    qualified_name: String,
    source_path: Option<String>,
    source_span: Option<JsonSpan>,
}

#[derive(Serialize)]
struct JsonDependency {
    from: String,
    to: String,
    target_type: String,
    kind: String,
}

#[derive(Serialize)]
struct JsonDiagnostic {
    severity: String,
    code: String,
    message: String,
    source_span: Option<JsonSpan>,
}

/// Handshake-first JSON graph. Field order is the struct order: handshake
/// envelope, then packages/targets/modules/dependencies/diagnostics.
#[derive(Serialize)]
struct JsonGraph {
    protocol_version: u32,
    spec_version: String,
    schema_version: u32,
    generator: String,
    packages: Vec<JsonPackage>,
    targets: Vec<JsonTarget>,
    modules: Vec<JsonModule>,
    dependencies: Vec<JsonDependency>,
    diagnostics: Vec<JsonDiagnostic>,
}

/// Serialize the graph (externals always included) pretty-printed with a
/// trailing newline. The graph is sorted first, so bytes are deterministic.
pub fn render_graph_json(graph: &WorkspaceGraph) -> String {
    let mut sorted = graph.clone();
    sort_graph(&mut sorted);
    // Belt-and-braces: keep the explicit comparator visible at the boundary
    // so JSON order never depends on `sort_graph` internals alone.
    sorted.dependencies.sort_by(compare_dependencies);
    let json = JsonGraph {
        protocol_version: PROTOCOL_VERSION,
        spec_version: SPEC_VERSION.to_string(),
        schema_version: SCHEMA_VERSION,
        generator: GENERATOR.to_string(),
        packages: sorted
            .packages
            .iter()
            .map(|p| JsonPackage {
                id: p.id.0.clone(),
                name: p.name.clone(),
                manifest_path: p.manifest_path.to_string_lossy().into_owned(),
            })
            .collect(),
        targets: sorted
            .targets
            .iter()
            .map(|t| JsonTarget {
                id: t.id.0.clone(),
                package_id: t.package_id.0.clone(),
                name: t.name.clone(),
                kind: t.kind.kind_token().to_string(),
                root_source: t.root_source.to_string_lossy().into_owned(),
            })
            .collect(),
        modules: sorted
            .modules
            .iter()
            .map(|m| JsonModule {
                id: m.id.0.clone(),
                target_id: m.target_id.0.clone(),
                name: m.name.clone(),
                qualified_name: m.qualified_name.clone(),
                source_path: m
                    .source_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                source_span: json_span(&m.source_span),
            })
            .collect(),
        dependencies: sorted
            .dependencies
            .iter()
            .map(|d| {
                let (to, target_type) = match &d.to {
                    DependencyTarget::Internal(m) => (m.0.clone(), "internal".to_string()),
                    DependencyTarget::External(e) => (e.0.clone(), "external".to_string()),
                };
                JsonDependency {
                    from: d.from.0.clone(),
                    to,
                    target_type,
                    kind: kind_token(&d.kind).to_string(),
                }
            })
            .collect(),
        diagnostics: sorted
            .diagnostics
            .iter()
            .map(|d| JsonDiagnostic {
                severity: match d.severity {
                    crate::domain::DiagnosticSeverity::Info => "info".to_string(),
                    crate::domain::DiagnosticSeverity::Warning => "warning".to_string(),
                    crate::domain::DiagnosticSeverity::Error => "error".to_string(),
                },
                code: d.code.clone(),
                message: d.message.clone(),
                source_span: json_span(&d.source_span),
            })
            .collect(),
    };
    let mut out = serde_json::to_string_pretty(&json).expect("graph JSON serialization");
    out.push('\n');
    out
}
