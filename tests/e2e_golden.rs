//! Task 9: end-to-end golden tests.
//!
//! The `full-workspace` fixture exercises lib + 2 bins, inline/nested
//! modules, `#[path]`, aliases, `pub use`, duplicate edges, a real external
//! (`serde`), `#[cfg]`, the ambiguous `qux.rs` + `qux/mod.rs` layout, and a
//! Item-level `use` paths (e.g. `use crate::models::User;`) resolve by
//! longest-prefix attribution to the nearest known ancestor module (per
//! Post-MVP amendment A), and bare `core::...` imports resolve across the
//! member package's targets; only genuinely unresolvable ownership stays
//! `SCOPE001`. The goldens document actual behavior.
//!
//! Snapshots live in `tests/snapshots/`: `tree/` mirrors the output tree
//! (`manifest.json`, `graph.json`, every `.md` note), `stdout.txt` and
//! `stderr.txt` pin the binary's stream split. All snapshot bytes use the
//! `@@WORKSPACE@@` placeholder for the absolute fixture root so they are
//! machine-independent; tests normalize actual bytes the same way before
//! comparing. Regenerate single-threaded with
//! `RSDEPS_UPDATE_SNAPSHOTS=1 cargo test --test e2e_golden -- --test-threads=1`
//! (once, eyeball the diff for spec conformance, then lock).

use rsdeps2obsidian::config::{resolve_config, CliArgs, Config};
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/full-workspace")
}

fn snapshots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn e2e_config(output: &Path) -> Config {
    let fixture = fixture_root();
    let args = CliArgs {
        workspace: Some(fixture),
        manifest_path: None,
        output: Some(output.to_path_buf()),
        clean: false,
        dry_run: false,
        external: true,
        verbose: false,
    };
    // Absolute --output resolves identically under any cwd.
    resolve_config(&args, Path::new("/")).expect("e2e config resolves")
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
        .arg("--external");
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
fn full_run_produces_golden_bytes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out = tmp.path().join("graph");
    let config = e2e_config(&out);
    let summary = run(&config).expect("full run succeeds");

    // Hand-counted fixture shape: 2 packages (api, core), 4 targets
    // (api lib, alpha, beta, core lib), 17 modules.
    assert_eq!(summary.packages, 2, "packages: {summary:?}");
    assert_eq!(summary.targets, 4, "targets: {summary:?}");
    assert_eq!(summary.modules, 17, "modules: {summary:?}");
    // 26 deduped dependencies: 13 Module containment keys + 20 Use/ReExport
    // keys minus the 7 (from,to) overlaps dedup collapses to the higher
    // precedence (api->models goes ReExport, the rest Use): api->models,
    // api->qux (pub mod qux + #[cfg(unix)] use crate::qux), api->routes,
    // alpha->shared, beta->shared, services->deep, core->models. Amendment A
    // adds 2 new keys: api->core::core::models (bare member import) and
    // api::api::models->api::api::services (item-level prefix attribution);
    // the other 7 newly-resolved item imports collapse into existing keys.
    // 1 distinct external (serde@1.0.229, referenced from the lib root and beta).
    assert_eq!(summary.dependencies, 26, "dependencies: {summary:?}");
    assert_eq!(summary.external, 1, "external: {summary:?}");
    // CFG001 + MOD002 (all 9 former item-level SCOPE001 now resolve).
    assert_eq!(summary.warnings, 2, "warnings: {summary:?}");
    // Fresh tree: 17 module notes + 1 external note + index.md + graph.json.
    assert_eq!(summary.create.len(), 20, "create: {summary:?}");
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
        "output tree differs from tests/snapshots/tree"
    );
}

#[test]
fn repeated_runs_are_identical() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_a = tmp.path().join("a");
    let out_b = tmp.path().join("b");
    let first = run(&e2e_config(&out_a)).expect("first run");
    let second = run(&e2e_config(&out_b)).expect("second run");
    // Raw bytes are comparable: content carries workspace paths (identical
    // fixture) but never output paths.
    assert_eq!(
        snapshot_tree(&out_a),
        snapshot_tree(&out_b),
        "two runs into different dirs must be byte-identical"
    );
    // Same-dir rerun is fully stable too (everything becomes update).
    let rerun = run(&e2e_config(&out_a)).expect("rerun");
    assert_eq!(snapshot_tree(&out_a), snapshot_tree(&out_b));
    assert_eq!(rerun.create.len(), 0, "rerun creates: {rerun:?}");
    assert_eq!(rerun.update.len(), 20, "rerun updates: {rerun:?}");
    assert_eq!(rerun.delete.len(), 0, "rerun deletes: {rerun:?}");
    assert_eq!(first.workspace, second.workspace);
}

#[test]
fn dry_run_leaves_tree_untouched() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out = tmp.path().join("graph");
    let mut args = e2e_config(&out);
    args.dry_run = true;
    let summary = run(&args).expect("dry run succeeds");
    assert_eq!(summary.create.len(), 20, "dry-run plan: {summary:?}");
    assert!(!out.exists(), "dry-run must not create the output dir");

    // The binary prints the plan on stdout and writes nothing.
    let (stdout, _stderr, code) = bin_stdout_stderr(&fixture_root(), &out, &["--dry-run"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("status: success"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("plan create: "),
        "dry-run must print the plan:\n{stdout}"
    );
    assert!(
        !out.exists(),
        "binary dry-run must not create the output dir"
    );
}

#[test]
fn clean_path_requires_valid_manifest() {
    use rsdeps2obsidian::commit::MANIFEST_FILENAME;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out = tmp.path().join("graph");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("unrelated.md"), "user note\n").unwrap();
    let clean_config = |dry_run: bool| Config {
        workspace_input: fixture_root(),
        manifest_path: None,
        output_raw: out.clone(),
        clean: true,
        dry_run,
        external: true,
        verbose: false,
    };

    // Missing manifest: report + zero deletes (Ok), tree untouched.
    let summary = run(&clean_config(false)).expect("clean without manifest is Ok");
    assert!(summary.delete.is_empty(), "clean deletes: {summary:?}");
    assert!(out.join("unrelated.md").exists());

    // Invalid manifest: fatal (Err) + zero deletes.
    fs::write(out.join(MANIFEST_FILENAME), "{not json").unwrap();
    let err = run(&clean_config(false)).unwrap_err();
    assert!(err.contains("invalid manifest"), "unexpected error: {err}");
    assert!(out.join("unrelated.md").exists());
    assert!(out.join(MANIFEST_FILENAME).exists());

    // Valid manifest: deletes exactly the listed entries, spares the commit
    // record and unrelated files; second clean is empty (idempotent).
    let live = tmp.path().join("live");
    let produced = run(&e2e_config(&live)).expect("setup run");
    assert_eq!(produced.create.len(), 20);
    let clean_live = Config {
        output_raw: live.clone(),
        clean: true,
        ..e2e_config(&live)
    };
    let cleaned = run(&clean_live).expect("clean with manifest");
    assert_eq!(cleaned.delete.len(), 20, "clean deletes: {cleaned:?}");
    assert!(
        live.join(MANIFEST_FILENAME).exists(),
        "commit record survives clean"
    );
    assert_eq!(
        run(&clean_live).expect("second clean").delete.len(),
        0,
        "clean is idempotent"
    );

    // --dry-run --clean: zero deletes, delete plan printed on stdout.
    let (stdout, _stderr, code) =
        bin_stdout_stderr(&fixture_root(), &live, &["--clean", "--dry-run"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("status: success"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("plan delete: "),
        "dry-run --clean must print the plan:\n{stdout}"
    );
    assert!(
        live.join(MANIFEST_FILENAME).exists(),
        "dry-run deletes nothing"
    );
}

#[test]
fn stdout_stderr_split() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out = tmp.path().join("graph");
    let (stdout, stderr, code) = bin_stdout_stderr(&fixture_root(), &out, &[]);
    assert_eq!(code, 0, "stderr:\n{stderr}");

    for key in [
        "status: success",
        "packages: 2",
        "targets: 4",
        "modules: 17",
        "dependencies: 26",
        "warnings: 2",
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
    for key in ["warning[CFG001]", "warning[MOD002]"] {
        assert!(stderr.contains(key), "stderr missing {key:?}:\n{stderr}");
    }
    // Amendment A resolves every item-level import, so no SCOPE001 remains.
    assert!(
        !stderr.contains("warning[SCOPE001]"),
        "stderr must carry no SCOPE001:\n{stderr}"
    );
    assert!(
        !stderr.contains("\"protocol_version\""),
        "stderr is never JSON:\n{stderr}"
    );

    // Lock the stream goldens (workspace-normalized).
    let workspace = run(&e2e_config(&tmp.path().join("other")))
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
        "stdout differs from tests/snapshots/stdout.txt"
    );
    assert_eq!(
        norm_stderr,
        fs::read(snapshots_dir().join("stderr.txt")).expect("stderr snapshot"),
        "stderr differs from tests/snapshots/stderr.txt"
    );
}
