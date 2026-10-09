use rsdeps2obsidian::domain::{
    Dependency, DependencyKind, DependencyTarget, Diagnostic, DiagnosticSeverity, Module, Package,
    SourceSpan, Target, TargetKind, WorkspaceGraph,
};
use rsdeps2obsidian::ids::{make_external_id, make_module_id, make_target_id};
use std::collections::HashSet;
use std::path::PathBuf;

#[test]
fn target_ids_use_lib_and_bin_tokens() {
    assert_eq!(
        make_target_id("api", "lib", "api").unwrap().0,
        "api::lib::api"
    );
    assert_eq!(
        make_target_id("cli", "bin", "cli").unwrap().0,
        "cli::bin::cli"
    );
}

#[test]
fn target_id_rejects_double_colon_component() {
    assert!(make_target_id("a::b", "lib", "t").is_err());
    assert!(make_target_id("pkg", "lib", "x::y").is_err());
}

#[test]
fn package_names_preserved_verbatim() {
    assert_eq!(
        make_target_id("my-pkg", "lib", "my-pkg").unwrap().0,
        "my-pkg::lib::my-pkg"
    );
    assert_eq!(
        make_target_id("Foo", "bin", "Foo").unwrap().0,
        "Foo::bin::Foo"
    );
}

#[test]
fn external_ids_distinguish_versions() {
    assert_ne!(
        make_external_id("serde", "1.0.210"),
        make_external_id("serde", "1.0.190")
    );
}

#[test]
fn module_id_shape() {
    assert_eq!(
        make_module_id("api", "api", &["handler".to_string()]).0,
        "api::api::handler"
    );
}

#[test]
fn id_newtypes_order_and_hash() {
    let mut ids = [
        make_module_id("b", "b", &["z".to_string()]),
        make_module_id("a", "a", &["a".to_string()]),
    ];
    ids.sort();
    assert_eq!(ids[0].0, "a::a::a");
    assert_eq!(ids[1].0, "b::b::z");

    let mut set = HashSet::new();
    set.insert(make_external_id("serde", "1.0.210"));
    set.insert(make_external_id("serde", "1.0.210"));
    assert_eq!(set.len(), 1);
}

// Compile-time shape checks: touch every domain type/field so later
// renames break loudly here.
#[test]
fn domain_model_shapes_compile() {
    let span = SourceSpan {
        file: PathBuf::from("src/main.rs"),
        line_start: 1,
        line_end: 2,
    };
    assert_eq!(TargetKind::Lib.kind_token(), "lib");
    assert_eq!(TargetKind::Bin.kind_token(), "bin");

    let pkg = Package {
        id: rsdeps2obsidian::ids::PackageId("p".to_string()),
        name: "p".to_string(),
        manifest_path: PathBuf::from("Cargo.toml"),
    };
    let target = Target {
        id: make_target_id("p", "lib", "p").unwrap(),
        package_id: rsdeps2obsidian::ids::PackageId("p".to_string()),
        name: "p".to_string(),
        kind: TargetKind::Lib,
        root_source: PathBuf::from("src/lib.rs"),
    };
    let module = Module {
        id: make_module_id("p", "p", &["m".to_string()]),
        target_id: make_target_id("p", "lib", "p").unwrap(),
        name: "m".to_string(),
        qualified_name: "crate::m".to_string(),
        source_path: Some(PathBuf::from("src/m.rs")),
        source_span: Some(span),
    };
    let dep = Dependency {
        from: make_module_id("p", "p", &["m".to_string()]),
        to: DependencyTarget::External(make_external_id("serde", "1.0.0")),
        kind: DependencyKind::Use,
        source_span: None,
    };
    let _internal = DependencyTarget::Internal(make_module_id("p", "p", &["n".to_string()]));
    let _kinds = [
        DependencyKind::Module,
        DependencyKind::Use,
        DependencyKind::ReExport,
    ];
    let diag = Diagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "MOD001".to_string(),
        message: "unresolved".to_string(),
        source_span: None,
    };
    let graph = WorkspaceGraph {
        packages: vec![pkg],
        targets: vec![target],
        modules: vec![module],
        dependencies: vec![dep],
        diagnostics: vec![diag],
    };
    assert_eq!(graph.packages.len(), 1);
    let _sevs = [
        DiagnosticSeverity::Info,
        DiagnosticSeverity::Warning,
        DiagnosticSeverity::Error,
    ];
}
