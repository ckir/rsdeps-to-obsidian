//! Task 3: C# module and import resolution (`CsAnalyzer`).
//!
//! Exercises `CsAnalyzer::analyze` over `tests/fixtures/cs-resolve/`:
//! namespace module discovery and `ModuleId` shape (block + file-scoped
//! namespaces, partial-merge to one node at the first sorted file, two
//! namespaces sharing one file, namespace-less top-level fallback to
//! path segments + stem), `using` extraction (plain, static full-path,
//! alias full-path with the alias never minting an ID, `global using` as
//! project-level context), longest-prefix resolution (own namespaces,
//! one-hop `ProjectReference` namespaces with cross-project ambiguity as
//! `SCOPE001`, `PackageReference` pins as `External` with unpinned names as
//! `EXT001`, BCL `System*` silent skip consulted before externals), the
//! single run-level `CFG001` for `#if`, and legacy `Compile`-list walks.

use rsdeps2obsidian::domain::{
    DependencyKind, DependencyTarget, DiagnosticSeverity, Target, TargetKind,
};
use rsdeps2obsidian::ids::{make_external_id, make_target_id, PackageId};
use rsdeps2obsidian::metadata::ExternalDep;
use rsdeps2obsidian::resolver::{AnalysisInput, Analyzer};
use rsdeps2obsidian::resolver_cs::CsAnalyzer;
use std::path::{Path, PathBuf};

fn resolve_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cs-resolve");
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
        id: make_target_id(pkg, "bin", name).unwrap(),
        package_id: PackageId(pkg.to_string()),
        name: name.to_string(),
        kind: TargetKind::Bin,
        root_source: root,
    }
}

fn analyze(input: &AnalysisInput) -> rsdeps2obsidian::domain::WorkspaceGraph {
    CsAnalyzer.analyze(input).expect("csharp analysis succeeds")
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

fn has_use_edge(graph: &rsdeps2obsidian::domain::WorkspaceGraph, from: &str, to: &str) -> bool {
    graph.dependencies.iter().any(|d| {
        d.from.0 == from
            && matches!(&d.to, DependencyTarget::Internal(id) if id.0 == to)
            && matches!(d.kind, DependencyKind::Use)
    })
}

#[test]
fn modules_namespaces_and_partials() {
    // Block namespaces, file-scoped namespaces, two partial files sharing
    // one namespace (one node, first-sorted path), two namespaces in one
    // file (two nodes, shared path), and a namespace-less top-level
    // `Program.cs` (stem module) map to exact `ModuleId`s.
    let dir = resolve_root().join("mods");
    let graph = analyze(&AnalysisInput {
        workspace_root: dir.clone(),
        targets: vec![lib_target("shop", "shop", dir)],
        deps: vec![],
    });
    assert_eq!(
        module_ids(&graph),
        vec![
            "shop::shop",
            "shop::shop::Program",
            "shop::shop::Shop::A",
            "shop::shop::Shop::B",
            "shop::shop::Shop::Partial",
            "shop::shop::Shop::Scoped",
            "shop::shop::Shop::Services",
        ]
    );

    let block = find_module(&graph, "shop::shop::Shop::Services");
    assert_eq!(block.qualified_name, "Shop.Services");
    assert_eq!(block.name, "Services");
    assert!(block
        .source_path
        .as_ref()
        .unwrap()
        .ends_with("mods/Block.cs"));

    let scoped = find_module(&graph, "shop::shop::Shop::Scoped");
    assert_eq!(scoped.qualified_name, "Shop.Scoped");

    // The partial pair merges: one node pointing at the first sorted file.
    let partial = find_module(&graph, "shop::shop::Shop::Partial");
    assert!(
        partial
            .source_path
            .as_ref()
            .unwrap()
            .ends_with("mods/Part1.cs"),
        "partial merge keeps the first sorted file: {:?}",
        partial.source_path
    );

    // Two namespaces in one file are distinct nodes sharing the file, with
    // distinct declaration spans.
    let a = find_module(&graph, "shop::shop::Shop::A");
    let b = find_module(&graph, "shop::shop::Shop::B");
    assert_eq!(a.source_path, b.source_path);
    assert!(a.source_path.as_ref().unwrap().ends_with("mods/Two.cs"));
    assert_ne!(
        a.source_span, b.source_span,
        "shared-file modules keep per-declaration spans"
    );

    // Namespace-less top-level statements fall back to the stem module.
    let program = find_module(&graph, "shop::shop::Program");
    assert_eq!(program.qualified_name, "Program");
    assert!(program
        .source_path
        .as_ref()
        .unwrap()
        .ends_with("mods/Program.cs"));

    // No usings anywhere: every dependency is a `Module`-kind containment
    // edge and diagnostics stay empty.
    assert!(
        graph
            .dependencies
            .iter()
            .all(|d| matches!(d.kind, DependencyKind::Module)),
        "usings-free fixture has only containment edges: {:?}",
        graph.dependencies
    );
    assert_eq!(graph.dependencies.len(), 6);
    assert!(graph.diagnostics.is_empty());
}

fn sem_input(root: &Path) -> AnalysisInput {
    AnalysisInput {
        workspace_root: root.to_path_buf(),
        targets: vec![
            lib_target("app", "app", root.join("sem")),
            lib_target("reflib", "reflib", root.join("reflib")),
        ],
        // `Newtonsoft.Json` + `System.Text.Json` are exact pins (as discovery
        // would record them); `Floaty` floats (`*`), so it yields no entry
        // and resolves to the unresolved diagnostic downstream.
        deps: vec![
            ExternalDep {
                name: "Newtonsoft.Json".to_string(),
                version: "13.0.3".to_string(),
            },
            ExternalDep {
                name: "System.Text.Json".to_string(),
                version: "8.0.0".to_string(),
            },
        ],
    }
}

#[test]
fn import_semantics() {
    // Plain own-namespace usings, static full-path usings, alias full-path
    // usings, cross-project usings, pinned externals, and the silent/ext
    // arms resolve exactly; `#if` yields one run-level diagnostic.
    let root = resolve_root();
    let graph = analyze(&sem_input(&root));
    let utils = "app::app::App::Utils";

    // `using` of an own namespace is `Internal`.
    assert!(
        has_use_edge(&graph, "app::app::App", utils),
        "own-namespace edge missing: {:?}",
        graph.dependencies
    );
    // `using static` resolves by its full type path through the same order
    // (longest prefix `App.Utils` names the known module).
    assert!(
        has_use_edge(&graph, "app::app::App::Static", utils),
        "using-static edge missing: {:?}",
        graph.dependencies
    );
    // Aliases resolve by full path, and the alias never mints an ID.
    assert!(
        has_use_edge(&graph, "app::app::App::Alias", utils),
        "alias edge missing: {:?}",
        graph.dependencies
    );
    assert!(
        graph
            .modules
            .iter()
            .map(|m| &m.id.0)
            .all(|id| !id.ends_with("::H"))
            && graph.dependencies.iter().all(|d| match &d.to {
                DependencyTarget::Internal(id) => !id.0.ends_with("::H"),
                DependencyTarget::External(_) => true,
            }),
        "alias must never mint an ID: {:?} {:?}",
        module_ids(&graph),
        graph.dependencies
    );
    // The `global using` in `Global.cs` is gated to silent: the bare
    // `Consumer.cs` (no directive of its own) gains NO edge from it, and the
    // global yields NO edge from its own file's module either.
    for from in ["app::app::App::Consumer", "app::app::App::Glob"] {
        assert!(
            !graph
                .dependencies
                .iter()
                .any(|d| d.from.0 == from && matches!(d.kind, DependencyKind::Use)),
            "global must stay silent for {from}: {:?}",
            graph.dependencies
        );
    }
    // With the globals present-but-inert, an explicit `using` of the same
    // namespace still resolves normally.
    assert!(
        has_use_edge(&graph, "app::app::App::Explicit", utils),
        "explicit edge alongside inert globals missing: {:?}",
        graph.dependencies
    );
    // Explicit `System.*` is BCL-silent: no `Use` edge at all from `Sys`.
    assert!(
        !graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "app::app::App::Sys" && matches!(d.kind, DependencyKind::Use)),
        "BCL using must stay silent: {:?}",
        graph.dependencies
    );
    // A pinned `System.*` package still loses to the BCL skip (consulted
    // before external lookup): no `System` `External` edge and no `Use`
    // edge at all from `JsonMod`.
    assert!(
        !graph
            .dependencies
            .iter()
            .any(|d| matches!(&d.to, DependencyTarget::External(id) if id.0.starts_with("System"))),
        "BCL skip beats the pinned external: {:?}",
        graph.dependencies
    );
    assert!(
        !graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "app::app::App::JsonMod" && matches!(d.kind, DependencyKind::Use)),
        "BCL using must stay silent: {:?}",
        graph.dependencies
    );
    // A pinned NuGet name is an `External` edge ...
    let external = make_external_id("Newtonsoft.Json", "13.0.3");
    assert!(
        graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "app::app::App::Nuget"
                && matches!(&d.to, DependencyTarget::External(id) if *id == external)
                && matches!(d.kind, DependencyKind::Use)),
        "pinned external edge missing: {:?}",
        graph.dependencies
    );
    // ... while the floating version is the unresolved diagnostic verbatim.
    assert!(
        graph.diagnostics.iter().any(|d| d.code == "EXT001"
            && matches!(d.severity, DiagnosticSeverity::Warning)
            && d.message
                == "unresolved external: app::app::App::Nuget -> Floaty.Thing; no pinned version"),
        "floating-version EXT001 missing: {:?}",
        graph.diagnostics
    );
    // A namespaced import of a directly-referenced project is `Internal`.
    assert!(
        has_use_edge(
            &graph,
            "app::app::App::Cross",
            "reflib::reflib::RefLib::Api"
        ),
        "cross-project edge missing: {:?}",
        graph.dependencies
    );
    // `#if` is parsed inclusively with no evaluation: exactly one run-level
    // `CFG001` for the whole run.
    let cfg001: Vec<_> = graph
        .diagnostics
        .iter()
        .filter(|d| d.code == "CFG001")
        .collect();
    assert_eq!(
        cfg001.len(),
        1,
        "exactly one CFG001: {:?}",
        graph.diagnostics
    );
    assert!(matches!(cfg001[0].severity, DiagnosticSeverity::Warning));
    assert_eq!(
        cfg001[0].message,
        "conditional compilation detected - graph reflects unexpanded source"
    );
    assert_eq!(cfg001[0].source_span, None);
    // Nothing `System`-flavored is an edge or a diagnostic.
    assert!(
        !graph.dependencies.iter().any(|d| match &d.to {
            DependencyTarget::Internal(id) => id.0.contains("System"),
            DependencyTarget::External(id) => id.0.starts_with("System"),
        }) && !graph
            .diagnostics
            .iter()
            .any(|d| d.message.contains("System")),
        "System.* must be fully silent: {:?} {:?}",
        graph.dependencies,
        graph.diagnostics
    );
}

#[test]
fn review_focus_pins() {
    // The same namespace in two referenced projects is ambiguity (no edge);
    // an unknown `using static` path is `SCOPE001`; a legacy `Compile`
    // list walks exactly the listed files.
    let root = resolve_root();
    let graph = analyze(&AnalysisInput {
        workspace_root: root.clone(),
        targets: vec![
            lib_target("alpha", "alpha", root.join("review/alpha")),
            lib_target("beta", "beta", root.join("review/beta")),
            lib_target("gamma", "gamma", root.join("review/gamma")),
        ],
        deps: vec![],
    });
    // Same-namespace-two-projects ambiguity: the exact text, no edge.
    assert!(
        graph.diagnostics.iter().any(|d| d.code == "SCOPE001"
            && d.message
                == "scope collision: alpha::alpha::Alpha -> Shared.Thing; unresolved syntactic ownership"),
        "ambiguity SCOPE001 missing: {:?}",
        graph.diagnostics
    );
    assert!(
        !graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "alpha::alpha::Alpha" && matches!(d.kind, DependencyKind::Use)),
        "ambiguous using must yield no Use edge: {:?}",
        graph.dependencies
    );
    // Unknown `using static` paths are `SCOPE001` with the full type path.
    assert!(
        graph.diagnostics.iter().any(|d| d.code == "SCOPE001"
            && d.message
                == "scope collision: alpha::alpha::Alpha::Other -> Bogus.Type; unresolved syntactic ownership"),
        "static-unknown SCOPE001 missing: {:?}",
        graph.diagnostics
    );

    // Legacy explicit `Compile` lists walk exactly those files: 2-of-3.
    let legacy = analyze(&AnalysisInput {
        workspace_root: root.clone(),
        targets: vec![lib_target("old", "old", root.join("legacy"))],
        deps: vec![],
    });
    assert_eq!(
        module_ids(&legacy),
        vec!["old::old", "old::old::Old::A", "old::old::Old::B"]
    );
}

#[test]
fn bin_missing_root_fails_closed() {
    // A `Bin` target whose root is gone still registers its root module;
    // analysis continues with an IO-family Error diagnostic.
    let root = resolve_root();
    let missing = root.join("no-such-dir");
    let graph = analyze(&AnalysisInput {
        workspace_root: root,
        targets: vec![Target {
            id: make_target_id("tool", "bin", "tool").unwrap(),
            package_id: PackageId("tool".to_string()),
            name: "tool".to_string(),
            kind: TargetKind::Bin,
            root_source: missing,
        }],
        deps: vec![],
    });
    assert_eq!(module_ids(&graph), vec!["tool::tool"]);
    assert!(
        graph
            .diagnostics
            .iter()
            .any(|d| d.code == "IO001" && matches!(d.severity, DiagnosticSeverity::Error)),
        "missing-root IO001 missing: {:?}",
        graph.diagnostics
    );
}

#[test]
fn same_package_fallback_edges() {
    // Same-package full-path fallback (no package prefix is ever stripped):
    // a `Bin` file's `using OwnPkg.Mod;` names the `Lib` tree's module, and
    // a second `Lib` target's file resolves the same way (uniform rule,
    // kind-blind). Exactly one `Internal` edge each, zero diagnostics.
    let root = resolve_root();
    let dir = root.join("fallback");
    let graph = analyze(&AnalysisInput {
        workspace_root: dir.clone(),
        targets: vec![
            lib_target("own", "own", dir.join("lib")),
            lib_target("own", "second", dir.join("libb")),
            bin_target("own", "tool", dir.join("tool")),
        ],
        deps: vec![],
    });
    let lib_mod = "own::own::OwnPkg::Mod";
    assert!(
        has_use_edge(&graph, "own::tool::OwnPkg::Tool", lib_mod),
        "bin->lib fallback edge missing: {:?}",
        graph.dependencies
    );
    assert!(
        has_use_edge(&graph, "own::second::OwnPkg::Other", lib_mod),
        "lib->lib fallback edge missing: {:?}",
        graph.dependencies
    );
    assert!(
        graph.diagnostics.is_empty(),
        "fallback edges carry no diagnostics: {:?}",
        graph.diagnostics
    );
}

#[test]
fn no_top_level_hijack() {
    // Package `Web` declares both top-level `Models` and `Web.Models`; a
    // `Bin` file's `using Web.Models;` must edge to the `Lib` `Web.Models`
    // module — never to top-level `Models` (the stripped-remainder hijack
    // the fallback replaced).
    let root = resolve_root();
    let dir = root.join("hijack");
    let graph = analyze(&AnalysisInput {
        workspace_root: dir.clone(),
        targets: vec![
            lib_target("Web", "Web", dir.join("lib")),
            bin_target("Web", "tool", dir.join("tool")),
        ],
        deps: vec![],
    });
    assert!(
        has_use_edge(&graph, "Web::tool::Web::Tool", "Web::Web::Web::Models"),
        "fallback edge to Web.Models missing: {:?}",
        graph.dependencies
    );
    assert!(
        !graph
            .dependencies
            .iter()
            .any(|d| d.from.0 == "Web::tool::Web::Tool"
                && matches!(&d.to, DependencyTarget::Internal(id) if id.0 == "Web::Web::Models")),
        "top-level hijack edge must not exist: {:?}",
        graph.dependencies
    );
    assert!(
        graph.diagnostics.is_empty(),
        "hijack-free resolution carries no diagnostics: {:?}",
        graph.diagnostics
    );
}
