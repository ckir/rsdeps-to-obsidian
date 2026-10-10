//! Task 4: C# end-to-end golden tests.
//!
//! The `cs-workspace` fixture is a multi-project solution (`App.sln` with lib
//! `Web.csproj` + Exe `Cli.csproj` referencing `Web`, plus loose library
//! `Tool.csproj` outside the solution) exercising every settled Task 2/3
//! rule: block namespaces (`Web.Services`) + file-scoped namespaces
//! (`Web.Scoped`) + partial merge (`Web.Partial` across `Part1.cs`/`Part2.cs`
//! into one node at the first sorted file) + two namespaces sharing one file
//! (`Web.Alpha` + `Web.Beta` in `Two.cs`, distinct spans) + a namespace-less
//! top-level `Program.cs` in the Exe (stem module per Bin target); explicit
//! usings of every shape (plain `using Web.Services;`, `using static` by
//! full type path, alias `using H = ...` by full path with the alias never
//! minting an ID); a `global using` present-but-inert (ambient entries never
//! edge and never diagnose: bare `Consumer` and the global's own `Glob`
//! carry zero `Use` edges); per-TFM `ImplicitUsings` enabled on `Web`
//! (likewise silent); cross-project `ProjectReference` edges (`Cli` files
//! → `Web.Services`, one hop, `Web` + `Tool` both referenced); the
//! same-namespace-two-projects ambiguity (`Shared.Models` declared by both
//! `Web` and `Tool`, used from `Cli` → `SCOPE001`, no edge); pinned
//! (`Newtonsoft.Json`), CPM-`VersionOverride` (`CentralLib`) + CPM-`Version`
//! (`PropsLib`), lockfile-`resolved` (`Serilog` in `Cli/packages.lock.json`,
//! undeclared in any csproj), and legacy `packages.config` (`Dapper` in
//! `Tool`) externals as `External` edges; floating (`Floaty` `*`), range
//! (`RangeLib` `[6.*)`), and `Build.props`-only (`BuildOnly`, whose `9.9.9`
//! lives in the never-read `Directory.Build.props`) names as `EXT001`;
//! silent BCL (`System.Linq` — never an edge, never a diagnostic); an
//! item-level remainder miss (`using NoSuchLib.Thing;` — no prefix names a
//! module, package, or BCL root → `SCOPE001`); an unknown `using static`
//! path (`Bogus.Type` → `SCOPE001`); exactly one run-level `CFG001` for the
//! `#if` in `Cfg.cs` (whose explicit `using` sits outside the `#if`, so the
//! edge is certain regardless of preproc query subtleties); the Bin-only
//! Exe (`Cli`, no Lib tree) plus multi-TFM `-net8.0`/`-net48` suffix targets
//! (every `Cli` file — and its diagnostics — duplicated per TFM); the
//! `obj/` exclusion (`Web/obj/Generated.cs` declares `Web.Sneaky` and is
//! never walked, pinned by the exact module counts); and NO other
//! diagnostic code in the output (`IO001` avoided: every walked root
//! exists, every `Compile` default resolves, all XML well-formed).
//!
//! Same-package fallback note (settled Task 3 rule, NOT reachable here by
//! construction): discovery gives every same-package target the same
//! `root_source`, so sibling trees are identical and arm (a2) can never
//! fire — anything resolving in a sibling resolves in the own tree first.
//! The rule stays pinned by the `same_package_fallback_edges` unit test in
//! `tests/resolver_cs.rs`; the goldens below document every
//! discovery-reachable rule.
//!
//! Snapshots live in `tests/snapshots/cs/`: `tree/` mirrors the output tree
//! (`manifest.json`, `graph.json`, every `.md` note), `stdout.txt` and
//! `stderr.txt` pin the binary's stream split. All snapshot bytes use the
//! `@@WORKSPACE@@` placeholder for the absolute fixture root so they are
//! machine-independent; tests normalize actual bytes the same way before
//! comparing. Regenerate single-threaded with
//! `RSDEPS_UPDATE_SNAPSHOTS=1 cargo test --test cs_golden -- --test-threads=1`
//! (once, eyeball the diff for rule conformance, then lock). This rewrites
//! only `tests/snapshots/cs/`; the Rust goldens under `tests/snapshots/`
//! and the Python/TypeScript goldens under `tests/snapshots/py|ts/` stay
//! byte-identical (any hunk there is a contamination failure).

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cs-workspace")
}

fn snapshots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/cs")
}

fn cs_config(output: &Path) -> Config {
    let fixture = fixture_root();
    let args = CliArgs {
        workspace: Some(fixture),
        manifest_path: None,
        output: Some(output.to_path_buf()),
        clean: false,
        dry_run: false,
        external: true,
        verbose: false,
        lang: Lang::Csharp,
    };
    // Absolute --output resolves identically under any cwd.
    resolve_config(&args, Path::new("/")).expect("cs e2e config resolves")
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
        .arg("csharp");
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
fn cs_full_run_produces_golden_bytes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out = tmp.path().join("graph");
    let config = cs_config(&out);
    let summary = run(&config).expect("csharp run succeeds");

    // Hand-counted fixture shape: 3 packages (Cli, Tool, Web), 4 targets
    // (Web lib, Tool lib, Cli-net8.0 + Cli-net48 bins — the Exe is
    // Bin-only, never Lib), 26 modules (17 Web: root + Services, Scoped,
    // Partial, Alpha, Beta, Alias, Static, Glob, Consumer, Sys, Ext,
    // Unpinned, Shared.Models, Cfg, Remnant, Unknown; 3 Tool: root +
    // Shared.Models + Tool.Consumer; 6 Cli: per-TFM root + Program stem +
    // Cli.Worker). The `obj/`-excluded `Web.Sneaky` never appears.
    assert_eq!(summary.packages, 3, "packages: {summary:?}");
    assert_eq!(summary.targets, 4, "targets: {summary:?}");
    assert_eq!(summary.modules, 26, "modules: {summary:?}");
    // 38 deduped dependencies: 22 Module containment keys (every non-root
    // module parents at its target root — no intermediate `Web` module is
    // declared) + 16 Use keys with no (from,to) overlap to collapse. Use
    // keys: Scoped/Static/Alias/Alpha/Beta/Cfg->Services (plain, static,
    // alias x3 counting both Two.cs modules, Cfg explicit); Ext->Newtonsoft
    // +CentralLib+PropsLib (pinned/CPM-overridden/CPM-versioned);
    // Tool.Consumer->Dapper (packages.config); per-TFM Program->Services,
    // Worker->Services (cross-project), Worker->Serilog (lockfile). 5
    // distinct externals.
    assert_eq!(summary.dependencies, 38, "dependencies: {summary:?}");
    assert_eq!(summary.external, 5, "external: {summary:?}");
    // 8 warnings: 4 SCOPE001 (Remnant item-remainder `NoSuchLib.Thing`,
    // Unknown `using static Bogus.Type`, Worker `Shared.Models` ambiguity
    // x2 TFMs) + 3 EXT001 (Unpinned `Floaty.Thing` floating,
    // `RangeLib.Thing` range, `BuildOnly.Thing` Build.props-only) + 1
    // CFG001 (run-level, `Cfg.cs` `#if`). No IO001 or any other code.
    assert_eq!(summary.warnings, 8, "warnings: {summary:?}");
    // Fresh tree: 26 module notes + 5 external notes + index.md + graph.json.
    assert_eq!(summary.create.len(), 33, "create: {summary:?}");
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
        "output tree differs from tests/snapshots/cs/tree"
    );
}

#[test]
fn cs_repeated_runs_identical_and_dry_run_clean() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_a = tmp.path().join("a");
    let out_b = tmp.path().join("b");
    let first = run(&cs_config(&out_a)).expect("first run");
    let second = run(&cs_config(&out_b)).expect("second run");
    // Raw bytes are comparable: content carries workspace paths (identical
    // fixture) but never output paths.
    assert_eq!(
        snapshot_tree(&out_a),
        snapshot_tree(&out_b),
        "two runs into different dirs must be byte-identical"
    );
    // Same-dir rerun is fully stable too (everything becomes update).
    let rerun = run(&cs_config(&out_a)).expect("rerun");
    assert_eq!(snapshot_tree(&out_a), snapshot_tree(&out_b));
    assert_eq!(rerun.create.len(), 0, "rerun creates: {rerun:?}");
    assert_eq!(rerun.update.len(), 33, "rerun updates: {rerun:?}");
    assert_eq!(rerun.delete.len(), 0, "rerun deletes: {rerun:?}");
    assert_eq!(first.workspace, second.workspace);

    // --dry-run plans the full tree and writes nothing.
    let out_dry = tmp.path().join("dry");
    let mut dry = cs_config(&out_dry);
    dry.dry_run = true;
    let planned = run(&dry).expect("dry run succeeds");
    assert_eq!(planned.create.len(), 33, "dry-run plan: {planned:?}");
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
fn cs_stdout_stderr_split() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out = tmp.path().join("graph");
    // Bin-level `--lang csharp` run against a temp output dir.
    let (stdout, stderr, code) = bin_stdout_stderr(&fixture_root(), &out, &[]);
    assert_eq!(code, 0, "stderr:\n{stderr}");

    for key in [
        "status: success",
        "packages: 3",
        "targets: 4",
        "modules: 26",
        "dependencies: 38",
        "warnings: 8",
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
    for key in ["warning[SCOPE001]", "warning[EXT001]", "warning[CFG001]"] {
        assert!(stderr.contains(key), "stderr missing {key:?}:\n{stderr}");
    }
    // C# reuses exactly three diagnostic codes: nothing else may appear.
    for key in [
        "warning[MOD",
        "warning[TS001]",
        "warning[IO001]",
        "warning[PATH",
        "warning[LANG",
        "error[",
    ] {
        assert!(
            !stderr.contains(key),
            "stderr must carry no {key:?}:\n{stderr}"
        );
    }
    assert!(
        !stderr.contains("\"protocol_version\""),
        "stderr is never JSON:\n{stderr}"
    );

    // Lock the stream goldens (workspace-normalized).
    let workspace = run(&cs_config(&tmp.path().join("other")))
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
        "stdout differs from tests/snapshots/cs/stdout.txt"
    );
    assert_eq!(
        norm_stderr,
        fs::read(snapshots_dir().join("stderr.txt")).expect("stderr snapshot"),
        "stderr differs from tests/snapshots/cs/stderr.txt"
    );
}
