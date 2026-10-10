//! Task 3: TypeScript module and import resolution (`TsAnalyzer`).
//!
//! Exercises `TsAnalyzer::analyze` over `tests/fixtures/ts-resolve/`:
//! tsconfig-relative `ModuleId` shape (`index.*` barrels drop their segment,
//! `.d.ts` and `node_modules/` skipped silently), extension probe order with
//! the single `MOD004` warning, and import semantics (`export`-from,
//! `require('lit')`, dynamic `import('lit')`, `import type`, `paths`
//! longest-prefix, pinned vs unpinned bare specifiers, `node:`/core silent
//! skips, own-package bare resolution).

use rsdeps2obsidian::domain::{
    DependencyKind, DependencyTarget, DiagnosticSeverity, Target, TargetKind,
};
use rsdeps2obsidian::ids::{make_external_id, make_target_id, PackageId, TargetId};
use rsdeps2obsidian::metadata::ExternalDep;
use rsdeps2obsidian::resolver::{AnalysisInput, Analyzer};
use rsdeps2obsidian::resolver_ts::TsAnalyzer;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ts-resolve")
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
    TsAnalyzer
        .analyze(input)
        .expect("typescript analysis succeeds")
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
    let input = AnalysisInput {
        workspace_root: root.clone(),
        targets: vec![lib_target("web", "web", root.clone())],
        deps: vec![],
    };
    let graph = analyze(&input);
    // tsconfig-relative ids: `index.ts` barrels denote the directory module
    // (no `index` segment), `.mts`/`.cts` strip like `.ts`, `.d.ts` and
    // `node_modules/` never become modules, and the `src/` dir with no index
    // is a file-less namespace module (mirrors Python PEP 420).
    assert_eq!(
        module_ids(&graph),
        vec![
            "web::web",
            "web::web::app",
            "web::web::src",
            "web::web::src::extra",
            "web::web::src::legacy",
            "web::web::src::routes",
            "web::web::src::routes::user",
        ]
    );

    let routes = find_module(&graph, "web::web::src::routes");
    assert!(routes
        .source_path
        .as_ref()
        .unwrap()
        .ends_with("proj/src/routes/index.ts"));
    assert_eq!(routes.name, "routes");

    let user = find_module(&graph, "web::web::src::routes::user");
    assert!(user
        .source_path
        .as_ref()
        .unwrap()
        .ends_with("proj/src/routes/user.ts"));

    // The namespace dir carries no file of its own.
    let src = find_module(&graph, "web::web::src");
    assert_eq!(src.source_path, None);

    // Leaf-free fixture: no imports anywhere, so no diagnostics at all
    // (`.d.ts` and `node_modules/` skips are silent by design).
    assert!(
        graph.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        graph.diagnostics
    );
    assert!(
        module_ids(&graph)
            .iter()
            .all(|id| !id.contains("types") && !id.contains("node_modules")),
        ".d.ts / node_modules leak: {:?}",
        module_ids(&graph)
    );
}

#[test]
fn probe_order_and_mod004() {
    let root = fixture("probe");
    let input = AnalysisInput {
        workspace_root: root.clone(),
        targets: vec![lib_target("web", "web", root.clone())],
        deps: vec![],
    };
    let graph = analyze(&input);

    // `user.ts`, `user.tsx`, and `user/index.ts` are three DISTINCT modules
    // (no MOD003 analog) sharing one id; the probe order hits `user.ts`.
    assert_eq!(
        graph
            .modules
            .iter()
            .filter(|m| m.id.0 == "web::web::user")
            .count(),
        3,
        "three colliding user modules: {:?}",
        module_ids(&graph)
    );
    assert!(
        graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "web::web::main"
                && matches!(&d.to, DependencyTarget::Internal(id) if id.0 == "web::web::user")
                && matches!(d.kind, DependencyKind::Use)),
        "main -> user edge missing: {:?}",
        graph.dependencies
    );

    // Exactly one MOD004 Warning naming the hit (`user.ts`) and the first
    // loser in probe order (`user.tsx`, ahead of `user/index.ts`).
    let mod004: Vec<_> = graph
        .diagnostics
        .iter()
        .filter(|d| d.code == "MOD004")
        .collect();
    assert_eq!(
        mod004.len(),
        1,
        "exactly one MOD004: {:?}",
        graph.diagnostics
    );
    assert!(matches!(mod004[0].severity, DiagnosticSeverity::Warning));
    assert_eq!(
        mod004[0].message,
        format!(
            "ambiguous ts layout: both {} and {} exist; using {}",
            root.join("user.ts").display(),
            root.join("user.tsx").display(),
            root.join("user.ts").display(),
        )
    );

    // A missing relative target is SCOPE001 with the specifier verbatim.
    assert!(
        graph.diagnostics.iter().any(|d| d.code == "SCOPE001"
            && d.message
                == "scope collision: web::web::main -> ./ghost; unresolved syntactic ownership"),
        "relative-miss SCOPE001 missing: {:?}",
        graph.diagnostics
    );

    // `paths` without `baseUrl` resolves against the tsconfig dir (default).
    assert!(
        graph.dependencies.iter().any(|d| d.from.0 == "web::web::main"
            && matches!(&d.to, DependencyTarget::Internal(id) if id.0 == "web::web::mapped::file")
            && matches!(d.kind, DependencyKind::Use)),
        "paths default-baseUrl edge missing: {:?}",
        graph.dependencies
    );
}

fn sem_input(root: &Path) -> AnalysisInput {
    AnalysisInput {
        workspace_root: root.to_path_buf(),
        targets: vec![
            lib_target("web", "web", root.to_path_buf()),
            bin_target("web", "tool", root.join("tool.js")),
        ],
        deps: vec![
            ExternalDep {
                name: "lodash".to_string(),
                version: "4.17.21".to_string(),
            },
            ExternalDep {
                name: "nm-only".to_string(),
                version: "9.9.9".to_string(),
            },
        ],
    }
}

#[test]
fn import_semantics() {
    let root = fixture("sem");
    let graph = analyze(&sem_input(&root));
    let from = "web::web::src::main";

    let has_internal = |from: &str, to: &str| {
        graph.dependencies.iter().any(|d| {
            d.from.0 == from
                && matches!(&d.to, DependencyTarget::Internal(id) if id.0 == to)
                && matches!(d.kind, DependencyKind::Use)
        })
    };
    // `export ... from`, `require('lit')`, dynamic `import('lit')`, and
    // `import type` are all normal `Use` edges to the probed file module.
    for to in ["web::web::src::lib", "web::web::src::tmod"] {
        assert!(has_internal(from, to), "edge {from} -> {to} missing");
    }
    // `require(x)` (non-literal) is silently ignored: no edge, no diagnostic.
    assert!(
        graph
            .diagnostics
            .iter()
            .all(|d| !d.message.contains("-> x;")),
        "non-literal require must stay silent: {:?}",
        graph.diagnostics
    );
    // `export * from './ghost'` misses like any other relative specifier.
    assert!(
        graph.diagnostics.iter().any(|d| d.code == "SCOPE001"
            && d.message
                == format!("scope collision: {from} -> ./ghost; unresolved syntactic ownership")),
        "export-star SCOPE001 missing: {:?}",
        graph.diagnostics
    );
    // `paths` longest-literal-prefix-first: `@/deep/*` beats `@/*`, both
    // resolved against the explicit `baseUrl` (`src/`).
    assert!(
        has_internal(from, "web::web::src::special::mod"),
        "`@/deep/*` edge missing: {:?}",
        graph.dependencies
    );
    assert!(
        has_internal(from, "web::web::src::lib"),
        "`@/*` + baseUrl edge missing: {:?}",
        graph.dependencies
    );
    assert!(
        graph
            .modules
            .iter()
            .all(|m| m.id.0 != "web::web::src::deep::mod"),
        "`@/*` must lose to `@/deep/*`: {:?}",
        module_ids(&graph)
    );
    // Pinned bare specifiers (lockfile and node_modules-only alike) edge to
    // the versioned external.
    for (name, version) in [("lodash", "4.17.21"), ("nm-only", "9.9.9")] {
        let external = make_external_id(name, version);
        assert!(
            graph.dependencies.iter().any(|d| d.from.0 == from
                && matches!(&d.to, DependencyTarget::External(id) if *id == external)),
            "pinned external edge {name}@{version} missing: {:?}",
            graph.dependencies
        );
    }
    // Unpinned bare specifiers are EXT001 with the exact text, attributed to
    // the specifier's own line (the multi-line import points at line 18).
    for expected in [
        format!("unresolved external: {from} -> ghostpkg; no pinned version"),
        format!("unresolved external: {from} -> ghostmulti; no pinned version"),
    ] {
        assert!(
            graph.diagnostics.iter().any(|d| d.code == "EXT001"
                && matches!(d.severity, DiagnosticSeverity::Warning)
                && d.message == expected),
            "missing EXT001 `{expected}`: {:?}",
            graph.diagnostics
        );
    }
    let multi = graph
        .diagnostics
        .iter()
        .find(|d| d.code == "EXT001" && d.message.ends_with("-> ghostmulti; no pinned version"))
        .expect("ghostmulti EXT001");
    let span = multi.source_span.as_ref().expect("import span");
    assert_eq!(span.line_start, 18, "span line for ghostmulti");
    assert!(span.file.ends_with("sem/src/main.ts"), "span file");
    // `node:`-prefixed and core-listed bare specifiers are silently skipped:
    // no edge to any `fs` external, no diagnostic.
    assert!(
        graph
            .dependencies
            .iter()
            .all(|d| !matches!(&d.to, DependencyTarget::External(id) if id.0.starts_with("fs@"))),
        "core skips must have no edge: {:?}",
        graph.dependencies
    );
    assert!(
        graph
            .diagnostics
            .iter()
            .all(|d| !d.message.contains("node:fs") && !d.message.contains("-> fs;")),
        "core skips must stay silent: {:?}",
        graph.diagnostics
    );
    // Own-package bare specifiers resolve against the Lib tree verbatim:
    // `web/other` edges, `web/ghost` is SCOPE001 (never EXT001).
    assert!(
        has_internal(from, "web::web::other"),
        "own-package edge missing: {:?}",
        graph.dependencies
    );
    assert!(
        graph.diagnostics.iter().any(|d| d.code == "SCOPE001"
            && d.message
                == format!("scope collision: {from} -> web/ghost; unresolved syntactic ownership")),
        "own-package ghost SCOPE001 missing: {:?}",
        graph.diagnostics
    );
    // TSX files extract with the TSX grammar: `comp.tsx` edges to `./lib`.
    assert!(
        has_internal("web::web::src::comp", "web::web::src::lib"),
        "tsx edge missing: {:?}",
        graph.dependencies
    );
    // The `Bin` target (a compiled `.js` executable) is a silent root module:
    // registered, never parsed, no diagnostics of its own.
    assert!(
        module_ids(&graph).contains(&"web::tool"),
        "bin module missing: {:?}",
        module_ids(&graph)
    );
    // No MOD004 here (every probe has a single on-disk candidate), and the
    // single-run conditional diagnostic stays Rust-only.
    assert!(
        graph.diagnostics.iter().all(|d| d.code != "MOD004"),
        "no ambiguity expected: {:?}",
        graph.diagnostics
    );
    assert!(
        graph.diagnostics.iter().all(|d| d.code != "CFG001"),
        "no CFG001 for typescript: {:?}",
        graph.diagnostics
    );
}
