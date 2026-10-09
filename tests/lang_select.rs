//! Task 2: `--lang` selection, registry, and run() dispatch.
//!
//! `detect_language` scans project files in priority
//! `Cargo.toml` > `pyproject.toml`/`setup.py` > `package.json` (none → Rust).
//! `analyzer_for`/`discover_for` resolve the Rust arms live; the Python and
//! TypeScript arms return exact placeholder errors that Tasks 3–4 replace.
//! `run()` on a Rust workspace behaves exactly as before (summary counts
//! unchanged), and ambiguous autodetect (no explicit flag, >1 project file)
//! appends one `LANG001` Info diagnostic.

use rsdeps2obsidian::config::{detect_language, resolve_config, CliArgs, Config, Lang};
use rsdeps2obsidian::{analyzer_for, discover_for, run};
use std::path::{Path, PathBuf};

fn touch(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), "").expect("write project file");
}

fn auto_config(workspace: PathBuf, output: PathBuf) -> Config {
    let args = CliArgs {
        workspace: Some(workspace),
        manifest_path: None,
        output: Some(output),
        clean: false,
        dry_run: false,
        external: false,
        verbose: false,
        lang: Lang::Auto,
    };
    resolve_config(&args, Path::new("/")).expect("config resolves")
}

fn read_graph_json(output: &Path) -> String {
    std::fs::read_to_string(output.join("graph.json")).expect("graph.json written")
}

#[test]
fn detect_priority_and_default() {
    // Cargo.toml beats pyproject.toml.
    let dir = tempfile::TempDir::new().expect("tempdir");
    touch(dir.path(), "Cargo.toml");
    touch(dir.path(), "pyproject.toml");
    assert_eq!(detect_language(dir.path()), Lang::Rust);

    // pyproject.toml beats package.json.
    let dir = tempfile::TempDir::new().expect("tempdir");
    touch(dir.path(), "pyproject.toml");
    touch(dir.path(), "package.json");
    assert_eq!(detect_language(dir.path()), Lang::Python);

    // setup.py alone selects Python.
    let dir = tempfile::TempDir::new().expect("tempdir");
    touch(dir.path(), "setup.py");
    assert_eq!(detect_language(dir.path()), Lang::Python);

    // package.json alone selects Typescript.
    let dir = tempfile::TempDir::new().expect("tempdir");
    touch(dir.path(), "package.json");
    assert_eq!(detect_language(dir.path()), Lang::Typescript);

    // Cargo.toml beats package.json.
    let dir = tempfile::TempDir::new().expect("tempdir");
    touch(dir.path(), "Cargo.toml");
    touch(dir.path(), "package.json");
    assert_eq!(detect_language(dir.path()), Lang::Rust);

    // No project file defaults to Rust.
    let dir = tempfile::TempDir::new().expect("tempdir");
    assert_eq!(detect_language(dir.path()), Lang::Rust);
}

#[test]
fn registry_rust_ok_others_err_exact() {
    assert!(
        analyzer_for(Lang::Rust).is_ok(),
        "Rust analyzer must resolve"
    );
    // `Box<dyn Analyzer>` is not `Debug`, so assert via let-else rather than
    // `unwrap_err`.
    let Err(e) = analyzer_for(Lang::Python) else {
        panic!("Python analyzer must not resolve yet");
    };
    assert_eq!(e, "python support not yet implemented");
    let Err(e) = analyzer_for(Lang::Typescript) else {
        panic!("TypeScript analyzer must not resolve yet");
    };
    assert_eq!(e, "typescript support not yet implemented");

    let ws = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/min-workspace");
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cfg = auto_config(ws, tmp.path().join("out"));
    assert!(
        discover_for(Lang::Rust, &cfg).is_ok(),
        "Rust discovery must resolve"
    );
    assert_eq!(
        discover_for(Lang::Python, &cfg).unwrap_err(),
        "python support not yet implemented"
    );
    assert_eq!(
        discover_for(Lang::Typescript, &cfg).unwrap_err(),
        "typescript support not yet implemented"
    );
}

#[test]
fn rust_run_still_works_through_registry() {
    // min-workspace: 2 packages (api, domain), 3 targets (api lib, api bin
    // server, domain lib), 4 modules (api lib root, server root, domain root
    // + user), 1 Module-containment dependency, no externals, no warnings.
    let ws = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/min-workspace");
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let summary = run(&auto_config(ws, tmp.path().join("out"))).expect("run succeeds");
    assert_eq!(summary.packages, 2, "packages: {summary:?}");
    assert_eq!(summary.targets, 3, "targets: {summary:?}");
    assert_eq!(summary.modules, 4, "modules: {summary:?}");
    assert_eq!(summary.dependencies, 1, "dependencies: {summary:?}");
    assert_eq!(summary.external, 0, "external: {summary:?}");
    assert_eq!(summary.warnings, 0, "warnings: {summary:?}");
}

#[test]
fn lang001_notice_only_on_ambiguous_autodetect() {
    // Minimal dependency-free Cargo package so `cargo metadata` works offline.
    let make_tiny = |extra: &[&str]| {
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write Cargo.toml");
        std::fs::create_dir(dir.path().join("src")).expect("mkdir src");
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").expect("write main");
        for name in extra {
            touch(dir.path(), name);
        }
        dir
    };

    // Ambiguous autodetect (Cargo.toml + package.json, no explicit flag):
    // run succeeds as Rust and graph.json carries exactly one LANG001 Info
    // diagnostic with the pinned message shape.
    let dir = make_tiny(&["package.json"]);
    let out = dir.path().join("out");
    let summary = run(&auto_config(dir.path().to_path_buf(), out.clone())).expect("run ok");
    assert_eq!(summary.packages, 1, "packages: {summary:?}");
    let graph = read_graph_json(&out);
    assert_eq!(graph.matches("LANG001").count(), 1, "one LANG001:\n{graph}");
    assert!(
        graph.contains(
            "multiple project files detected; analyzing Cargo.toml (rust), ignoring: package.json"
        ),
        "LANG001 message shape:\n{graph}"
    );

    // Explicit --lang rust on the same ambiguous dir: silent.
    let dir = make_tiny(&["package.json"]);
    let out = dir.path().join("out");
    let mut cfg = auto_config(dir.path().to_path_buf(), out.clone());
    cfg.lang = Lang::Rust;
    run(&cfg).expect("explicit run ok");
    let graph = read_graph_json(&out);
    assert!(
        !graph.contains("LANG001"),
        "explicit flag must be silent:\n{graph}"
    );

    // Unambiguous autodetect (Cargo.toml only): silent.
    let dir = make_tiny(&[]);
    let out = dir.path().join("out");
    run(&auto_config(dir.path().to_path_buf(), out.clone())).expect("plain run ok");
    let graph = read_graph_json(&out);
    assert!(
        !graph.contains("LANG001"),
        "single project file must be silent"
    );
}
