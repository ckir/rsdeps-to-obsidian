//! Task 5: Python end-to-end golden tests.
//!
//! The `py-workspace` fixture is a `src`-layout project (`[project]
//! name = "acme"`) exercising every settled Task 3/4 rule: `__init__` adds
//! no segment, nested + namespace packages, relative + aliased absolute
//! imports, `TYPE_CHECKING`-normal imports, item-level `SCOPE001`, the
//! ambiguous `amb.py` + `amb/__init__.py` (`MOD003`), pinned (`flask`) vs
//! unpinned (`requests`, `ghost*`, `nosuch*`) third-party (`EXT001`),
//! silent stdlib, own-package Bin/Lib imports, own-package ghosts and
//! relative misses (`SCOPE001`), plus both `Bin` kinds (top-level `tool.py`
//! script outside the import root, `src/acme/__main__.py` entry module).
//! Python has no `pub use` equivalent: there is nothing to fixture and the
//! omission is this paragraph (no re-export edge exists at module
//! granularity; `from . import api` in `__init__.py` is the closest form
//! and is covered as a plain `Use` edge).
//!
//! `src`-layout Bin subtlety (settled Task 4 strip-arm rule, pinned here):
//! the `Lib` tree is rooted at `src/`, so a `Bin` absolute import with a
//! remainder (`import acme.models`) strips `acme` and misses the tree
//! (`["models"]` is not a module; the tree carries `["acme", "models"]`)
//! → `SCOPE001`, never external. Only the bare `import acme` (empty
//! remainder) edges `Bin` → `Lib` root. `Lib`-internal copies of the same
//! paths hit verbatim and edge normally. The goldens document this.
//!
//! Snapshots live in `tests/snapshots/py/`: `tree/` mirrors the output tree
//! (`manifest.json`, `graph.json`, every `.md` note), `stdout.txt` and
//! `stderr.txt` pin the binary's stream split. All snapshot bytes use the
//! `@@WORKSPACE@@` placeholder for the absolute fixture root so they are
//! machine-independent; tests normalize actual bytes the same way before
//! comparing. Regenerate single-threaded with
//! `RSDEPS_UPDATE_SNAPSHOTS=1 cargo test --test py_golden -- --test-threads=1`
//! (once, eyeball the diff for rule conformance, then lock). This rewrites
//! only `tests/snapshots/py/`; the Rust goldens under `tests/snapshots/`
//! stay byte-identical (any hunk there is a contamination failure).

use rsdeps2obsidian::config::{resolve_config, CliArgs, Config, Lang};
use rsdeps2obsidian::run;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const WORKSPACE_TOKEN: &str = "@@WORKSPACE@@";

// Serializes snapshot update-mode (read-remove-write on the shared
// snapshot dir) so concurrent `RSDEPS_UPDATE_SNAPSHOTS=1` execution cannot
// interleave. Read (compare) mode never takes this lock, keeping parallel
// reads fast.
static SNAPSHOT_UPDATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/py-workspace")
}

fn snapshots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/py")
}

fn py_config(output: &Path) -> Config {
    let fixture = fixture_root();
    let args = CliArgs {
        workspace: Some(fixture),
        manifest_path: None,
        output: Some(output.to_path_buf()),
        clean: false,
        dry_run: false,
        external: true,
        verbose: false,
        lang: Lang::Python,
    };
    // Absolute --output resolves identically under any cwd.
    resolve_config(&args, Path::new("/")).expect("py e2e config resolves")
}

/// Roots that may appear verbatim in emitted bytes: the discovered
/// (possibly non-canonical) workspace root plus its canonical form, longest
/// first. Source spans are canonicalized while diagnostic messages may carry
/// the non-canonical join, so both must normalize.
fn normalization_roots(workspace: &Path) -> Vec<String> {
    let mut roots = vec![workspace.to_string_lossy().into_owned()];
    if let Ok(canon) = std::fs::canonicalize(workspace) {
        let canon = canon.to_string_lossy().into_owned();
        if !roots.contains(&canon) {
            roots.push(canon);
        }
    }
    roots.sort_by_key(|a| std::cmp::Reverse(a.len()));
    roots
}

fn normalize(mut bytes: Vec<u8>, roots: &[String]) -> Vec<u8> {
    for root in roots {
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i..].starts_with(root.as_bytes()) {
                out.extend_from_slice(WORKSPACE_TOKEN.as_bytes());
                i += root.len();
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        bytes = out;
    }
    bytes
}

/// Sorted rel-path -> file bytes for everything under `root`.
fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let entries = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read test tree dir {}: {e}", dir.display()));
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|e| panic!("read test tree entry in {}: {e}", dir.display()));
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.insert(
                    rel,
                    fs::read(&path)
                        .unwrap_or_else(|e| panic!("read snapshot file {}: {e}", path.display())),
                );
            }
        }
    }
    walk(root, root, &mut out);
    out
}

fn normalized_tree(root: &Path, workspace: &Path) -> BTreeMap<String, Vec<u8>> {
    let roots = normalization_roots(workspace);
    snapshot_tree(root)
        .into_iter()
        .map(|(rel, bytes)| (rel, normalize(bytes, &roots)))
        .collect()
}

fn read_snapshot_tree() -> BTreeMap<String, Vec<u8>> {
    snapshot_tree(&snapshots_dir().join("tree"))
}

fn should_update() -> bool {
    std::env::var("RSDEPS_UPDATE_SNAPSHOTS").as_deref() == Ok("1")
}

fn update_snapshots(
    tree: &BTreeMap<String, Vec<u8>>,
    stdout: Option<&[u8]>,
    stderr: Option<&[u8]>,
) {
    let _guard = SNAPSHOT_UPDATE_LOCK.lock().expect("snapshot update lock");
    let dir = snapshots_dir();
    let tree_dir = dir.join("tree");
    let _ = fs::remove_dir_all(&tree_dir);
    for (rel, bytes) in tree {
        let path = tree_dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    if let Some(out) = stdout {
        fs::write(dir.join("stdout.txt"), out).unwrap();
    }
    if let Some(err) = stderr {
        fs::write(dir.join("stderr.txt"), err).unwrap();
    }
}

fn bin_stdout_stderr(workspace: &Path, output: &Path, extra: &[&str]) -> (String, String, i32) {
    let bin = env!("CARGO_BIN_EXE_rsdeps2obsidian");
    let mut cmd = Command::new(bin);
    cmd.arg(workspace)
        .arg("--output")
        .arg(output)
        .arg("--external")
        .arg("--lang")
        .arg("python");
    for a in extra {
        cmd.arg(a);
    }
    let result = cmd.output().expect("run test binary");
    (
        String::from_utf8(result.stdout).expect("stdout utf8"),
        String::from_utf8(result.stderr).expect("stderr utf8"),
        result.status.code().unwrap_or(-1),
    )
}

#[test]
fn python_full_run_produces_golden_bytes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out = tmp.path().join("graph");
    let config = py_config(&out);
    let summary = run(&config).expect("python run succeeds");

    // Hand-counted fixture shape: 1 package (acme), 3 targets (acme lib,
    // tool + __main__ bins), 20 modules (18 lib incl. the __main__ lib
    // copy, the amb package, and the ns namespace chain; 2 bin roots).
    assert_eq!(summary.packages, 1, "packages: {summary:?}");
    assert_eq!(summary.targets, 3, "targets: {summary:?}");
    assert_eq!(summary.modules, 20, "modules: {summary:?}");
    // 27 deduped dependencies: 17 Module containment keys + 11 Use keys
    // minus the 1 (from,to) overlap dedup collapses to the higher
    // precedence (acme->api goes Use: the __init__ `from . import api` Use
    // wins over containment). Use keys: init->api; api->sib; models->sib;
    // rel->sib, rel->pkg::m, rel->pkg; ext->flask pinned (plain + from
    // collapse); star->pkg; deep->pkg::m; lib-__main__->models (verbatim);
    // tool->lib-root (bare `import acme`, empty remainder). 1 distinct
    // external (flask@3.0.0).
    assert_eq!(summary.dependencies, 27, "dependencies: {summary:?}");
    assert_eq!(summary.external, 1, "external: {summary:?}");
    // 5 SCOPE001 (views item-level; tool acme.models remainder-miss;
    // tool acme.ghost own-ghost; tool .nosuchsib relative miss;
    // bin-__main__ acme.models remainder-miss) + 7 EXT001 (ghostsecond,
    // requests, nosuchpkg.thing, nosuchmulti.item1/2, ghostpkg via
    // TYPE_CHECKING, nosuchstar) + 1 MOD003 (amb.py vs amb/__init__.py).
    assert_eq!(summary.warnings, 13, "warnings: {summary:?}");
    // Fresh tree: 20 module notes + 1 external note + index.md + graph.json.
    assert_eq!(summary.create.len(), 23, "create: {summary:?}");
    assert!(summary.update.is_empty(), "fresh run updates: {summary:?}");
    assert!(summary.delete.is_empty(), "fresh run deletes: {summary:?}");

    let tree = normalized_tree(&out, &summary.workspace);
    if should_update() {
        update_snapshots(&tree, None, None);
        return;
    }
    assert_eq!(
        tree,
        read_snapshot_tree(),
        "output tree differs from tests/snapshots/py/tree"
    );
}

#[test]
fn python_repeated_runs_identical_and_dry_run_clean() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_a = tmp.path().join("a");
    let out_b = tmp.path().join("b");
    let first = run(&py_config(&out_a)).expect("first run");
    let second = run(&py_config(&out_b)).expect("second run");
    // Raw bytes are comparable: content carries workspace paths (identical
    // fixture) but never output paths.
    assert_eq!(
        snapshot_tree(&out_a),
        snapshot_tree(&out_b),
        "two runs into different dirs must be byte-identical"
    );
    // Same-dir rerun is fully stable too (everything becomes update).
    let rerun = run(&py_config(&out_a)).expect("rerun");
    assert_eq!(snapshot_tree(&out_a), snapshot_tree(&out_b));
    assert_eq!(rerun.create.len(), 0, "rerun creates: {rerun:?}");
    assert_eq!(rerun.update.len(), 23, "rerun updates: {rerun:?}");
    assert_eq!(rerun.delete.len(), 0, "rerun deletes: {rerun:?}");
    assert_eq!(first.workspace, second.workspace);

    // --dry-run plans the full tree and writes nothing.
    let out_dry = tmp.path().join("dry");
    let mut dry = py_config(&out_dry);
    dry.dry_run = true;
    let planned = run(&dry).expect("dry run succeeds");
    assert_eq!(planned.create.len(), 23, "dry-run plan: {planned:?}");
    assert!(!out_dry.exists(), "dry-run must not create the output dir");

    // The binary prints the plan on stdout and writes nothing.
    let (stdout, _stderr, code) = bin_stdout_stderr(&fixture_root(), &out_dry, &["--dry-run"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("status: success"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("plan create: "),
        "dry-run must print the plan:\n{stdout}"
    );
    assert!(
        !out_dry.exists(),
        "binary dry-run must not create the output dir"
    );
}

#[test]
fn python_stdout_stderr_split() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out = tmp.path().join("graph");
    // Bin-level `--lang python` run against a temp output dir.
    let (stdout, stderr, code) = bin_stdout_stderr(&fixture_root(), &out, &[]);
    assert_eq!(code, 0, "stderr:\n{stderr}");

    for key in [
        "status: success",
        "packages: 1",
        "targets: 3",
        "modules: 20",
        "dependencies: 27",
        "warnings: 13",
    ] {
        assert!(stdout.contains(key), "stdout missing {key:?}:\n{stdout}");
    }
    assert!(
        !stdout.contains("warning["),
        "stdout must carry no diagnostics:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"protocol_version\""),
        "stdout is never JSON:\n{stdout}"
    );
    for key in ["warning[EXT001]", "warning[SCOPE001]", "warning[MOD003]"] {
        assert!(stderr.contains(key), "stderr missing {key:?}:\n{stderr}");
    }
    // The single-run conditional-compilation diagnostic stays Rust-only.
    assert!(
        !stderr.contains("warning[CFG001]"),
        "python stderr must carry no CFG001:\n{stderr}"
    );
    assert!(
        !stderr.contains("\"protocol_version\""),
        "stderr is never JSON:\n{stderr}"
    );

    // Lock the stream goldens (workspace-normalized).
    let workspace = run(&py_config(&tmp.path().join("other")))
        .expect("run for workspace root")
        .workspace;
    let roots = normalization_roots(&workspace);
    let norm_stdout = normalize(stdout.into_bytes(), &roots);
    let norm_stderr = normalize(stderr.into_bytes(), &roots);
    if should_update() {
        let tree = normalized_tree(&out, &workspace);
        update_snapshots(&tree, Some(&norm_stdout), Some(&norm_stderr));
        return;
    }
    assert_eq!(
        norm_stdout,
        fs::read(snapshots_dir().join("stdout.txt")).expect("stdout snapshot"),
        "stdout differs from tests/snapshots/py/stdout.txt"
    );
    assert_eq!(
        norm_stderr,
        fs::read(snapshots_dir().join("stderr.txt")).expect("stderr snapshot"),
        "stderr differs from tests/snapshots/py/stderr.txt"
    );
}
