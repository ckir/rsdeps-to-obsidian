use rsdeps2obsidian::config::Config;
use rsdeps2obsidian::metadata::discover;
use std::path::PathBuf;

fn fixture_config(workspace: &str) -> Config {
    Config {
        workspace_input: PathBuf::from(workspace),
        manifest_path: None,
        output_raw: PathBuf::from("/tmp/rsdeps-test-output"),
        clean: false,
        dry_run: false,
        external: false,
        verbose: false,
    }
}

fn fixture_root() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/min-workspace").to_string()
}

#[test]
fn workspace_discovery_finds_packages() {
    let ws = discover(&fixture_config(&fixture_root())).unwrap();
    let names: Vec<&str> = ws.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["api", "domain"]);
}

#[test]
fn lib_and_bin_targets_get_distinct_namespaces() {
    let ws = discover(&fixture_config(&fixture_root())).unwrap();
    let ids: Vec<&str> = ws.targets.iter().map(|t| t.id.0.as_str()).collect();
    assert!(ids.contains(&"api::lib::api"), "missing api lib: {ids:?}");
    assert!(
        ids.contains(&"api::bin::server"),
        "missing api bin: {ids:?}"
    );
    let lib = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "api::lib::api")
        .unwrap();
    let bin = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "api::bin::server")
        .unwrap();
    assert_ne!(lib.root_source, bin.root_source);
}

#[test]
fn unsupported_target_kinds_are_ignored() {
    let ws = discover(&fixture_config(&fixture_root())).unwrap();
    // The fixture declares test- and bench-only targets; they must be skipped.
    assert!(
        ws.targets
            .iter()
            .all(|t| t.id.0.contains("::lib::") || t.id.0.contains("::bin::")),
        "unexpected target kinds: {:?}",
        ws.targets.iter().map(|t| &t.id.0).collect::<Vec<_>>()
    );
    assert!(ws.targets.iter().any(|t| t.id.0 == "api::lib::api"));
    assert!(ws.targets.iter().any(|t| t.id.0 == "api::bin::server"));
}

#[test]
fn metadata_failure_is_fatal_string() {
    let err = discover(&fixture_config("/nonexistent-dir-rsdeps-xyz")).unwrap_err();
    assert!(!err.is_empty());
}
