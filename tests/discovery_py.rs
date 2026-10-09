//! Task 3: Python project discovery.
//!
//! `discover_py` maps a Python project dir to a [`DiscoveredWorkspace`]:
//! package name from `[project] name` (fallback: root dir name), one `Lib`
//! target at the import root (`src/` wins over the flat root), `Bin`
//! targets for top-level scripts / `__main__` modules / `[project.scripts]`
//! entries, and versioned `external_deps` from `==` pins and lockfiles.

use rsdeps2obsidian::config::{Config, Lang};
use rsdeps2obsidian::discovery_py::discover_py;
use rsdeps2obsidian::domain::TargetKind;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::canonicalize(&root).unwrap_or(root)
}

fn py_config(workspace: PathBuf) -> Config {
    Config {
        workspace_input: workspace,
        manifest_path: None,
        output_raw: PathBuf::from("out"),
        clean: false,
        dry_run: false,
        external: false,
        verbose: false,
        lang: Lang::Python,
    }
}

#[test]
fn src_layout_wins_and_flat_ignored_for_lib() {
    // Root holds `src/web/` plus a stray flat `top.py`: the `Lib` target
    // must point at `src/`, and the stray file must not leak into it.
    let root = fixture("py-src-workspace");
    let ws = discover_py(&py_config(root.clone())).expect("discovery succeeds");

    assert_eq!(ws.root, root, "root: {ws:?}");
    let names: Vec<_> = ws.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["web"], "packages: {ws:?}");
    assert_eq!(
        ws.packages[0].manifest_path,
        root.join("pyproject.toml"),
        "manifest: {ws:?}"
    );

    let libs: Vec<_> = ws
        .targets
        .iter()
        .filter(|t| matches!(t.kind, TargetKind::Lib))
        .collect();
    assert_eq!(libs.len(), 1, "one Lib target: {ws:?}");
    assert_eq!(libs[0].id.0, "web::lib::web");
    assert_eq!(libs[0].root_source, root.join("src"));

    // The stray flat file is a script Bin, not Lib content.
    let tops: Vec<_> = ws
        .targets
        .iter()
        .filter(|t| t.id.0 == "web::bin::top")
        .collect();
    assert_eq!(tops.len(), 1, "stray top.py is a Bin: {ws:?}");
    assert_eq!(tops[0].root_source, root.join("top.py"));
}

#[test]
fn names_and_bins() {
    // No-pyproject dir "demo": package name falls back to the dir name and
    // the manifest falls back to the root itself.
    let root = fixture("py-demo-workspace").join("demo");
    let ws = discover_py(&py_config(root.clone())).expect("discovery succeeds");

    assert_eq!(ws.root, root, "root: {ws:?}");
    assert_eq!(ws.packages.len(), 1, "one package: {ws:?}");
    assert_eq!(ws.packages[0].name, "demo");
    assert_eq!(ws.packages[0].manifest_path, root);

    // Flat layout (no `src/`): the Lib lives at the root; `run.py` and
    // `pkg/__main__.py` each become a Bin.
    let lib = ws
        .targets
        .iter()
        .find(|t| matches!(t.kind, TargetKind::Lib))
        .expect("one Lib target");
    assert_eq!(lib.id.0, "demo::lib::demo");
    assert_eq!(lib.root_source, root);

    let bin_ids: Vec<_> = ws
        .targets
        .iter()
        .filter(|t| matches!(t.kind, TargetKind::Bin))
        .map(|t| t.id.0.as_str())
        .collect();
    assert_eq!(bin_ids, ["demo::bin::__main__", "demo::bin::run"]);
    let run = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "demo::bin::run")
        .unwrap();
    assert_eq!(run.root_source, root.join("run.py"));
    let main = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "demo::bin::__main__")
        .unwrap();
    assert_eq!(main.root_source, root.join("pkg").join("__main__.py"));

    // `[project.scripts]`: `app = "web.cli:main"` resolves to
    // `src/web/cli.py`, so the Bin takes the script name; `ghost`
    // points at a missing module, so its Bin falls back to the stem.
    let root = fixture("py-scripts-workspace");
    let ws = discover_py(&py_config(root.clone())).expect("discovery succeeds");
    assert_eq!(ws.packages[0].name, "demo");
    let app = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "demo::bin::app")
        .expect("script-name Bin: {ws:?}");
    assert_eq!(app.root_source, root.join("src").join("web").join("cli.py"));
    let ghost = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "demo::bin::mod")
        .expect("stem-fallback Bin: {ws:?}");
    assert_eq!(
        ghost.root_source,
        root.join("src").join("nosuch").join("mod.py")
    );
}

#[test]
fn pins_and_lockfiles() {
    // Only `==` pins (requirements files + `[project] dependencies`) and
    // lockfile pairs land in `external_deps`, sorted by (name, version).
    // `-r`/`-e` lines, markers, ranges, comments, and bare names contribute
    // nothing.
    let root = fixture("py-pins-workspace");
    let ws = discover_py(&py_config(root.clone())).expect("discovery succeeds");

    let pairs: Vec<(&str, &str)> = ws
        .external_deps
        .iter()
        .map(|d| (d.name.as_str(), d.version.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("django", "4.2.7"),
            ("flask", "3.0.0"),
            ("gunicorn", "21.2.0"),
            ("pytest", "7.4.0"),
            ("requests", "2.31.0"),
            ("ruff", "0.1.0"),
            ("urllib3", "1.26.16"),
        ],
        "external_deps: {ws:?}"
    );
}
