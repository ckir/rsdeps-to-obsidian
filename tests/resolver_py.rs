//! Task 4: Python module and import resolution (`PyAnalyzer`).
//!
//! Exercises `PyAnalyzer::analyze` over `tests/fixtures/py-resolve/`:
//! module discovery and `ModuleId` shape (`__init__` adds no segment,
//! namespace dirs become modules, `Bin` scripts are single root modules),
//! the `MOD003` ambiguity warning, relative-import anchoring (including
//! climbing above the root), stdlib silent-skips, item-level `SCOPE001`,
//! pinned-vs-unpinned third-party (`EXT001`), `TYPE_CHECKING` normality,
//! star-import edges, and fail-closed `Bin` targets with a missing root.

use rsdeps2obsidian::domain::{
    DependencyKind, DependencyTarget, DiagnosticSeverity, Target, TargetKind,
};
use rsdeps2obsidian::ids::{make_external_id, make_target_id, PackageId, TargetId};
use rsdeps2obsidian::metadata::ExternalDep;
use rsdeps2obsidian::resolver::{AnalysisInput, Analyzer};
use rsdeps2obsidian::resolver_py::PyAnalyzer;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/py-resolve")
        .join(name);
    std::fs::canonicalize(&root).unwrap_or(root)
}

fn lib_target(pkg: &str, name: &str, root: PathBuf) -> Target {
    Target {
        id: make_target_id(pkg, "lib", name).unwrap(),
        package_id: PackageId(pkg.to_string()),
        name: name.to_string(),
        kind: TargetKind::Lib,
        root_source: root,
    }
}

fn bin_target(pkg: &str, name: &str, root: PathBuf) -> Target {
    Target {
        id: TargetId(format!("{pkg}::bin::{name}")),
        package_id: PackageId(pkg.to_string()),
        name: name.to_string(),
        kind: TargetKind::Bin,
        root_source: root,
    }
}

fn analyze(input: &AnalysisInput) -> rsdeps2obsidian::domain::WorkspaceGraph {
    PyAnalyzer.analyze(input).expect("python analysis succeeds")
}

fn proj_input(root: &Path) -> AnalysisInput {
    AnalysisInput {
        workspace_root: root.to_path_buf(),
        targets: vec![
            lib_target("web", "web", root.to_path_buf()),
            bin_target("web", "tool", root.join("tool.py")),
        ],
        deps: vec![ExternalDep {
            name: "flask".to_string(),
            version: "3.0.0".to_string(),
        }],
    }
}

fn module_ids(graph: &rsdeps2obsidian::domain::WorkspaceGraph) -> Vec<&str> {
    graph.modules.iter().map(|m| m.id.0.as_str()).collect()
}

fn find_module<'a>(
    graph: &'a rsdeps2obsidian::domain::WorkspaceGraph,
    id: &str,
) -> &'a rsdeps2obsidian::domain::Module {
    graph
        .modules
        .iter()
        .find(|m| m.id.0 == id)
        .unwrap_or_else(|| panic!("missing module {id}; have {:?}", module_ids(graph)))
}

#[test]
fn modules_and_ids() {
    let root = fixture("proj");
    let graph = analyze(&proj_input(&root));
    // The Lib target's root is the package dir itself, so dotted paths land
    // exactly on `web::web` / `web::web::api`: `__init__.py` adds no
    // segment, namespace dirs are modules, and the `Bin` script `tool.py`
    // is both a `Bin` root module (`web::tool`, sorts first) and an import-
    // root file (`web::web::tool`, as the walk covers every `.py` file).
    assert_eq!(
        module_ids(&graph),
        vec![
            "web::tool",
            "web::web",
            "web::web::amb",
            "web::web::api",
            "web::web::ext",
            "web::web::ns",
            "web::web::ns::nmod",
            "web::web::pkg",
            "web::web::pkg::m",
            "web::web::rel",
            "web::web::sib",
            "web::web::star",
            "web::web::sub",
            "web::web::sub::deep",
            "web::web::tc",
            "web::web::tool",
            "web::web::top",
            "web::web::views",
        ]
    );

    let api = find_module(&graph, "web::web::api");
    assert_eq!(api.qualified_name, "api");
    assert!(api.source_path.as_ref().unwrap().ends_with("proj/api.py"));

    // The ambiguous `amb.py` loses to `amb/__init__.py`: the module points
    // at the package file.
    let amb = find_module(&graph, "web::web::amb");
    assert!(amb
        .source_path
        .as_ref()
        .unwrap()
        .ends_with("proj/amb/__init__.py"));

    // Namespace dirs carry no file of their own.
    let ns = find_module(&graph, "web::web::ns");
    assert_eq!(ns.source_path, None);
    assert_eq!(ns.qualified_name, "ns");

    let tool = find_module(&graph, "web::tool");
    assert!(tool.source_path.as_ref().unwrap().ends_with("tool.py"));

    // src-layout: the import root is `src/`, so `src/svc/...` nests under
    // the root exactly as discovery wires it, and the relative edge resolves.
    let src_root = fixture("srcproj");
    let src_graph = analyze(&AnalysisInput {
        workspace_root: src_root.clone(),
        targets: vec![lib_target("svc", "svc", src_root.join("src"))],
        deps: vec![],
    });
    assert_eq!(
        module_ids(&src_graph),
        vec!["svc::svc", "svc::svc::svc", "svc::svc::svc::core"]
    );
    assert!(
        src_graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "svc::svc::svc"
                && matches!(&d.to, DependencyTarget::Internal(to) if to.0 == "svc::svc::svc::core")
                && matches!(d.kind, DependencyKind::Use)),
        "src-layout relative edge missing: {:?}",
        src_graph.dependencies
    );
}

#[test]
fn mod003_and_overclimb() {
    let root = fixture("proj");
    let graph = analyze(&proj_input(&root));

    let mod003: Vec<_> = graph
        .diagnostics
        .iter()
        .filter(|d| d.code == "MOD003")
        .collect();
    assert_eq!(
        mod003.len(),
        1,
        "exactly one MOD003: {:?}",
        graph.diagnostics
    );
    assert!(matches!(mod003[0].severity, DiagnosticSeverity::Warning));
    assert_eq!(
        mod003[0].message,
        format!(
            "ambiguous python layout: both {} and {}/__init__.py exist; using package",
            root.join("amb.py").display(),
            root.join("amb").display()
        )
    );

    // Climbing above the root yields an unresolved diagnostic and no edge,
    // both nested (`web/top.py`, level 3) and at the top (`tool.py`).
    for (from, path) in [("web::web::top", "...x"), ("web::tool", "...x")] {
        let expected = format!("scope collision: {from} -> {path}; unresolved syntactic ownership");
        assert!(
            graph
                .diagnostics
                .iter()
                .any(|d| d.code == "SCOPE001" && d.message == expected),
            "missing overclimb SCOPE001 for {from}: {:?}",
            graph.diagnostics
        );
        assert!(
            graph.dependencies.iter().all(|d| d.from.0 != from),
            "overclimbing {from} must have no edges: {:?}",
            graph.dependencies
        );
    }
}

#[test]
fn import_semantics() {
    let root = fixture("proj");
    let graph = analyze(&proj_input(&root));

    let has_internal = |from: &str, to: &str| {
        graph.dependencies.iter().any(|d| {
            d.from.0 == from
                && matches!(&d.to, DependencyTarget::Internal(id) if id.0 == to)
                && matches!(d.kind, DependencyKind::Use)
        })
    };
    // Relative and absolute from-imports of submodules are `Use` edges.
    assert!(has_internal("web::web", "web::web::api"), "init edge");
    assert!(has_internal("web::web::rel", "web::web::sib"), "sib edge");
    assert!(
        has_internal("web::web::sub::deep", "web::web::pkg::m"),
        "parent-package edge"
    );
    assert!(
        has_internal("web::web::rel", "web::web::pkg::m"),
        "aliased import edge (alias ignored)"
    );
    // Plain `import`s take the longest-prefix edge (amendment-A rule),
    // even when the tail is not a known module.
    assert!(
        has_internal("web::web::rel", "web::web::pkg"),
        "prefix edge"
    );
    // `from m import *` is a normal edge to the module, with no diagnostic;
    // a star whose module only partly resolves follows the normal rules.
    assert!(has_internal("web::web::star", "web::web::pkg"), "star edge");
    assert!(
        graph.diagnostics.iter().any(|d| d.code == "SCOPE001"
            && d.message
                == "scope collision: web::web::star -> pkg.nonexistent; unresolved syntactic ownership"),
        "partial star SCOPE001 missing: {:?}",
        graph.diagnostics
    );
    assert!(
        graph
            .diagnostics
            .iter()
            .all(|d| !d.message.contains("-> pkg;")),
        "star import must not diagnose: {:?}",
        graph.diagnostics
    );
    // Pinned third-party (plain and from forms) collapses to one External.
    let flask = make_external_id("flask", "3.0.0");
    assert!(
        graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "web::web::ext"
                && matches!(&d.to, DependencyTarget::External(id) if *id == flask)),
        "pinned external edge missing: {:?}",
        graph.dependencies
    );
    // Item-level remainder is SCOPE001 with no edge, exact text.
    assert!(
        graph.diagnostics.iter().any(|d| {
            d.code == "SCOPE001"
            && d.message
                == "scope collision: web::web::views -> api.handler; unresolved syntactic ownership"
        }),
        "item SCOPE001 missing: {:?}",
        graph.diagnostics
    );
    // Unpinned third-party is EXT001, exact text.
    for expected in [
        "unresolved external: web::web::ext -> requests; no pinned version",
        "unresolved external: web::web::ext -> nosuchpkg.thing; no pinned version",
        "unresolved external: web::web::star -> nosuchstar; no pinned version",
        // `TYPE_CHECKING`-only imports are treated normally, not skipped.
        "unresolved external: web::web::tc -> ghostpkg; no pinned version",
        // `;`-separated statements on one row never leak names across each
        // other (`sys` stays silent, the sibling import still diagnoses).
        "unresolved external: web::web::sib -> ghostsecond; no pinned version",
    ] {
        assert!(
            graph.diagnostics.iter().any(|d| d.code == "EXT001"
                && matches!(d.severity, DiagnosticSeverity::Warning)
                && d.message == expected),
            "missing EXT001 `{expected}`: {:?}",
            graph.diagnostics
        );
    }
    // Parenthesized multi-line from-imports attribute each name to its own
    // line (snippet-row arithmetic, not statement-row).
    for (path, line) in [("nosuchmulti.item1", 6), ("nosuchmulti.item2", 7)] {
        let expected = format!("unresolved external: web::web::ext -> {path}; no pinned version");
        let diag = graph
            .diagnostics
            .iter()
            .find(|d| d.code == "EXT001" && d.message == expected)
            .unwrap_or_else(|| panic!("missing EXT001 `{expected}`: {:?}", graph.diagnostics));
        let span = diag.source_span.as_ref().expect("import span");
        assert_eq!(span.line_start, line, "line for {path}");
        assert!(span.file.ends_with("proj/ext.py"), "file for {path}");
    }
    // Stdlib is silently skipped: no node, no diagnostic.
    assert!(
        graph
            .dependencies
            .iter()
            .all(|d| d.from.0 != "web::web::api"),
        "`import os` must yield no edge: {:?}",
        graph.dependencies
    );
    assert!(
        graph
            .diagnostics
            .iter()
            .all(|d| !d.message.contains("web::web::api")),
        "`import os` must yield no diagnostic: {:?}",
        graph.diagnostics
    );
    // The single-run CFG001 rule stays Rust-only.
    assert!(
        graph.diagnostics.iter().all(|d| d.code != "CFG001"),
        "no CFG001 for python: {:?}",
        graph.diagnostics
    );
}

#[test]
fn missing_bin_root_fails_closed() {
    // A `Bin` target whose `root_source` does not exist is an IO-family
    // Error diagnostic: no panic, no abort, other modules unaffected.
    let root = fixture("proj");
    let mut input = proj_input(&root);
    input
        .targets
        .push(bin_target("web", "ghost", root.join("nonexistent.py")));
    let graph = analyze(&input);

    assert!(
        graph
            .diagnostics
            .iter()
            .any(|d| d.code == "IO001" && matches!(d.severity, DiagnosticSeverity::Error)),
        "missing Bin root needs an IO001 Error: {:?}",
        graph.diagnostics
    );
    for id in [
        "web::web",
        "web::web::api",
        "web::web::ns::nmod",
        "web::web::pkg::m",
        "web::tool",
        "web::ghost",
    ] {
        assert!(
            module_ids(&graph).contains(&id),
            "missing {id} after IO failure"
        );
    }
    assert!(
        graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "web::web::rel"),
        "other targets still resolve: {:?}",
        graph.dependencies
    );
}

fn shop_input() -> AnalysisInput {
    // The `Bin` script lives OUTSIDE the `Lib` import root on purpose: it is
    // never walked into the `Lib` tree, so every Bin edge below is exactly
    // one edge (no `Lib`-copy twin).
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/py-resolve");
    let base = std::fs::canonicalize(&base).unwrap_or(base);
    AnalysisInput {
        workspace_root: base.clone(),
        targets: vec![
            lib_target("shop", "shop", base.join("shop")),
            bin_target("shop", "ship", base.join("ship.py")),
        ],
        deps: vec![],
    }
}

#[test]
fn own_package_absolute_resolves_against_lib_trees() {
    let graph = analyze(&shop_input());
    assert_eq!(
        module_ids(&graph),
        vec![
            "shop::ship",
            "shop::shop",
            "shop::shop::models",
            "shop::shop::sib",
        ]
    );

    // Bin `import shop.models` → exactly one Internal edge to the Lib module.
    let to_models: Vec<_> = graph
        .dependencies
        .iter()
        .filter(|d| {
            d.from.0 == "shop::ship"
                && matches!(&d.to, DependencyTarget::Internal(id) if id.0 == "shop::shop::models")
        })
        .collect();
    assert_eq!(
        to_models.len(),
        1,
        "one edge to models: {:?}",
        graph.dependencies
    );
    assert!(
        matches!(to_models[0].kind, DependencyKind::Use),
        "kind stays Use: {:?}",
        to_models[0]
    );
    // `import shop` (empty remainder) edges to the Lib root itself.
    assert!(
        graph.dependencies.iter().any(|d| d.from.0 == "shop::ship"
            && matches!(&d.to, DependencyTarget::Internal(id) if id.0 == "shop::shop")
            && matches!(d.kind, DependencyKind::Use)),
        "empty-remainder root edge missing: {:?}",
        graph.dependencies
    );
    // `import shop.ghost` → SCOPE001 exact text, never EXT001, no edge.
    assert!(
        graph.diagnostics.iter().any(|d| d.code == "SCOPE001"
            && d.message
                == "scope collision: shop::ship -> shop.ghost; unresolved syntactic ownership"),
        "ghost SCOPE001 missing: {:?}",
        graph.diagnostics
    );
    assert!(
        graph.diagnostics.iter().all(|d| d.code != "EXT001"),
        "own namespace is never external: {:?}",
        graph.diagnostics
    );
    // Lib self-prefixed absolute gets the same treatment uniformly.
    assert!(
        graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "shop::shop::models"
                && matches!(&d.to, DependencyTarget::Internal(id) if id.0 == "shop::shop::sib")
                && matches!(d.kind, DependencyKind::Use)),
        "lib self-prefix edge missing: {:?}",
        graph.dependencies
    );
}

#[test]
fn relative_miss_is_scope_not_external() {
    // `from . import nosuchsib` in the Bin script misses everywhere: it is
    // an ownership failure (SCOPE001), never a pin failure (EXT001).
    let graph = analyze(&shop_input());
    assert!(
        graph.diagnostics.iter().any(|d| d.code == "SCOPE001"
            && d.message
                == "scope collision: shop::ship -> .nosuchsib; unresolved syntactic ownership"),
        "relative-miss SCOPE001 missing: {:?}",
        graph.diagnostics
    );
    assert!(
        graph
            .diagnostics
            .iter()
            .all(|d| !d.message.contains("nosuchsib") || d.code == "SCOPE001"),
        "relative miss must never be EXT001: {:?}",
        graph.diagnostics
    );
    assert!(
        graph
            .dependencies
            .iter()
            .all(|d| d.from.0 != "shop::ship" || !matches!(&d.to, DependencyTarget::External(_))),
        "relative miss must have no external edge: {:?}",
        graph.dependencies
    );
}

#[test]
fn nested_owning_tree_match_stays_primary() {
    // Guard for the miss-only ordering of the own-package arm: in a nested
    // layout (import root above the package dir) the owning tree already
    // contains the full `svc.util` path, so `import svc.util` edges
    // directly instead of stripping to a top-level `util` that is not meant.
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/py-resolve");
    let base = std::fs::canonicalize(&base).unwrap_or(base);
    let graph = analyze(&AnalysisInput {
        workspace_root: base.clone(),
        targets: vec![lib_target("svc", "svc", base.join("nest"))],
        deps: vec![],
    });
    assert!(
        graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "svc::svc::svc::core"
                && matches!(&d.to, DependencyTarget::Internal(id) if id.0 == "svc::svc::svc::util")
                && matches!(d.kind, DependencyKind::Use)),
        "nested verbatim edge missing: {:?}",
        graph.dependencies
    );
    assert!(
        graph
            .diagnostics
            .iter()
            .all(|d| d.code != "SCOPE001" && d.code != "EXT001"),
        "no miss diagnostics for a full owning match: {:?}",
        graph.diagnostics
    );
}
