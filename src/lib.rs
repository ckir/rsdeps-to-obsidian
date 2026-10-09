pub const PROTOCOL_VERSION: u32 = 1;
pub const SPEC_VERSION: &str = "0.5.0";
pub const SCHEMA_VERSION: u32 = 3;
pub const GENERATOR: &str = "cargo-obsidian-graph";

pub mod commit;
pub mod config;
pub mod diag;
pub mod discovery_py;
pub mod domain;
pub mod graph;
pub mod ids;
pub mod manifest;
pub mod metadata;
pub mod paths;
pub mod render;
pub mod resolver;
pub mod resolver_py;
pub mod ts_parse;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Summary of one end-to-end run, printed by the binary on stdout.
///
/// Counts (`packages`/`targets`/`modules`/`dependencies`) describe the
/// analyzed graph; `external` is the number of distinct external crates
/// referenced by it. `create`/`update`/`delete` are the sorted file lists
/// from the commit plan (what happened, or — under `--dry-run` — what would
/// happen); `warnings` counts `Warning`-severity diagnostics. In `--clean`
/// mode only `workspace` (the output root) and `delete` are meaningful; all
/// other counts are zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSummary {
    pub workspace: PathBuf,
    pub packages: usize,
    pub targets: usize,
    pub modules: usize,
    pub dependencies: usize,
    pub external: usize,
    pub create: Vec<String>,
    pub update: Vec<String>,
    pub delete: Vec<String>,
    pub warnings: usize,
}

/// Filename of the machine-readable graph inside the output root.
pub const GRAPH_FILENAME: &str = "graph.json";

/// Resolve the language backend's [`Analyzer`]. The Rust and Python arms are
/// live; the TypeScript arm returns the exact placeholder error that the
/// TypeScript task replaces with a real constructor.
pub fn analyzer_for(lang: config::Lang) -> Result<Box<dyn resolver::Analyzer>, String> {
    match lang {
        // `Auto` never reaches here (`run()` resolves it first); treating it
        // as Rust keeps direct callers on the pre-`--lang` behavior.
        config::Lang::Auto | config::Lang::Rust => Ok(Box::new(resolver::SynAnalyzer)),
        config::Lang::Python => Ok(Box::new(resolver_py::PyAnalyzer)),
        config::Lang::Typescript => Err("typescript support not yet implemented".to_string()),
    }
}

/// Run project discovery for `lang`. The Rust and Python arms are live; the
/// TypeScript arm returns the same exact placeholder error as
/// [`analyzer_for`].
pub fn discover_for(
    lang: config::Lang,
    config: &config::Config,
) -> Result<metadata::DiscoveredWorkspace, String> {
    match lang {
        config::Lang::Auto | config::Lang::Rust => metadata::discover(config),
        config::Lang::Python => discovery_py::discover_py(config),
        config::Lang::Typescript => Err("typescript support not yet implemented".to_string()),
    }
}

/// The `LANG001` Info diagnostic for ambiguous autodetect: no explicit
/// `--lang` and more than one project file present. `None` for explicit
/// flags and single-project (or bare) directories, which stay silent.
fn lang_multi_notice(dir: &std::path::Path, lang: config::Lang) -> Option<domain::Diagnostic> {
    let present = config::project_files_present(dir);
    if present.len() < 2 {
        return None;
    }
    let (chosen, others) = present.split_first().expect("len >= 2 checked");
    Some(domain::Diagnostic {
        severity: domain::DiagnosticSeverity::Info,
        code: "LANG001".to_string(),
        message: format!(
            "multiple project files detected; analyzing {chosen} ({}), ignoring: {}",
            lang.name(),
            others.join(", ")
        ),
        source_span: None,
    })
}

/// Run the full pipeline: resolve the output root → discover the workspace →
/// analyze → sort → render Markdown + JSON → diff against the previous
/// manifest → report orphans and diagnostics on stderr → commit (or compute
/// the plan only under `--dry-run`).
///
/// Diagnostics (sorted, `severity[CODE]: message` with a workspace-relative
/// `(at path:line)` suffix when a span exists) and `orphan generated file:`
/// lines go to stderr via `eprintln!`; the caller prints the [`RunSummary`]
/// on stdout. Stdin is never touched.
///
/// Exit-code contract (applied by the binary): `Ok` → 0, `Err` → 1. Exit 2
/// is reserved for protocol/compat failures and unused in the MVP.
///
/// `--clean` mode deletes exactly the manifest-listed entries and performs
/// no analysis: a missing manifest is reported with zero deletes (`Ok`), an
/// invalid manifest is fatal (`Err`, zero deletes). `--dry-run` (with or
/// without `--clean`) performs zero writes and zero deletes.
pub fn run(config: &config::Config) -> Result<RunSummary, String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot determine current directory: {e}"))?;
    let output_root = paths::resolve_output_root(&config.output_raw, &cwd)?;

    if config.clean {
        return run_clean(config, &output_root);
    }

    // Explicit `--lang` overrides autodetect silently; `auto` scans the
    // manifest's parent dir when `--manifest-path` is set, else the
    // workspace input dir (first project file in priority order wins).
    // The LANG001 notice below scans this same dir.
    let explicit = config.lang != config::Lang::Auto;
    let scan_dir: PathBuf = match &config.manifest_path {
        Some(manifest) => manifest
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")),
        None => config.workspace_input.clone(),
    };
    let lang = if explicit {
        config.lang
    } else {
        config::detect_language(&scan_dir)
    };

    let discovered = discover_for(lang, config)?;
    let analyzer = analyzer_for(lang)?;
    let input = resolver::AnalysisInput {
        workspace_root: discovered.root.clone(),
        targets: discovered.targets.clone(),
        deps: discovered.external_deps.clone(),
    };
    let mut graph = analyzer.analyze(&input)?;

    // AnalysisInput carries no packages, so analyze() synthesizes placeholder
    // manifest paths; restore the real per-package paths (matched by
    // PackageId, synthesized value kept when unmatched).
    let real_manifests: HashMap<&crate::ids::PackageId, &PathBuf> = discovered
        .packages
        .iter()
        .map(|p| (&p.id, &p.manifest_path))
        .collect();
    for package in &mut graph.packages {
        if let Some(real) = real_manifests.get(&package.id) {
            package.manifest_path = (*real).clone();
        }
    }
    graph::sort_graph(&mut graph);

    // Ambiguous autodetect names the ignored project files; explicit flags
    // and single-project runs stay silent. Re-sort to keep the §22 total
    // order (Info sorts first, so the notice lands at the head).
    if !explicit {
        if let Some(notice) = lang_multi_notice(&scan_dir, lang) {
            graph.diagnostics.push(notice);
            diag::sort_diagnostics(&mut graph.diagnostics);
        }
    }

    let mut rendered = render::render_all(&graph, &discovered.root, config.external);
    rendered.push(render::RenderedFile {
        rel_path: GRAPH_FILENAME.to_string(),
        content: render::render_graph_json(&graph),
    });
    let manifest = manifest::build_manifest(&rendered, &graph);

    // Missing previous manifest → deletions disabled; invalid previous
    // manifest → warn and proceed with deletions disabled (the commit
    // overwrites it as the new commit record; leftovers surface as orphans
    // on the next run). The --clean path is stricter (see run_clean).
    let prev = match manifest::read_manifest(&output_root.join(commit::MANIFEST_FILENAME)) {
        Ok(prev) => prev,
        Err(e) => {
            eprintln!("warning: ignoring invalid previous manifest: {e}");
            None
        }
    };

    for line in commit::detect_orphans(&output_root, prev.as_ref()) {
        eprintln!("{line}");
    }
    for diagnostic in &graph.diagnostics {
        let severity = match diagnostic.severity {
            domain::DiagnosticSeverity::Info => "info",
            domain::DiagnosticSeverity::Warning => "warning",
            domain::DiagnosticSeverity::Error => "error",
        };
        let mut line = format!("{severity}[{}]: {}", diagnostic.code, diagnostic.message);
        if let Some(span) = &diagnostic.source_span {
            line.push_str(&format!(
                " (at {}:{})",
                diag::workspace_rel(&span.file, &discovered.root),
                span.line_start
            ));
        }
        eprintln!("{line}");
    }
    let warnings = graph
        .diagnostics
        .iter()
        .filter(|d| matches!(d.severity, domain::DiagnosticSeverity::Warning))
        .count();

    let plan = commit::commit(
        &rendered,
        &manifest,
        &output_root,
        prev.as_ref(),
        config.dry_run,
    )?;

    let mut externals: HashSet<&crate::ids::ExternalCrateId> = HashSet::new();
    for dependency in &graph.dependencies {
        match &dependency.to {
            domain::DependencyTarget::External(id) => {
                externals.insert(id);
            }
            domain::DependencyTarget::Internal(_) => {}
        }
    }

    Ok(RunSummary {
        workspace: discovered.root,
        packages: graph.packages.len(),
        targets: graph.targets.len(),
        modules: graph.modules.len(),
        dependencies: graph.dependencies.len(),
        external: externals.len(),
        create: plan.create,
        update: plan.update,
        delete: plan.delete,
        warnings,
    })
}

/// `--clean` path: delete exactly the manifest-listed entries, no analysis.
/// Missing manifest is reported with zero deletes (`Ok`); an invalid
/// manifest is fatal (`Err`) with zero deletes. `--dry-run --clean` is
/// observational: the manifest-listed set is reported as the delete plan
/// with zero deletes.
fn run_clean(config: &config::Config, output_root: &std::path::Path) -> Result<RunSummary, String> {
    let manifest_path = output_root.join(commit::MANIFEST_FILENAME);
    let empty = |delete: Vec<String>| RunSummary {
        workspace: output_root.to_path_buf(),
        packages: 0,
        targets: 0,
        modules: 0,
        dependencies: 0,
        external: 0,
        create: Vec::new(),
        update: Vec::new(),
        delete,
        warnings: 0,
    };
    match manifest::read_manifest(&manifest_path)? {
        None => {
            eprintln!(
                "clean: no manifest at {}; nothing to delete",
                manifest_path.display()
            );
            Ok(empty(Vec::new()))
        }
        Some(manifest) => {
            if config.dry_run {
                let mut delete = manifest.generated_files.clone();
                delete.sort();
                delete.dedup();
                return Ok(empty(delete));
            }
            let deleted = commit::clean(output_root, &manifest)?;
            for rel in &deleted {
                eprintln!("clean: deleted {rel}");
            }
            Ok(empty(deleted))
        }
    }
}
