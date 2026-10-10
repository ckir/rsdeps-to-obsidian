//! Task 2: C# project discovery.
//!
//! `discover_cs` maps a .NET workspace dir to a [`DiscoveredWorkspace`]:
//! root is the dir of the nearest `*.sln` (members = projects), else the dir
//! of the nearest `*.csproj` walking down from the input (never above it),
//! else the workspace input itself; loose `*.csproj` files each become own
//! packages and multiple `.sln` files union their members. The package name
//! is `<AssemblyName>` when present, else the csproj file stem. Libraries
//! get Lib target(s), `OutputType=Exe` projects get Bin target(s) only, and
//! multi-targeted (`TargetFrameworks` plural) projects split one target per
//! TFM with a `-<tfm>` suffix. Versioned `external_deps` come from exact-pin
//! `Version` attributes, `packages.lock.json`, `Directory.Packages.props`
//! (`Version`/`VersionOverride`), and legacy `packages.config`; floating,
//! range, and undefined versions are absent. Malformed csproj XML is a hard
//! path-tagged error.

use rsdeps2obsidian::config::{Config, Lang};
use rsdeps2obsidian::discovery_cs::discover_cs;
use rsdeps2obsidian::domain::TargetKind;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::canonicalize(&root).unwrap_or(root)
}

fn cs_config(workspace: PathBuf) -> Config {
    Config {
        workspace_input: workspace,
        manifest_path: None,
        output_raw: PathBuf::from("out"),
        clean: false,
        dry_run: false,
        external: false,
        verbose: false,
        lang: Lang::Csharp,
    }
}

#[test]
fn roots_names_and_targets() {
    // `App.sln` lists Web (library, single TFM) + Cli (Exe, `net8.0;net48`);
    // `Extra.sln` lists the same members in reverse order (union, deduped)
    // plus a dangling `Missing\Missing.csproj` entry (skipped silently);
    // `Tool` is a loose library project with an explicit
    // `<OutputType>Library</OutputType>` and no TFM at all.
    // No fixture project ships `.cs` sources: targets are emitted anyway.
    let root = fixture("cs-main-workspace");
    let ws = discover_cs(&cs_config(root.clone())).expect("discovery succeeds");

    assert_eq!(ws.root, root, "root: {ws:?}");

    let names: Vec<&str> = ws.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Cli", "Tool", "Web"], "packages: {ws:?}");
    for package in &ws.packages {
        let manifest = root
            .join(&package.name)
            .join(format!("{}.csproj", package.name));
        assert_eq!(
            package.manifest_path, manifest,
            "manifest_path is the csproj: {ws:?}"
        );
    }

    // Exe projects get Bin targets ONLY (no Lib); the multi-TFM Exe splits
    // per TFM with a `-<tfm>` suffix; `root_source` is the project dir.
    let cli_bins: Vec<_> = ws
        .targets
        .iter()
        .filter(|t| t.package_id.0 == "Cli")
        .collect();
    assert_eq!(cli_bins.len(), 2, "Exe splits per TFM: {ws:?}");
    assert!(
        cli_bins.iter().all(|t| matches!(t.kind, TargetKind::Bin)),
        "Exe is Bin-only: {ws:?}"
    );
    for name in ["Cli-net48", "Cli-net8.0"] {
        let bin = cli_bins
            .iter()
            .find(|t| t.id.0 == format!("Cli::bin::{name}"))
            .unwrap_or_else(|| panic!("missing Bin {name}: {ws:?}"));
        assert_eq!(
            bin.root_source,
            root.join("Cli"),
            "Bin root_source is the project dir: {ws:?}"
        );
    }

    // Libraries get Lib targets ONLY (zero Bins), named plainly for
    // single-TFM projects; `root_source` is the project dir.
    let web_libs: Vec<_> = ws
        .targets
        .iter()
        .filter(|t| t.package_id.0 == "Web")
        .collect();
    assert_eq!(web_libs.len(), 1, "single-TFM lib: {ws:?}");
    assert_eq!(web_libs[0].id.0, "Web::lib::Web", "plain name: {ws:?}");
    assert_eq!(
        web_libs[0].root_source,
        root.join("Web"),
        "Lib root_source is the project dir: {ws:?}"
    );
    let tool_libs: Vec<_> = ws
        .targets
        .iter()
        .filter(|t| t.package_id.0 == "Tool")
        .collect();
    assert_eq!(tool_libs.len(), 1, "loose library Lib: {ws:?}");
    assert_eq!(tool_libs[0].id.0, "Tool::lib::Tool", "plain name: {ws:?}");
    assert!(
        ws.targets
            .iter()
            .filter(|t| t.package_id.0 != "Cli")
            .all(|t| matches!(t.kind, TargetKind::Lib)),
        "libraries are Lib-only: {ws:?}"
    );

    // Targets sort by (package, kind-token, name).
    let ids: Vec<&str> = ws.targets.iter().map(|t| t.id.0.as_str()).collect();
    assert_eq!(
        ids,
        [
            "Cli::bin::Cli-net48",
            "Cli::bin::Cli-net8.0",
            "Tool::lib::Tool",
            "Web::lib::Web"
        ],
        "sorted targets: {ws:?}"
    );
}

#[test]
fn versions_and_refs() {
    // `Newtonsoft.Json` carries an exact `Version` pin; `CentralLib` is
    // versionless in the csproj with a `VersionOverride` in
    // `Directory.Packages.props`; `PropsLib` is versionless with a `Version`
    // in the same props file; `Serilog` resolves via `packages.lock.json`
    // (both TFMs agree, deduped); `Dapper` comes from legacy
    // `packages.config`. `FloatingLib` (`[6.*)`) and `BuildOnlyLib`
    // (versioned only in `Directory.Build.props`, which is never read) are
    // absent — the resolver reports those as unresolved downstream.
    let root = fixture("cs-main-workspace");
    let ws = discover_cs(&cs_config(root)).expect("discovery succeeds");

    let pairs: Vec<(&str, &str)> = ws
        .external_deps
        .iter()
        .map(|d| (d.name.as_str(), d.version.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("CentralLib", "2.1.0"),
            ("Dapper", "2.0.123"),
            ("Newtonsoft.Json", "13.0.3"),
            ("PropsLib", "4.0.0"),
            ("Serilog", "3.1.1"),
        ],
        "exact versions, deduped + sorted: {ws:?}"
    );
}

#[test]
fn exe_family_output_types_yield_bins() {
    // `WinExe` (WinForms/WPF/WinUI) and `AppContainerExe` (UWP, spelled
    // lowercase here to prove the ASCII case-insensitive match) are
    // executables like `Exe`: Bin targets only, zero Lib.
    let root = fixture("cs-exe-workspace");
    let ws = discover_cs(&cs_config(root.clone())).expect("discovery succeeds");

    assert_eq!(ws.root, root, "root: {ws:?}");
    let names: Vec<&str> = ws.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Gui", "Uwp"], "packages: {ws:?}");
    let ids: Vec<&str> = ws.targets.iter().map(|t| t.id.0.as_str()).collect();
    assert_eq!(
        ids,
        ["Gui::bin::Gui", "Uwp::bin::Uwp"],
        "Bin targets only: {ws:?}"
    );
    assert!(
        ws.targets.iter().all(|t| matches!(t.kind, TargetKind::Bin)),
        "no Lib for executables: {ws:?}"
    );
}

#[test]
fn malformed_csproj_fails_closed() {
    // Garbage XML in a discovered csproj is a hard error naming the file.
    let root = fixture("cs-bad-workspace");
    let err = discover_cs(&cs_config(root)).expect_err("malformed csproj must fail");
    assert!(err.contains("Bad.csproj"), "error names the file: {err}");
}

#[test]
fn nested_sln_sets_root_and_pulls_members() {
    // `app/App.sln` (the nearest sln walking down from the input) sets the
    // root to `app/`; its members are `Inner\Inner.csproj` (loose-visible
    // too) and `..\Top\Top.csproj` (outside the root, pulled in as an
    // explicit member). The loose `Top/Top.csproj` would NOT be discovered
    // from this input without the sln reference.
    let input = fixture("cs-nested-workspace");
    let ws = discover_cs(&cs_config(input.clone())).expect("discovery succeeds");

    assert_eq!(ws.root, input.join("app"), "nearest sln sets root: {ws:?}");

    let names: Vec<&str> = ws.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Inner", "Top"], "packages: {ws:?}");
    let top = ws
        .packages
        .iter()
        .find(|p| p.name == "Top")
        .expect("outside-root member: {ws:?}");
    assert_eq!(
        top.manifest_path,
        input.join("Top").join("Top.csproj"),
        "member manifest keeps its own path: {ws:?}"
    );

    let ids: Vec<&str> = ws.targets.iter().map(|t| t.id.0.as_str()).collect();
    assert_eq!(
        ids,
        ["Inner::lib::Inner", "Top::lib::Top"],
        "targets: {ws:?}"
    );
    let top_lib = ws
        .targets
        .iter()
        .find(|t| t.id.0 == "Top::lib::Top")
        .expect("Top lib: {ws:?}");
    assert_eq!(
        top_lib.root_source,
        input.join("Top"),
        "root_source is the member project dir: {ws:?}"
    );
}
