//! Task 2: TypeScript project discovery.
//!
//! `discover_ts` maps a TypeScript project dir to a [`DiscoveredWorkspace`]:
//! root is the nearest dir with `package.json`, else the nearest with
//! `tsconfig.json`, else the workspace input; the package name is kept
//! verbatim (scoped `@org/web` included); one `Lib` target per
//! `tsconfig*.json` (root `tsconfig.json` takes the package short name,
//! `tsconfig.<stem>.json` takes `<stem>`); `Bin` targets come from the
//! `package.json` `bin` table (missing files are still emitted); and
//! versioned `external_deps` come from lockfiles first, then the top-level
//! `node_modules` scan.

use rsdeps2obsidian::config::{Config, Lang};
use rsdeps2obsidian::discovery_ts::discover_ts;
use rsdeps2obsidian::domain::TargetKind;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::canonicalize(&root).unwrap_or(root)
}

fn ts_config(workspace: PathBuf) -> Config {
    Config {
        workspace_input: workspace,
        manifest_path: None,
        output_raw: PathBuf::from("out"),
        clean: false,
        dry_run: false,
        external: false,
        verbose: false,
        lang: Lang::Typescript,
    }
}

#[test]
fn roots_names_and_tsconfigs() {
    // `package.json` names `@org/web` (kept verbatim, `/` intact);
    // `tsconfig.json` (rootDir src) + `tsconfig.node.json` (extends the root
    // config, inheriting rootDir) each become a Lib target: the root one
    // takes the package short name, the dotted one its stem.
    let root = fixture("ts-roots-workspace");
    let ws = discover_ts(&ts_config(root.clone())).expect("discovery succeeds");

    assert_eq!(ws.root, root, "root: {ws:?}");
    assert_eq!(ws.packages.len(), 1, "one package: {ws:?}");
    assert_eq!(
        ws.packages[0].name, "@org/web",
        "scoped name verbatim: {ws:?}"
    );
    assert_eq!(
        ws.packages[0].manifest_path,
        root.join("package.json"),
        "manifest: {ws:?}"
    );

    let libs: Vec<_> = ws
        .targets
        .iter()
        .filter(|t| matches!(t.kind, TargetKind::Lib))
        .collect();
    assert_eq!(libs.len(), 2, "two Lib targets: {ws:?}");
    let web = libs
        .iter()
        .find(|t| t.id.0 == "@org/web::lib::web")
        .expect("root tsconfig Lib: {ws:?}");
    assert_eq!(web.root_source, root.join("src"), "rootDir honored: {ws:?}");
    let node = libs
        .iter()
        .find(|t| t.id.0 == "@org/web::lib::node")
        .expect("stem tsconfig Lib: {ws:?}");
    assert_eq!(
        node.root_source,
        root.join("src"),
        "extends rootDir inherited: {ws:?}"
    );
}

#[test]
fn extends_rootdir_inherited() {
    // `rootDir` inherited from a JSONC base config (comments + trailing
    // comma) resolves against the base file's directory.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        tmp.path().join("package.json"),
        "{\"name\": \"kid\", \"version\": \"1.0.0\"}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("tsconfig.base.json"),
        "{\n  // base layout\n  \"compilerOptions\": {\n    \"rootDir\": \"./src\",\n  },\n}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("tsconfig.json"),
        "{\"extends\": \"./tsconfig.base.json\"}\n",
    )
    .unwrap();
    let ws = discover_ts(&ts_config(tmp.path().to_path_buf())).expect("discovery succeeds");
    let kid = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "kid::lib::kid")
        .expect("root Lib: {ws:?}");
    assert_eq!(
        kid.root_source,
        tmp.path().join("src"),
        "rootDir honored: {ws:?}"
    );
}

#[test]
fn bins_and_missing() {
    // `bin` maps `app` to an existing file and `ghost` to a missing one:
    // both Bins are emitted (the resolver fails closed downstream).
    let root = fixture("ts-bins-workspace");
    let ws = discover_ts(&ts_config(root.clone())).expect("discovery succeeds");

    let app = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "cmddemo::bin::app")
        .expect("app Bin: {ws:?}");
    assert_eq!(app.root_source, root.join("dist").join("app.js"));
    let ghost = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "cmddemo::bin::ghost")
        .expect("ghost Bin emitted despite missing file: {ws:?}");
    assert_eq!(ghost.root_source, root.join("nope.js"));

    // No `bin` table means zero Bins.
    let root = fixture("ts-nobin-workspace");
    let ws = discover_ts(&ts_config(root.clone())).expect("discovery succeeds");
    let bins: Vec<_> = ws
        .targets
        .iter()
        .filter(|t| matches!(t.kind, TargetKind::Bin))
        .collect();
    assert!(bins.is_empty(), "no table, no Bins: {ws:?}");
}

#[test]
fn versions_lock_then_modules() {
    // The lock pins lodash (winning over the stale copy under
    // `node_modules`); react comes from `node_modules` with no lock entry;
    // the nested copy and the bare unpinned name contribute nothing.
    let root = fixture("ts-versions-workspace");
    let ws = discover_ts(&ts_config(root.clone())).expect("discovery succeeds");

    let pairs: Vec<(&str, &str)> = ws
        .external_deps
        .iter()
        .map(|d| (d.name.as_str(), d.version.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [("lodash", "4.17.21"), ("react", "18.2.0")],
        "external_deps: {ws:?}"
    );
}

#[test]
fn malformed_tsconfig_fails_closed() {
    // A broken tsconfig is a hard error (path-tagged), never a silently
    // incomplete target list.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        tmp.path().join("package.json"),
        "{\"name\": \"broken\", \"version\": \"1.0.0\"}\n",
    )
    .unwrap();
    let bad = tmp.path().join("tsconfig.json");
    std::fs::write(&bad, "{\"compilerOptions\": {oops}\n").unwrap();
    let err = discover_ts(&ts_config(tmp.path().to_path_buf())).unwrap_err();
    assert!(
        err.contains("tsconfig.json"),
        "error names the bad file: {err}"
    );
}

#[test]
fn legacy_lock_and_yarn_merge() {
    // The legacy `dependencies` table (npm-shrinkwrap) and yarn.lock v1 both
    // contribute versions; the first lockfile in read order wins per name.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        tmp.path().join("package.json"),
        "{\"name\": \"oldie\", \"version\": \"1.0.0\"}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("npm-shrinkwrap.json"),
        "{\"dependencies\": {\"debug\": {\"version\": \"4.3.4\"}, \
         \"shared\": {\"version\": \"1.0.0\"}}, \"lockfileVersion\": 1, \
         \"name\": \"oldie\", \"version\": \"1.0.0\"}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("yarn.lock"),
        "# yarn lockfile v1\n\n\n\"chalk@^5.0.0\":\n  version \"5.3.0\"\n  \
         resolved \"https://registry.yarnpkg.com/chalk/-/chalk-5.3.0.tgz\"\n\n\
         \"shared@^1.0.0\":\n  version \"2.0.0\"\n  \
         resolved \"https://registry.yarnpkg.com/shared/-/shared-2.0.0.tgz\"\n",
    )
    .unwrap();
    let ws = discover_ts(&ts_config(tmp.path().to_path_buf())).expect("discovery succeeds");

    let pairs: Vec<(&str, &str)> = ws
        .external_deps
        .iter()
        .map(|d| (d.name.as_str(), d.version.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [("chalk", "5.3.0"), ("debug", "4.3.4"), ("shared", "1.0.0"),],
        "legacy + yarn versions, first lockfile wins: {ws:?}"
    );
}

#[test]
fn node_modules_scope_and_nesting() {
    // `@scope/util` two levels down is a top-level install (scope dir, not
    // nesting) and counts; a copy under a nested `node_modules` does not.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        tmp.path().join("package.json"),
        "{\"name\": \"scoped\", \"version\": \"1.0.0\"}\n",
    )
    .unwrap();
    let scope_dir = tmp.path().join("node_modules").join("@scope").join("util");
    std::fs::create_dir_all(&scope_dir).unwrap();
    std::fs::write(
        scope_dir.join("package.json"),
        "{\"name\": \"@scope/util\", \"version\": \"1.0.0\"}\n",
    )
    .unwrap();
    let nested = tmp
        .path()
        .join("node_modules")
        .join("util")
        .join("node_modules");
    std::fs::create_dir_all(nested.join("deep")).unwrap();
    std::fs::write(
        tmp.path()
            .join("node_modules")
            .join("util")
            .join("package.json"),
        "{\"name\": \"util\", \"version\": \"2.0.0\"}\n",
    )
    .unwrap();
    std::fs::write(
        nested.join("deep").join("package.json"),
        "{\"name\": \"deep\", \"version\": \"9.9.9\"}\n",
    )
    .unwrap();
    let ws = discover_ts(&ts_config(tmp.path().to_path_buf())).expect("discovery succeeds");

    let pairs: Vec<(&str, &str)> = ws
        .external_deps
        .iter()
        .map(|d| (d.name.as_str(), d.version.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [("@scope/util", "1.0.0"), ("util", "2.0.0")],
        "scoped in, nested out: {ws:?}"
    );
}

#[test]
fn bin_string_form_and_non_js_out() {
    // A bare-string `bin` is one executable named after the package; values
    // pointing at non-JS files (and non-string values) are out.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::create_dir(tmp.path().join("dist")).unwrap();
    std::fs::write(tmp.path().join("dist").join("cli.mjs"), "bin\n").unwrap();
    std::fs::write(
        tmp.path().join("package.json"),
        "{\"bin\": \"./dist/cli.mjs\", \"name\": \"toolbox\", \"version\": \"1.0.0\"}\n",
    )
    .unwrap();
    let ws = discover_ts(&ts_config(tmp.path().to_path_buf())).expect("discovery succeeds");
    let cli = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "toolbox::bin::toolbox")
        .expect("string-form Bin: {ws:?}");
    assert_eq!(cli.root_source, tmp.path().join("dist").join("cli.mjs"));

    std::fs::write(
        tmp.path().join("package.json"),
        "{\"bin\": {\"helper\": \"./bin/help.sh\", \"weird\": 42}, \
         \"name\": \"toolbox\", \"version\": \"1.0.0\"}\n",
    )
    .unwrap();
    let ws = discover_ts(&ts_config(tmp.path().to_path_buf())).expect("discovery succeeds");
    let bins: Vec<_> = ws
        .targets
        .iter()
        .filter(|t| matches!(t.kind, TargetKind::Bin))
        .collect();
    assert!(
        bins.is_empty(),
        "non-JS and non-string bin values emit no Bin: {ws:?}"
    );
}
