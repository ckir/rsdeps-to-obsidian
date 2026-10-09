//! Task 7: Markdown notes, graph JSON, and manifest rendering.

use rsdeps2obsidian::domain::{
    Dependency, DependencyKind, DependencyTarget, Diagnostic, DiagnosticSeverity, Module, Package,
    SourceSpan, Target, TargetKind, WorkspaceGraph,
};
use rsdeps2obsidian::ids::{make_external_id, make_target_id, ModuleId, PackageId};
use rsdeps2obsidian::manifest::{build_manifest, read_manifest};
use rsdeps2obsidian::render::{render_all, render_graph_json};
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from("/tmp/rsdeps-task7-ws")
}

fn fixture_graph() -> (WorkspaceGraph, PathBuf) {
    let root = workspace_root();
    let package_id = PackageId("api".to_string());
    let target_id = make_target_id("api", "lib", "api").unwrap();
    let target = Target {
        id: target_id.clone(),
        package_id: package_id.clone(),
        name: "api".to_string(),
        kind: TargetKind::Lib,
        root_source: root.join("crates/api/src/lib.rs"),
    };
    let module = |id: &str, file: &str| Module {
        id: ModuleId(id.to_string()),
        target_id: target_id.clone(),
        name: id.rsplit("::").next().unwrap_or(id).to_string(),
        qualified_name: id.to_string(),
        source_path: Some(root.join(file)),
        source_span: None,
    };
    let modules = vec![
        module("api::api", "crates/api/src/lib.rs"),
        module("api::api::handler", "crates/api/src/handler.rs"),
        module("api::api::model", "crates/api/src/model.rs"),
    ];
    let dependencies = vec![
        Dependency {
            from: ModuleId("api::api::handler".to_string()),
            to: DependencyTarget::Internal(ModuleId("api::api::model".to_string())),
            kind: DependencyKind::Use,
            source_span: None,
        },
        Dependency {
            from: ModuleId("api::api::handler".to_string()),
            to: DependencyTarget::External(make_external_id("serde", "1.0.205")),
            kind: DependencyKind::Use,
            source_span: None,
        },
        Dependency {
            from: ModuleId("api::api::model".to_string()),
            to: DependencyTarget::Internal(ModuleId("api::api::handler".to_string())),
            kind: DependencyKind::Module,
            source_span: None,
        },
    ];
    let diagnostics = vec![Diagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "MOD002".to_string(),
        message: "ambiguous module layout".to_string(),
        source_span: Some(SourceSpan {
            file: root.join("crates/api/src/handler.rs"),
            line_start: 1,
            line_end: 1,
        }),
    }];
    let graph = WorkspaceGraph {
        packages: vec![Package {
            id: package_id,
            name: "api".to_string(),
            manifest_path: root.join("crates/api/Cargo.toml"),
        }],
        targets: vec![target],
        modules,
        dependencies,
        diagnostics,
    };
    (graph, root)
}

#[test]
fn module_note_frontmatter_and_links() {
    let (graph, root) = fixture_graph();
    let rendered = render_all(&graph, &root, false);
    let handler = rendered
        .iter()
        .find(|f| f.rel_path == "api__api__handler.md")
        .expect("handler note missing");
    for key in [
        "module_id: api::api::handler",
        "target_id: api::lib::api",
        "package_id: api",
        "source: crates/api/src/handler.rs",
    ] {
        assert!(
            handler.content.contains(key),
            "handler note missing {key:?}:\n{}",
            handler.content
        );
    }
    assert!(
        handler
            .content
            .contains("[[api__api__model|api::api::model]]"),
        "handler Depends-on link missing:\n{}",
        handler.content
    );
    // Fixed frontmatter field order.
    let order = [
        "graph_type:",
        "protocol_version:",
        "spec_version:",
        "schema_version:",
        "generator:",
        "module_id:",
        "target_id:",
        "package_id:",
        "source:",
    ];
    let mut last = 0;
    for key in order {
        let pos = handler
            .content
            .find(key)
            .unwrap_or_else(|| panic!("handler note missing {key:?}"));
        assert!(pos >= last, "frontmatter field {key:?} out of order");
        last = pos;
    }
    // Internal dependency sorts before the external one.
    let internal = handler
        .content
        .find("[[api__api__model|api::api::model]]")
        .unwrap();
    let external = handler.content.find("serde@1.0.205").unwrap();
    assert!(
        internal < external,
        "internal link must precede external link"
    );
}

#[test]
fn every_note_carries_generator_marker_and_handshake() {
    let (graph, root) = fixture_graph();
    let rendered = render_all(&graph, &root, true);
    assert!(!rendered.is_empty());
    for file in &rendered {
        assert!(
            file.rel_path.ends_with(".md"),
            "unexpected non-markdown file {}",
            file.rel_path
        );
        for key in [
            "generator: cargo-obsidian-graph",
            "protocol_version: 1",
            "0.4.0",
            "schema_version: 3",
        ] {
            assert!(
                file.content.contains(key),
                "{} missing {key:?}:\n{}",
                file.rel_path,
                file.content
            );
        }
    }
    assert!(
        rendered.iter().any(|f| f.rel_path == "index.md"),
        "root index.md missing"
    );
}

#[test]
fn external_markdown_gated_but_json_retained() {
    let (graph, root) = fixture_graph();
    let without = render_all(&graph, &root, false);
    assert!(
        !without
            .iter()
            .any(|f| f.content.contains("rust-external-crate")),
        "external=false must emit no external notes"
    );
    let json = render_graph_json(&graph);
    assert!(
        json.contains("serde@1.0.205"),
        "JSON must retain externals regardless of flag:\n{json}"
    );
    let with = render_all(&graph, &root, true);
    let external = with
        .iter()
        .find(|f| f.content.contains("graph_type: rust-external-crate"))
        .expect("external=true must emit an external note");
    assert!(
        external
            .content
            .contains("external_crate_id: serde@1.0.205"),
        "external note missing crate id:\n{}",
        external.content
    );
}

#[test]
fn manifest_shape_and_determinism() {
    let (graph, root) = fixture_graph();
    let first = render_all(&graph, &root, true);
    let second = render_all(&graph, &root, true);
    let flat = |files: &[rsdeps2obsidian::render::RenderedFile]| {
        files
            .iter()
            .map(|f| format!("{}\n{}", f.rel_path, f.content))
            .collect::<Vec<_>>()
    };
    assert_eq!(flat(&first), flat(&second), "render must be byte-identical");

    let manifest = build_manifest(&first, &graph);
    assert_eq!(manifest.protocol_version, 1);
    assert_eq!(manifest.spec_version, "0.4.0");
    assert_eq!(manifest.schema_version, 3);
    assert_eq!(manifest.generator, "cargo-obsidian-graph");
    let mut sorted = manifest.generated_files.clone();
    sorted.sort();
    assert_eq!(manifest.generated_files, sorted, "generated_files unsorted");
    let ids: Vec<&str> = manifest
        .module_records
        .iter()
        .map(|r| r.module_id.as_str())
        .collect();
    let mut sorted_ids = ids.clone();
    sorted_ids.sort();
    assert_eq!(ids, sorted_ids, "module_records unsorted");
    assert_eq!(manifest.module_records.len(), 3);

    let json = render_graph_json(&graph);
    assert!(json.ends_with('\n'), "JSON must end with a newline");
    assert_eq!(
        json,
        render_graph_json(&graph),
        "JSON must be deterministic"
    );

    // Manifest round-trip through a temp dir.
    let dir = std::env::temp_dir().join(format!("rsdeps-task7-manifest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("manifest.json");
    assert_eq!(read_manifest(&path).unwrap(), None);
    std::fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    assert_eq!(read_manifest(&path).unwrap(), Some(manifest));
    std::fs::write(&path, "{not json").unwrap();
    assert!(read_manifest(&path).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Shared-ModuleId regression (lib + bin roots share a name, spec-legal):
/// two modules with the IDENTICAL ModuleId string but distinct target_ids
/// must get distinct filenames with correct per-target frontmatter, and a
/// link to the shared id resolves to the first file in module sort order.
fn shared_id_graph() -> (WorkspaceGraph, PathBuf) {
    let root = workspace_root();
    let package_id = PackageId("pkg".to_string());
    let lib_target = make_target_id("pkg", "lib", "pkg").unwrap();
    let bin_target = make_target_id("pkg", "bin", "pkg").unwrap();
    let targets = vec![
        Target {
            id: bin_target.clone(),
            package_id: package_id.clone(),
            name: "pkg".to_string(),
            kind: TargetKind::Bin,
            root_source: root.join("src/main.rs"),
        },
        Target {
            id: lib_target.clone(),
            package_id: package_id.clone(),
            name: "pkg".to_string(),
            kind: TargetKind::Lib,
            root_source: root.join("src/lib.rs"),
        },
    ];
    let modules = vec![
        Module {
            id: ModuleId("pkg::pkg".to_string()),
            target_id: lib_target.clone(),
            name: "pkg".to_string(),
            qualified_name: "pkg::pkg".to_string(),
            source_path: Some(root.join("src/lib.rs")),
            source_span: None,
        },
        Module {
            id: ModuleId("pkg::pkg".to_string()),
            target_id: bin_target.clone(),
            name: "pkg".to_string(),
            qualified_name: "pkg::pkg".to_string(),
            source_path: Some(root.join("src/main.rs")),
            source_span: None,
        },
        Module {
            id: ModuleId("pkg::pkg::user".to_string()),
            target_id: lib_target.clone(),
            name: "user".to_string(),
            qualified_name: "pkg::pkg::user".to_string(),
            source_path: Some(root.join("src/user.rs")),
            source_span: None,
        },
    ];
    let dependencies = vec![Dependency {
        from: ModuleId("pkg::pkg::user".to_string()),
        to: DependencyTarget::Internal(ModuleId("pkg::pkg".to_string())),
        kind: DependencyKind::Use,
        source_span: None,
    }];
    let graph = WorkspaceGraph {
        packages: vec![Package {
            id: package_id,
            name: "pkg".to_string(),
            manifest_path: root.join("Cargo.toml"),
        }],
        targets,
        modules,
        dependencies,
        diagnostics: vec![],
    };
    (graph, root)
}

#[test]
fn shared_module_id_gets_distinct_filenames() {
    let (graph, root) = shared_id_graph();
    let rendered = render_all(&graph, &root, false);
    // 2 shared-id roots + 1 dependent + index.
    assert_eq!(rendered.len(), 4, "rendered: {rendered:?}");
    let roots: Vec<_> = rendered
        .iter()
        .filter(|f| f.content.contains("module_id: pkg::pkg\n"))
        .collect();
    assert_eq!(roots.len(), 2, "both roots must be emitted: {rendered:?}");
    assert_ne!(
        roots[0].rel_path, roots[1].rel_path,
        "distinct (target_id, module_id) identities need distinct filenames"
    );
    // ModuleId strings are frozen (§9.3): both notes carry the same id…
    for r in &roots {
        assert!(r.content.contains("module_id: pkg::pkg\n"));
    }
    // …but per-target frontmatter distinguishes them.
    let targets: Vec<_> = roots
        .iter()
        .map(|r| {
            r.content
                .lines()
                .find(|l| l.starts_with("target_id: "))
                .unwrap_or("target_id: MISSING")
                .to_string()
        })
        .collect();
    assert!(
        targets.contains(&"target_id: pkg::lib::pkg".to_string()),
        "{targets:?}"
    );
    assert!(
        targets.contains(&"target_id: pkg::bin::pkg".to_string()),
        "{targets:?}"
    );
    // Link to the shared id resolves to the first file in module sort order
    // ((module_id, target_id): the bin root sorts before the lib root).
    let user = rendered
        .iter()
        .find(|f| f.content.contains("module_id: pkg::pkg::user\n"))
        .expect("dependent note missing");
    let first = roots
        .iter()
        .find(|r| r.content.contains("target_id: pkg::bin::pkg\n"))
        .expect("bin root note missing");
    let expected = format!(
        "[[{}|pkg::pkg]]",
        first
            .rel_path
            .strip_suffix(".md")
            .unwrap_or(&first.rel_path)
    );
    assert!(
        user.content.contains(&expected),
        "link must resolve to first in sort order ({expected}):\n{}",
        user.content
    );
}

#[test]
fn shared_module_id_manifest_has_no_duplicates() {
    let (graph, root) = shared_id_graph();
    let rendered = render_all(&graph, &root, false);
    let manifest = build_manifest(&rendered, &graph);
    let mut rels: Vec<&str> = rendered.iter().map(|f| f.rel_path.as_str()).collect();
    rels.sort();
    rels.dedup();
    assert_eq!(
        rels.len(),
        rendered.len(),
        "generated_files must be duplicate-free: {rendered:?}"
    );
    assert_eq!(manifest.generated_files.len(), rendered.len());
    let shared: Vec<_> = manifest
        .module_records
        .iter()
        .filter(|r| r.module_id == "pkg::pkg")
        .collect();
    assert_eq!(
        shared.len(),
        2,
        "one record per module: {:?}",
        manifest.module_records
    );
    assert_ne!(
        shared[0].filename, shared[1].filename,
        "records sharing module_id must carry distinct filenames"
    );
}
