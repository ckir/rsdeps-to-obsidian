//! Task 6: ownership resolution, graph build, ordering, cfg.
//!
//! Exercises `SynAnalyzer::analyze` (crate/self/super resolution, external
//! classification, SCOPE001 ambiguity, CFG001 coalescing) plus the total-order
//! helpers `dedup_dependencies`, `sort_graph`, `sort_diagnostics` and
//! `workspace_rel`.

use rsdeps2obsidian::diag::{sort_diagnostics, workspace_rel};
use rsdeps2obsidian::domain::{
    Dependency, DependencyKind, DependencyTarget, Diagnostic, DiagnosticSeverity, Module,
    Package, Target, TargetKind, WorkspaceGraph,
};
use rsdeps2obsidian::graph::{dedup_dependencies, sort_graph};
use rsdeps2obsidian::ids::{make_external_id, make_module_id, make_target_id, ModuleId};
use rsdeps2obsidian::metadata::ExternalDep;
use rsdeps2obsidian::resolver::{AnalysisInput, Analyzer, SynAnalyzer};
use std::path::{Path, PathBuf};

fn tmpdir(test: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rsdeps-task6-{}-{}", test, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn lib_target(pkg: &str, name: &str, root: PathBuf) -> Target {
    Target {
        id: make_target_id(pkg, "lib", name).unwrap(),
        package_id: rsdeps2obsidian::ids::PackageId(pkg.to_string()),
        name: name.to_string(),
        kind: TargetKind::Lib,
        root_source: root,
    }
}

fn input(dir: &Path, target: Target, deps: Vec<ExternalDep>) -> AnalysisInput {
    AnalysisInput {
        workspace_root: dir.to_path_buf(),
        targets: vec![target],
        deps,
    }
}

fn edge_kind(deps: &[Dependency], from: &str, to: &str) -> Option<DependencyKind> {
    deps.iter()
        .find(|d| {
            d.from.0 == from
                && match &d.to {
                    DependencyTarget::Internal(m) => m.0 == to,
                    DependencyTarget::External(e) => e.0 == to,
                }
        })
        .map(|d| d.kind.clone())
}

#[test]
fn self_super_crate_paths_resolve_internally() {
    let dir = tmpdir("self-super-crate");
    write_file(
        &dir.join("lib.rs"),
        "mod foo;\nmod bar;\nuse crate::foo;\nuse self::bar;\n",
    );
    write_file(&dir.join("foo.rs"), "use super::bar;\nuse crate::bar;\n");
    write_file(&dir.join("bar.rs"), "// leaf\n");

    let target = lib_target("t6a", "t6a", dir.join("lib.rs"));
    let graph = SynAnalyzer.analyze(&input(&dir, target, vec![])).unwrap();

    assert_eq!(graph.modules.len(), 3);
    assert!(graph.diagnostics.is_empty(), "{:?}", graph.diagnostics);

    // `mod` edges collapse into the co-located `use` edges via dedup
    // (Module < Use), so every surviving edge is an Internal Use.
    assert_eq!(graph.dependencies.len(), 3);
    assert!(graph
        .dependencies
        .iter()
        .all(|d| matches!(d.to, DependencyTarget::Internal(_)) && d.kind == DependencyKind::Use));
    assert_eq!(
        edge_kind(&graph.dependencies, "t6a::t6a", "t6a::t6a::foo"),
        Some(DependencyKind::Use)
    );
    assert_eq!(
        edge_kind(&graph.dependencies, "t6a::t6a", "t6a::t6a::bar"),
        Some(DependencyKind::Use)
    );
    assert_eq!(
        edge_kind(&graph.dependencies, "t6a::t6a::foo", "t6a::t6a::bar"),
        Some(DependencyKind::Use)
    );

    // Dependencies are totally ordered by `from`.
    let froms: Vec<&str> = graph.dependencies.iter().map(|d| d.from.0.as_str()).collect();
    assert_eq!(froms, vec!["t6a::t6a", "t6a::t6a", "t6a::t6a::foo"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn external_classification_uses_real_name_and_version() {
    let dir = tmpdir("external");
    write_file(
        &dir.join("lib.rs"),
        "use serde::de::Deserializer;\nuse serde::de::Serialize as myserde;\n",
    );

    let target = lib_target("t6b", "t6b", dir.join("lib.rs"));
    let deps = vec![ExternalDep {
        name: "serde".to_string(),
        version: "1.0.210".to_string(),
    }];
    let graph = SynAnalyzer.analyze(&input(&dir, target, deps)).unwrap();

    assert!(graph.diagnostics.is_empty(), "{:?}", graph.diagnostics);
    // Both imports (the `as myserde` alias never mints an ID) collapse to one edge.
    assert_eq!(graph.dependencies.len(), 1);
    let dep = &graph.dependencies[0];
    assert_eq!(dep.from.0, "t6b::t6b");
    assert_eq!(
        dep.to,
        DependencyTarget::External(make_external_id("serde", "1.0.210"))
    );
    assert_eq!(dep.kind, DependencyKind::Use);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ambiguous_ownership_warns_scope001_without_edge() {
    // `foo` is both a top-level module (so `foo::bar` resolves internally to
    // `t6c::t6c::foo::bar`) and an external crate name: inconclusive.
    let dir = tmpdir("ambiguous");
    write_file(&dir.join("lib.rs"), "mod foo;\nuse foo::bar;\n");
    write_file(&dir.join("foo.rs"), "mod bar;\n");
    // rustc 2018 rule (Task 5 round 2): children of file-module `foo.rs`
    // live under the `foo/` subdirectory.
    write_file(&dir.join("foo/bar.rs"), "// leaf\n");

    let target = lib_target("t6c", "t6c", dir.join("lib.rs"));
    let deps = vec![ExternalDep {
        name: "foo".to_string(),
        version: "9.9.9".to_string(),
    }];
    let graph = SynAnalyzer.analyze(&input(&dir, target, deps)).unwrap();

    let scope: Vec<&Diagnostic> =
        graph.diagnostics.iter().filter(|d| d.code == "SCOPE001").collect();
    assert_eq!(scope.len(), 1, "{:?}", graph.diagnostics);
    assert_eq!(scope[0].severity, DiagnosticSeverity::Warning);
    assert!(scope[0].message.contains("scope collision"), "{}", scope[0].message);

    // No edge for the ambiguous path: neither internal nor external.
    assert!(graph
        .dependencies
        .iter()
        .all(|d| !matches!(&d.to, DependencyTarget::External(_))), "{:?}", graph.dependencies);
    assert_eq!(edge_kind(&graph.dependencies, "t6c::t6c", "t6c::t6c::foo::bar"), None);
    // The `mod` containment edges survive.
    assert_eq!(graph.dependencies.len(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn multiversion_dep_warns_exact_scope001_text() {
    // Two resolved versions share the `serde` name, so `use serde::de` is
    // ambiguous: SCOPE001 with the single mandated text, and no edge.
    let dir = tmpdir("multiversion");
    write_file(&dir.join("lib.rs"), "use serde::de;\n");

    let target = lib_target("t6f", "t6f", dir.join("lib.rs"));
    let deps = vec![
        ExternalDep {
            name: "serde".to_string(),
            version: "1.0.190".to_string(),
        },
        ExternalDep {
            name: "serde".to_string(),
            version: "1.0.210".to_string(),
        },
    ];
    let graph = SynAnalyzer.analyze(&input(&dir, target, deps)).unwrap();

    let scope: Vec<&Diagnostic> =
        graph.diagnostics.iter().filter(|d| d.code == "SCOPE001").collect();
    assert_eq!(scope.len(), 1, "{:?}", graph.diagnostics);
    assert_eq!(scope[0].severity, DiagnosticSeverity::Warning);
    assert_eq!(
        scope[0].message,
        "scope collision: t6f::t6f -> serde::de; unresolved syntactic ownership"
    );
    assert!(graph.dependencies.is_empty(), "{:?}", graph.dependencies);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn duplicate_edges_dedup_with_precedence_and_order() {
    let from_a = make_module_id("p", "p", &["a".to_string()]);
    let from_b = make_module_id("p", "p", &["b".to_string()]);
    let to_z = DependencyTarget::Internal(make_module_id("p", "p", &["z".to_string()]));

    // Same (from, to) via all three kinds collapses to the single ReExport winner.
    let dupes = vec![
        Dependency { from: from_a.clone(), to: to_z.clone(), kind: DependencyKind::Module, source_span: None },
        Dependency { from: from_a.clone(), to: to_z.clone(), kind: DependencyKind::Use, source_span: None },
        Dependency { from: from_a.clone(), to: to_z.clone(), kind: DependencyKind::ReExport, source_span: None },
    ];
    let deduped = dedup_dependencies(dupes);
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].kind, DependencyKind::ReExport);

    // Comparator: from asc, then Internal < External, then target string asc,
    // then kind Module < Use < ReExport (dupes kept: sort_graph sorts only).
    let ext_a = DependencyTarget::External(make_external_id("a", "1"));
    let ext_x = DependencyTarget::External(make_external_id("x", "1"));
    let mut graph = WorkspaceGraph {
        packages: vec![Package {
            id: rsdeps2obsidian::ids::PackageId("p".to_string()),
            name: "p".to_string(),
            manifest_path: PathBuf::from("Cargo.toml"),
        }],
        targets: vec![],
        modules: vec![Module {
            id: from_a.clone(),
            target_id: make_target_id("p", "lib", "p").unwrap(),
            name: "a".to_string(),
            qualified_name: "crate::a".to_string(),
            source_path: None,
            source_span: None,
        }],
        dependencies: vec![
            Dependency { from: from_b.clone(), to: to_z.clone(), kind: DependencyKind::Module, source_span: None },
            Dependency { from: from_a.clone(), to: ext_x.clone(), kind: DependencyKind::ReExport, source_span: None },
            Dependency { from: from_a.clone(), to: ext_a.clone(), kind: DependencyKind::Module, source_span: None },
            Dependency { from: from_a.clone(), to: to_z.clone(), kind: DependencyKind::Use, source_span: None },
            Dependency { from: from_a.clone(), to: to_z.clone(), kind: DependencyKind::Module, source_span: None },
        ],
        diagnostics: vec![],
    };
    sort_graph(&mut graph);
    let order: Vec<(String, String, DependencyKind)> = graph
        .dependencies
        .iter()
        .map(|d| {
            let to = match &d.to {
                DependencyTarget::Internal(m) => format!("i:{}", m.0),
                DependencyTarget::External(e) => format!("e:{}", e.0),
            };
            (d.from.0.clone(), to, d.kind.clone())
        })
        .collect();
    assert_eq!(
        order,
        vec![
            (from_a.0.clone(), format!("i:{}", "p::p::z"), DependencyKind::Module),
            (from_a.0.clone(), format!("i:{}", "p::p::z"), DependencyKind::Use),
            (from_a.0.clone(), "e:a@1".to_string(), DependencyKind::Module),
            (from_a.0.clone(), "e:x@1".to_string(), DependencyKind::ReExport),
            (from_b.0.clone(), format!("i:{}", "p::p::z"), DependencyKind::Module),
        ]
    );
}

#[test]
fn cfg_emits_exactly_one_warning() {
    let dir = tmpdir("cfg");
    write_file(
        &dir.join("lib.rs"),
        "#[cfg(feature = \"x\")]\nmod foo;\n#[cfg(unix)]\nuse crate::foo;\n#[cfg(windows)]\nmod inline_mod {\n}\n",
    );
    write_file(&dir.join("foo.rs"), "// leaf\n");

    let target = lib_target("t6e", "t6e", dir.join("lib.rs"));
    let graph = SynAnalyzer.analyze(&input(&dir, target, vec![])).unwrap();

    let cfg: Vec<&Diagnostic> =
        graph.diagnostics.iter().filter(|d| d.code == "CFG001").collect();
    assert_eq!(cfg.len(), 1, "{:?}", graph.diagnostics);
    assert_eq!(cfg[0].severity, DiagnosticSeverity::Warning);
    assert_eq!(
        cfg[0].message,
        "conditional compilation detected - graph reflects unexpanded source"
    );
    // No evaluation: the cfg'd `use` still resolves against unexpanded source.
    assert_eq!(
        edge_kind(&graph.dependencies, "t6e::t6e", "t6e::t6e::foo"),
        Some(DependencyKind::Use)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn diag(
    severity: DiagnosticSeverity,
    code: &str,
    file: Option<(&str, u32)>,
    message: &str,
) -> Diagnostic {
    Diagnostic {
        severity,
        code: code.to_string(),
        message: message.to_string(),
        source_span: file.map(|(f, line)| rsdeps2obsidian::domain::SourceSpan {
            file: PathBuf::from(f),
            line_start: line,
            line_end: line,
        }),
    }
}

#[test]
fn diagnostics_sort_total_order() {
    // workspace_rel: `/` separators, root-relative, no `.`/`..`.
    assert_eq!(workspace_rel(Path::new("/ws/src/foo.rs"), Path::new("/ws")), "src/foo.rs");
    assert_eq!(workspace_rel(Path::new("/ws/a/../src/./foo.rs"), Path::new("/ws")), "src/foo.rs");
    assert_eq!(workspace_rel(Path::new("/other/x.rs"), Path::new("/ws")), "/other/x.rs");

    use DiagnosticSeverity::{Error, Info, Warning};
    // Missing line sorts before present lines within the same severity+code.
    let mut diags = vec![
        diag(Error, "B", Some(("b.rs", 5)), "m"),
        diag(Warning, "B", Some(("a.rs", 1)), "m"),
        diag(Warning, "A", Some(("b.rs", 1)), "a"),
        diag(Info, "Z", None, "m"),
        diag(Warning, "A", Some(("a.rs", 3)), "m"),
        diag(Info, "A", Some(("a.rs", 1)), "b"),
        diag(Warning, "A", None, "m-missing-line-first"),
        diag(Info, "A", Some(("a.rs", 1)), "a"),
    ];
    sort_diagnostics(&mut diags);

    let key = |d: &Diagnostic| {
        (
            format!("{:?}", d.severity),
            d.code.clone(),
            d.source_span.as_ref().map(|s| s.file.display().to_string()),
            d.source_span.as_ref().map(|s| s.line_start),
            d.message.clone(),
        )
    };
    assert_eq!(
        diags.iter().map(key).collect::<Vec<_>>(),
        vec![
            ("Info".to_string(), "A".to_string(), Some("a.rs".to_string()), Some(1), "a".to_string()),
            ("Info".to_string(), "A".to_string(), Some("a.rs".to_string()), Some(1), "b".to_string()),
            ("Info".to_string(), "Z".to_string(), None, None, "m".to_string()),
            ("Warning".to_string(), "A".to_string(), None, None, "m-missing-line-first".to_string()),
            ("Warning".to_string(), "A".to_string(), Some("a.rs".to_string()), Some(3), "m".to_string()),
            ("Warning".to_string(), "A".to_string(), Some("b.rs".to_string()), Some(1), "a".to_string()),
            ("Warning".to_string(), "B".to_string(), Some("a.rs".to_string()), Some(1), "m".to_string()),
            ("Error".to_string(), "B".to_string(), Some("b.rs".to_string()), Some(5), "m".to_string()),
        ]
    );

    // Unused import guard: ModuleId is part of the graph surface used above.
    let _ = ModuleId("p::p::m".to_string());
}
