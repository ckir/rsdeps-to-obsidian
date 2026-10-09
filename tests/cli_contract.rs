use rsdeps2obsidian::config::{resolve_config, CliArgs, Lang};
use rsdeps2obsidian::{GENERATOR, PROTOCOL_VERSION, SCHEMA_VERSION, SPEC_VERSION};
use std::path::{Path, PathBuf};

fn empty_args() -> CliArgs {
    CliArgs {
        workspace: None,
        manifest_path: None,
        output: None,
        clean: false,
        dry_run: false,
        external: false,
        verbose: false,
        lang: Lang::Auto,
    }
}

#[test]
fn no_args_default_to_dot_and_default_output() {
    let args = empty_args();
    let cfg = resolve_config(&args, Path::new("/work")).expect("defaults resolve");
    assert_eq!(cfg.workspace_input, PathBuf::from("."));
    assert_eq!(cfg.output_raw, PathBuf::from("/work/obsidian-graph"));
}

#[test]
fn workspace_and_manifest_path_conflict_errors() {
    let args = CliArgs {
        workspace: Some(PathBuf::from("some/workspace")),
        manifest_path: Some(PathBuf::from("some/Cargo.toml")),
        ..empty_args()
    };
    let err = resolve_config(&args, Path::new("/work")).expect_err("must conflict");
    assert!(
        err.contains("mutually exclusive"),
        "unexpected error: {err}"
    );
}

#[test]
fn relative_output_resolves_against_cwd_not_workspace() {
    let args = CliArgs {
        workspace: Some(PathBuf::from("some/workspace")),
        output: Some(PathBuf::from("../vault/graph")),
        ..empty_args()
    };
    let cfg = resolve_config(&args, Path::new("/a/b")).expect("resolves");
    assert_eq!(cfg.output_raw, PathBuf::from("/a/vault/graph"));
}

#[test]
fn handshake_constants_are_v05() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(SPEC_VERSION, "0.5.0");
    assert_eq!(SCHEMA_VERSION, 3);
    assert_eq!(GENERATOR, "cargo-obsidian-graph");
}
