# rsdeps-to-Obsidian MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `rsdeps2obsidian` CLI implementing the full v0.4 MVP (all 34 ACs) as a core lib + thin binary.

**Architecture:** Single package `rsdeps2obsidian` with a lib (ids, domain, config, metadata, discovery, resolver, graph, diag, paths, render, manifest, commit) plus a thin `src/bin/rsdeps2obsidian.rs`. `SynAnalyzer` implements `trait Analyzer`; the renderer consumes only `WorkspaceGraph`.

**Tech Stack:** Rust (edition 2021, floor ~1.80, host 1.99), `clap 4`, `cargo_metadata`, `syn 2` (full features for ItemMod/ItemUse/attrs), `serde`/`serde_json`, `tempfile` (tests), `thiserror`/`anyhow`.

**Spec:** `docs/superpowers/specs/2026-10-09-rsdeps-obsidian-mvp-design.md` (implements v0.4.0 at `docs/mvp/rust-obsidian-dependency-graph-mvp-v0.4.md` with the §0 deviations).

## Global Constraints

- Handshake emitted everywhere (JSON, manifest, every `.md` frontmatter): `protocol_version = 1`, `spec_version = "0.4.0"`, `schema_version = 3`.
- Emitted marker string is `generator: cargo-obsidian-graph` (binary is `rsdeps2obsidian`); orphan detection requires that field **and** `protocol_version: 1`.
- `Cargo metadata format-version = 1` requested explicitly; never confused with graph protocol/schema/spec versions.
- Binary name `rsdeps2obsidian`; default output `./obsidian-graph` resolved against **process CWD**; no-arg workspace defaults to `.`.
- stdin never read; stdout = human summary + dry-run plan (never JSON); stderr = all warnings/errors; exit `0` success, `1` fatal, `2` reserved.
- Target tokens `lib`/`bin` only; names preserved exactly (no case fold, no `-`→`_`); `::` in a component is rejected, never escaped.
- Dedup key `(from, to)`, precedence `ReExport > Use > Module`; dependency comparator `(from ↑, Internal<External, target-string ↑, Module<Use<ReExport)` via explicit comparator, never discriminants.
- Diagnostics total order: severity `Info<Warning<Error` → code → workspace-relative `/` path → line (missing first) → message.
- Filename encoding: per-segment `_`→`___`, join with `__`, + `.md`; then Windows policy (§14.3 order); no hashes/counters/order-dependence; overlong → diagnostic + skip.
- Safety S1–S8 hold: manifest-only deletion, no source writes, invalid manifest ≠ empty, no traversal, no symlink escape, no fabricated edges, dry-run zero writes, manifest-is-commit-record.
- Determinism: identical metadata+source+config+flags → byte-identical artifacts (no hash-map order leaks; sort at serialization boundaries).
- No custom Rust lexer/parser; `syn` only for `File/ItemMod/ItemUse`/attrs/paths; renderer never touches syn types.

## Review Focus

- Module names containing literal `_`/`__` must not collide with the `::`→`__` separator (e.g. `my__module` vs two segments) — expect reversible distinct filenames; pinned in Task 3 tests.
- Symlinked components anywhere in the output chain (including `./vault` parent) must abort as `unsafe output path`, never silently redirect — expect exit 1 with zero writes; pinned in Task 3/8 tests.
- `foo.rs` + `foo/mod.rs` both present must select `foo.rs` with exactly one `MOD002` warning and continue — never abort, never silently pick `mod.rs`; pinned in Task 5 tests.
- Renamed Cargo deps (`rename = …`) and duplicate crate versions (`serde@1.0.210` vs `serde@1.0.190`) must yield one external node per resolved name@version, never per alias — expect distinct nodes, no fork on alias; pinned in Task 6 tests.
- `#[cfg]` on any encountered `mod`/`use` must yield exactly one run-level `CFG001` warning regardless of count, with the graph reflecting unexpanded source — expect count == 1 after N cfg items; pinned in Task 6 tests.

---

### Task 1: Scaffold, constants, CLI contract

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/config.rs`
- Create: `src/bin/rsdeps2obsidian.rs`
- Test: `tests/cli_contract.rs`

**Interfaces:**
- Consumes: nothing (first task).
- Produces:
  - `src/lib.rs: pub const PROTOCOL_VERSION: u32 = 1; pub const SPEC_VERSION: &str = "0.4.0"; pub const SCHEMA_VERSION: u32 = 3; pub const GENERATOR: &str = "cargo-obsidian-graph";`
  - `src/config.rs: pub struct CliArgs { pub workspace: Option<PathBuf>, pub manifest_path: Option<PathBuf>, pub output: Option<PathBuf>, pub clean: bool, pub dry_run: bool, pub external: bool, pub verbose: bool }`
  - `src/config.rs: pub struct Config { pub workspace_input: PathBuf, pub manifest_path: Option<PathBuf>, pub output_raw: PathBuf, pub clean: bool, pub dry_run: bool, pub external: bool, pub verbose: bool }`
  - `src/config.rs: pub fn resolve_config(args: &CliArgs, cwd: &Path) -> Result<Config, String>` (defaults workspace→`.`, output→`./obsidian-graph` joined to `cwd`; both workspace+manifest → `Err("mutually exclusive")`).

- [ ] **Step 1: Write the failing tests**

```rust
// tests/cli_contract.rs
#[test]
fn no_args_default_to_dot_and_default_output() { /* resolve_config(empty, "/work") => workspace_input==".", output=="/work/obsidian-graph" */ }
#[test]
fn workspace_and_manifest_path_conflict_errors() { /* both set => Err containing "mutually exclusive" */ }
#[test]
fn relative_output_resolves_against_cwd_not_workspace() { /* output "../vault/graph", cwd "/a/b" => "/a/vault/graph" */ }
#[test]
fn handshake_constants_are_v04() { /* PROTOCOL_VERSION==1, SPEC_VERSION=="0.4.0", SCHEMA_VERSION==3, GENERATOR=="cargo-obsidian-graph" */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test cli_contract`
Expected: FAIL — file(s)/constant(s) not found.

- [ ] **Step 3: Implement `Cargo.toml` (package `rsdeps2obsidian`, edition 2021, lib + `[[bin]] rsdeps2obsidian`), `src/lib.rs` constants, `src/config.rs` (`CliArgs`, `Config`, `resolve_config`), `src/bin/rsdeps2obsidian.rs` (clap 4 derive: positional `WORKSPACE`, `--manifest-path`, `--output`, `--clean`, `--dry-run`, `--external`, `--verbose`, `-h/--help`, `-V/--version`; calls `resolve_config` with process CWD; conflict → stderr + exit 1; stdin never touched)**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test cli_contract`
Expected: PASS (4/4). Also `cargo run -- --help` exits 0 and shows the eight options.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/lib.rs src/config.rs src/bin/rsdeps2obsidian.rs tests/cli_contract.rs
git commit -m "feat: scaffold package, handshake constants, CLI contract"
```

### Task 2: Canonical IDs and domain model

**Files:**
- Create: `src/ids.rs`
- Create: `src/domain.rs`
- Test: `tests/ids_domain.rs`

**Interfaces:**
- Consumes: nothing from Task 1 except crate root.
- Produces:
  - `src/ids.rs: pub struct PackageId(pub String); pub struct TargetId(pub String); pub struct ModuleId(pub String); pub struct ExternalCrateId(pub String);` (all derive `Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash`)
  - `src/ids.rs: pub fn make_target_id(package: &str, kind_token: &str, target: &str) -> Result<TargetId, String>` (`kind_token` ∈ `{"lib","bin"}`; any component containing `::` → `Err`)
  - `src/ids.rs: pub fn make_module_id(package: &str, target: &str, path: &[String]) -> ModuleId` (`format!("{}::{}::{}", package, target, path.join("::"))`)
  - `src/ids.rs: pub fn make_external_id(name: &str, version: &str) -> ExternalCrateId` (`format!("{name}@{version}")`)
  - `src/domain.rs: pub struct SourceSpan { pub file: PathBuf, pub line_start: u32, pub line_end: u32 }`
  - `src/domain.rs: pub enum TargetKind { Lib, Bin }` + `pub fn kind_token(&self) -> &'static str`
  - `src/domain.rs: pub struct Package { pub id: PackageId, pub name: String, pub manifest_path: PathBuf }`
  - `src/domain.rs: pub struct Target { pub id: TargetId, pub package_id: PackageId, pub name: String, pub kind: TargetKind, pub root_source: PathBuf }`
  - `src/domain.rs: pub struct Module { pub id: ModuleId, pub target_id: TargetId, pub name: String, pub qualified_name: String, pub source_path: Option<PathBuf>, pub source_span: Option<SourceSpan> }`
  - `src/domain.rs: pub enum DependencyTarget { Internal(ModuleId), External(ExternalCrateId) }`
  - `src/domain.rs: pub enum DependencyKind { Module, Use, ReExport }`
  - `src/domain.rs: pub struct Dependency { pub from: ModuleId, pub to: DependencyTarget, pub kind: DependencyKind, pub source_span: Option<SourceSpan> }`
  - `src/domain.rs: pub enum DiagnosticSeverity { Info, Warning, Error }`
  - `src/domain.rs: pub struct Diagnostic { pub severity: DiagnosticSeverity, pub code: String, pub message: String, pub source_span: Option<SourceSpan> }`
  - `src/domain.rs: pub struct WorkspaceGraph { pub packages: Vec<Package>, pub targets: Vec<Target>, pub modules: Vec<Module>, pub dependencies: Vec<Dependency>, pub diagnostics: Vec<Diagnostic> }`

- [ ] **Step 1: Write the failing tests**

```rust
// tests/ids_domain.rs
#[test]
fn target_ids_use_lib_and_bin_tokens() { /* make_target_id("api","lib","api")=="api::lib::api"; ("cli","bin","cli")=="cli::bin::cli" */ }
#[test]
fn target_id_rejects_double_colon_component() { /* package "a::b" => Err; target "x::y" => Err */ }
#[test]
fn package_names_preserved_verbatim() { /* "my-pkg" stays "my-pkg"; "Foo" stays "Foo" (assert full TargetId string) */ }
#[test]
fn external_ids_distinguish_versions() { /* make_external_id("serde","1.0.210") != make_external_id("serde","1.0.190") */ }
#[test]
fn module_id_shape() { /* make_module_id("api","api",&["handler"]) == "api::api::handler" */ }
#[test]
fn id_newtypes_order_and_hash() { /* sorted ModuleIds ascending; HashSet dedups equal ExternalCrateIds */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test ids_domain`
Expected: FAIL — `ids`/`domain` modules not found.

- [ ] **Step 3: Implement `src/ids.rs` and `src/domain.rs` with exactly the structs/enums/constructors above (re-export both modules from `lib.rs`)**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test ids_domain`
Expected: PASS (6/6).

- [ ] **Step 5: Commit**

```bash
git add src/ids.rs src/domain.rs src/lib.rs tests/ids_domain.rs
git commit -m "feat: canonical IDs and domain model"
```

### Task 3: Path safety, filename mapping, Windows policy

**Files:**
- Create: `src/paths.rs`
- Test: `tests/paths.rs`

**Interfaces:**
- Consumes: `ModuleId` from Task 2.
- Produces:
  - `src/paths.rs: pub fn resolve_output_root(raw: &Path, cwd: &Path) -> Result<PathBuf, String>` (relative→`cwd` join; walk existing chain, any symlink component → `Err("unsafe output path")`; existing root→canonicalize; missing→canonicalize nearest existing non-symlink parent + append lexical tail)
  - `src/paths.rs: pub fn validate_generated_path(root: &Path, rel: &str) -> Result<PathBuf, String>` (reject absolute/`..`/symlink components; enforce lexical + canonical component-aware containment in `root`)
  - `src/paths.rs: pub fn module_filename(module_id: &ModuleId) -> String` (split `::`, per-segment `_`→`___`, join `__`, + `.md`)
  - `src/paths.rs: pub fn windows_safe_name(encoded_stem: &str, taken: &mut std::collections::HashSet<String>) -> String` (spec §14.3 order: double colliding leading `_`, prefix reserved base case-insensitively with `_`, trailing space/period→`_`, re-apply doubling until distinct and not reserved; insert result into `taken`; no hashes/counters)

- [ ] **Step 1: Write the failing tests**

```rust
// tests/paths.rs
#[test]
fn underscore_encoding_is_reversible_and_collision_free() {
    // module_filename("api::api::commands::build") == "api__api__commands__build.md"
    // module_filename("api::my__module::handler") == "api__my______module__handler.md"
    // assert_ne!(those two); also single "_" segment => "___"
}
#[test]
fn windows_reserved_names_escaped_with_collision_rule() {
    // windows_safe_name("CON", empty) == "_CON"; with "_CON" taken, windows_safe_name("CON", taken) == "__CON"
    // windows_safe_name("_con", empty) == "__con"; "PRN" => "_PRN"; case preserved otherwise
}
#[test]
fn output_root_rejects_symlink_component() { /* <tmp>/vault symlink + raw <tmp>/vault/graph => Err("unsafe output path") */ }
#[test]
fn generated_paths_cannot_escape_root() { /* validate("/vault/graph", "../evil.md") Err; ("/vault/graph", "/abs.md") Err; ("/vault/graph2" containment vs "/vault/graph") Err */ }
#[test]
fn nonexistent_root_uses_nearest_existing_parent() { /* raw <tmp>/new/sub/dir with <tmp> real => Ok(<canon-tmp>/new/sub/dir) */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test paths`
Expected: FAIL — `paths` module not found.

- [ ] **Step 3: Implement `src/paths.rs`** — algorithm only where tests don't determine it: symlink check via `symlink_metadata` on each existing prefix (never `canonicalize` through); containment by canonicalizing `root` and (for existing dest) dest then `starts_with` on components, plus lexical `rel` rejection of `..`/absolute before any I/O.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test paths`
Expected: PASS (5/5).

- [ ] **Step 5: Commit**

```bash
git add src/paths.rs src/lib.rs tests/paths.rs
git commit -m "feat: path safety, filename mapping, windows policy"
```

### Task 4: Cargo metadata + target discovery

**Files:**
- Create: `src/metadata.rs`
- Create: `tests/fixtures/min-workspace/Cargo.toml` + member crates (`domain` lib with `lib.rs`/`user.rs`, `api` lib + `src/bin/server.rs`)
- Test: `tests/metadata_discovery.rs`

**Interfaces:**
- Consumes: `Package`, `Target`, `TargetKind`, `PackageId`, `make_target_id` (Task 2); `Config` (Task 1).
- Produces:
  - `src/metadata.rs: pub struct DiscoveredWorkspace { pub root: PathBuf, pub packages: Vec<Package>, pub targets: Vec<Target>, pub external_deps: Vec<ExternalDep> }`
  - `src/metadata.rs: pub struct ExternalDep { pub name: String, pub version: String }` (resolved name@version pairs from metadata `resolve`; dependency alias never used for identity)
  - `src/metadata.rs: pub fn discover(config: &Config) -> Result<DiscoveredWorkspace, String>` (one `cargo_metadata::MetadataCommand` with explicit `format_version(1)`; `--manifest-path` or workspace dir; non-lib/bin kinds → skipped; each `Target.root_source` = target `src_path` as given)

- [ ] **Step 1: Write the failing tests**

```rust
// tests/metadata_discovery.rs
#[test]
fn workspace_discovery_finds_packages() { /* discover(fixture min-workspace) => packages ["api","domain"] sorted */ }
#[test]
fn lib_and_bin_targets_get_distinct_namespaces() { /* target ids contain "api::lib::api" and "api::bin::server"; lib root != bin root */ }
#[test]
fn unsupported_target_kinds_are_ignored() { /* fixture with a #[test]/bench-only target or custom kind => still Ok, lib+bin present */ }
#[test]
fn metadata_failure_is_fatal_string() { /* discover(nonexistent dir) => Err */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test metadata_discovery`
Expected: FAIL — `metadata` module / fixtures missing.

- [ ] **Step 3: Implement fixture workspace + `src/metadata.rs: discover`** (single metadata invocation; map `cargo_metadata::Target.kind` containing `"lib"`→`Lib`, `"bin"`→`Bin`; preserve names verbatim; sort packages by `PackageId`, targets by `(package, kind-token, name)`)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test metadata_discovery`
Expected: PASS (4/4).

- [ ] **Step 5: Commit**

```bash
git add src/metadata.rs src/lib.rs tests/metadata_discovery.rs tests/fixtures/min-workspace/
git commit -m "feat: cargo metadata and target discovery"
```

### Task 5: Syn module/use extraction

**Files:**
- Create: `src/resolver.rs` (part 1: parsing + mod walk + use extraction)
- Test: `tests/resolver_extract.rs`

**Interfaces:**
- Consumes: `DiscoveredWorkspace` (Task 4), domain types (Task 2).
- Produces:
  - `src/resolver.rs: pub struct AnalysisInput { pub workspace_root: PathBuf, pub targets: Vec<Target>, pub deps: Vec<crate::metadata::ExternalDep> }`
  - `src/resolver.rs: pub fn parse_source_file(path: &Path) -> Result<syn::File, String>`
  - `src/resolver.rs: pub fn discover_modules_for_target(target: &Target, workspace_root: &Path) -> (Vec<Module>, Vec<Diagnostic>)` (handles file/inline/`#[path]`/`foo.rs`-vs-`foo/mod.rs`→`MOD002`; inline spans 1-based; `#[path]` escape→`PATH001`-family diagnostic + skip; missing candidates→`MOD001` + continue)
  - `src/resolver.rs: pub struct RawUse { pub from: ModuleId, pub path: String, pub is_pub: bool, pub alias: Option<String>, pub span: Option<SourceSpan> }`
  - `src/resolver.rs: pub fn extract_uses(module: &Module, file: &syn::File) -> Vec<RawUse>` (covers `crate::`, `self::`, `super::`, `{a,b}`, nested trees, `as` aliases)

- [ ] **Step 1: Write the failing tests**

```rust
// tests/resolver_extract.rs  (uses tests/fixtures/resolve-fixture/ tree built in-step)
#[test]
fn file_and_inline_modules_discovered() { /* lib.rs with `mod foo;` + `mod api { mod handler {} }` => ModuleIds root, foo, api, api::handler; inline span points at containing file */ }
#[test]
fn path_attribute_respected_and_escaped_rejected() { /* #[path="special/foo.rs"] resolves inside root; #[path="../../evil.rs"] => PATH diagnostic, no module */ }
#[test]
fn ambiguous_layout_prefers_file_rs_with_mod002() { /* foo.rs + foo/mod.rs => module from foo.rs + exactly one MOD002 "ambiguous module layout" warning */ }
#[test]
fn use_trees_aliases_and_pub_use_extracted() { /* crate::{a,b}, crate::foo::{a,b}, `as` alias, pub use => RawUse entries with is_pub + alias preserved, no new ModuleIds */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test resolver_extract`
Expected: FAIL — `resolver` module missing.

- [ ] **Step 3: Implement `src/resolver.rs` part 1** with `syn 2`: walk `Item::Mod` (attrs for `#[path]`/`#[cfg]` detection passthrough), recurse inline `content`; candidate files relative to containing file's directory (`sibling foo.rs`, `sibling foo/mod.rs`, and for `mod.rs`-contained parents the `./foo.rs`-style child — implement exactly: candidate set `{dir/foo.rs, dir/foo/mod.rs}` where `dir` = containing file's parent, or `dir/<modpath-parent>` for `#[path]`-adjusted dirs); `use` traversal via `UseTree` recursion (`Path/Name/Rename/Glob` — Glob → unresolved diagnostic, no edge).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test resolver_extract`
Expected: PASS (4/4).

- [ ] **Step 5: Commit**

```bash
git add src/resolver.rs src/lib.rs tests/resolver_extract.rs tests/fixtures/resolve-fixture/
git commit -m "feat: syn module and use extraction"
```

### Task 6: Ownership resolution, graph build, ordering, cfg

**Files:**
- Modify: `src/resolver.rs` (part 2)
- Create: `src/graph.rs`
- Create: `src/diag.rs`
- Test: `tests/graph_build.rs`

**Interfaces:**
- Consumes: `RawUse`, `discover_modules_for_target`, `AnalysisInput.deps` (Tasks 4–5); `make_external_id` (Task 2).
- Produces:
  - `src/resolver.rs: pub struct SynAnalyzer;` + `pub trait Analyzer { fn analyze(&self, input: &AnalysisInput) -> Result<WorkspaceGraph, String>; }` + `impl Analyzer for SynAnalyzer` (full pipeline: modules → uses → resolve → dedup → sort → diagnostics)
  - `src/graph.rs: pub fn dedup_dependencies(deps: Vec<Dependency>) -> Vec<Dependency>` (key `(from,to)`, `ReExport>Use>Module`)
  - `src/graph.rs: pub fn sort_graph(graph: &mut WorkspaceGraph)` (packages/targets/modules/dependencies order per spec; explicit comparator)
  - `src/diag.rs: pub fn sort_diagnostics(diags: &mut Vec<Diagnostic>)` (severity→code→path→line(missing-first)→message) + `pub fn workspace_rel(path: &Path, root: &Path) -> String` (`/` separators, no `.`/`..`)

- [ ] **Step 1: Write the failing tests**

```rust
// tests/graph_build.rs
#[test]
fn self_super_crate_paths_resolve_internally() { /* `self::foo`, `super::foo`, `crate::foo::bar` in known tree => Internal edges */ }
#[test]
fn external_classification_uses_real_name_and_version() { /* dep serde 1.0.210 + `use serde::de` => External("serde@1.0.210"); renamed dep (alias myserde) => same node, not a second */ }
#[test]
fn ambiguous_ownership_warns_scope001_without_edge() { /* path matching both an internal module and a dep name where tree position is inconclusive => SCOPE001 "scope collision" + unresolved-count++ + zero edges for it */ }
#[test]
fn duplicate_edges_dedup_with_precedence_and_order() { /* (from,to) via Module+Use+ReExport => single ReExport; dependency comparator Internal<External, kind Module<Use<ReExport */ }
#[test]
fn cfg_emits_exactly_one_warning() { /* N items with #[cfg] => exactly one CFG001 "conditional compilation detected - graph reflects unexpanded source" */ }
#[test]
fn diagnostics_sort_total_order() { /* shuffled Info/Warning/Error + codes + paths + lines => expected order incl. missing-line-first */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test graph_build`
Expected: FAIL — `SynAnalyzer`/`graph`/`diag` missing.

- [ ] **Step 3: Implement `Analyzer`+`SynAnalyzer::analyze`, `dedup_dependencies`, `sort_graph`, `sort_diagnostics`, `workspace_rel`**; extend `AnalysisInput` with `deps` and populate it from `cargo_metadata` resolve (match `dep.name`→resolved package `name@version`; alias ignored for identity); resolution rule: `crate/self/super`-rooted paths first against the target's module tree, else first-segment lookup in `deps`, else unresolved diagnostic; inconclusive both-match → `SCOPE001`, no edge.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test graph_build`
Expected: PASS (6/6).

- [ ] **Step 5: Commit**

```bash
git add src/resolver.rs src/graph.rs src/diag.rs src/metadata.rs src/lib.rs tests/graph_build.rs
git commit -m "feat: ownership resolution, graph ordering, diagnostics"
```

### Task 7: JSON, manifest, Markdown rendering

**Files:**
- Create: `src/render.rs`
- Create: `src/manifest.rs`
- Test: `tests/render.rs`

**Interfaces:**
- Consumes: `WorkspaceGraph`, `sort_graph` output (Task 6); `module_filename`, `windows_safe_name`, `workspace_rel` (Tasks 2–3/6); handshake constants (Task 1).
- Produces:
  - `src/render.rs: pub struct RenderedFile { pub rel_path: String, pub content: String }`
  - `src/render.rs: pub fn render_all(graph: &WorkspaceGraph, workspace_root: &Path, external: bool) -> Vec<RenderedFile>` (module notes + optional external notes + `index.md` root; every file frontmatter includes `generator/protocol/spec/schema`; `source` workspace-relative `/`; wikilinks `[[file|id]]` in dependency order)
  - `src/render.rs: pub fn render_graph_json(graph: &WorkspaceGraph) -> String` (handshake envelope + sorted packages/targets/modules/dependencies incl. externals regardless of flag; pretty-printed with trailing newline, deterministic)
  - `src/manifest.rs: pub struct ModuleRecord { pub module_id: String, pub filename: String }`
  - `src/manifest.rs: pub struct Manifest { pub protocol_version: u32, pub spec_version: String, pub schema_version: u32, pub generator: String, pub generated_files: Vec<String>, pub module_records: Vec<ModuleRecord> }`
  - `src/manifest.rs: pub fn build_manifest(rendered: &[RenderedFile], graph: &WorkspaceGraph) -> Manifest` (sorted `generated_files`, records sorted by `module_id`)
  - `src/manifest.rs: pub fn read_manifest(path: &Path) -> Result<Option<Manifest>, String>` (`None` if missing; `Err` if invalid JSON/handshake mismatch)

- [ ] **Step 1: Write the failing tests**

```rust
// tests/render.rs
#[test]
fn module_note_frontmatter_and_links() { /* handler note contains module_id/target_id/package_id, source "crates/api/src/handler.rs", Depends-on [[api__api__model|api::api::model]] */ }
#[test]
fn every_note_carries_generator_marker_and_handshake() { /* all rendered .md contain "generator: cargo-obsidian-graph", protocol 1, spec 0.4.0, schema 3 */ }
#[test]
fn external_markdown_gated_but_json_retained() { /* external=false => no rust-external-crate files but JSON lists serde@x; external=true => external note with graph_type + external_crate_id */ }
#[test]
fn manifest_shape_and_determinism() { /* generated_files sorted, module_records sorted by module_id, handshake 1/0.4.0/3, render twice byte-identical */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test render`
Expected: FAIL — `render`/`manifest` modules missing.

- [ ] **Step 3: Implement `src/render.rs` + `src/manifest.rs`** (frontmatter field order fixed: `graph_type, protocol_version, spec_version, schema_version, generator, module_id, target_id, package_id, [external_crate_id], [source]`; root `index.md` with `graph_type: rust-dependency-root`; external note body `External Cargo dependency.`; JSON keys in handshake order via `serde_json::to_string_pretty` on an ordered struct; thread a single shared `taken: HashSet<String>` through every `module_filename` → `windows_safe_name` call so collisions resolve across the whole output set)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test render`
Expected: PASS (4/4).

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/manifest.rs src/lib.rs tests/render.rs
git commit -m "feat: markdown, JSON, and manifest rendering"
```

### Task 8: Atomic commit, clean, dry-run, orphans

**Files:**
- Create: `src/commit.rs`
- Test: `tests/commit.rs`

**Interfaces:**
- Consumes: `RenderedFile`, `Manifest`, `read_manifest` (Task 7); `resolve_output_root`, `validate_generated_path` (Task 3); `Config` (Task 1).
- Produces:
  - `src/commit.rs: pub struct Plan { pub create: Vec<String>, pub update: Vec<String>, pub delete: Vec<String> }`
  - `src/commit.rs: pub fn compute_plan(desired: &[RenderedFile], prev: Option<&Manifest>) -> Plan` (missing/invalid prev → `delete` empty)
  - `src/commit.rs: pub fn commit(rendered: &[RenderedFile], manifest: &Manifest, output_root: &Path, prev: Option<&Manifest>, dry_run: bool) -> Result<Plan, String>` (dry-run → compute only, zero writes; else stage temps in same dir → flush/fsync → atomic rename each Markdown → stage/flush/fsync/rename manifest → then delete obsolete prev-only entries inside root; cleanup failures → still `Ok` with warning diagnostics out-of-band)
  - `src/commit.rs: pub fn clean(output_root: &Path, manifest: &Manifest) -> Result<Vec<String>, String>` (only listed entries, each re-validated; never orphans/unrelated)
  - `src/commit.rs: pub fn detect_orphans(output_root: &Path, prev: Option<&Manifest>) -> Vec<String>` (`.md` + frontmatter `generator: cargo-obsidian-graph` + `protocol_version: 1` + not in prev → `orphan generated file: <name>` candidates)

- [ ] **Step 1: Write the failing tests**

```rust
// tests/commit.rs
#[test]
fn dry_run_writes_nothing() { /* commit(..., dry_run=true) => plan correct + output dir still empty */ }
#[test]
fn manifest_is_commit_record_and_cleanup_after() { /* normal commit writes md + manifest; stale prev-only file deleted only after manifest rename (assert manifest mtime < delete or presence sequencing via two-phase spy) */ }
#[test]
fn clean_deletes_only_listed_entries() { /* unrelated .md + orphan survive; invalid manifest => Err + zero deletes; missing manifest => Ok(empty) */ }
#[test]
fn orphans_reported_never_deleted() { /* generator-marked unlisted file => detect_orphans lists it; commit+clean leave it on disk */ }
#[test]
fn crash_points_keep_old_manifest_authoritative() { /* simulate interruption (a) after Markdown publish before manifest rename, (b) after manifest temp write before rename, (c) after rename before stale cleanup => in (a)/(b) previous manifest still reads as valid and new files are orphan candidates; in (c) new manifest authoritative with leftovers reported as orphans; never auto-deleted */ }
#[test]
fn symlink_escape_rejected() { /* output through symlinked parent => resolve fails "unsafe output path", commit never starts */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test commit`
Expected: FAIL — `commit` module missing.

- [ ] **Step 3: Implement `src/commit.rs`** following the 19-step §27 order verbatim (temps via `tempfile::NamedTempFile` in destination dir + `persist`/rename; `File::sync_all` where supported, best-effort on Windows; deletions only after manifest rename, only prev-listed, each through `validate_generated_path`)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test commit`
Expected: PASS (6/6).

- [ ] **Step 5: Commit**

```bash
git add src/commit.rs src/lib.rs tests/commit.rs
git commit -m "feat: atomic commit, clean, dry-run, orphans"
```

### Task 9: End-to-end wiring, golden fixtures, determinism

**Files:**
- Modify: `src/bin/rsdeps2obsidian.rs`, `src/lib.rs` (public `run(config: &Config) -> Result<RunSummary, String>` + `RunSummary { workspace, packages, targets, modules, dependencies, external, create, update, delete, warnings }`)
- Create: `tests/fixtures/full-workspace/` (lib + 2 bins + inline/nested/`#[path]`/alias/`pub use`/dupes/external/cfg/ambiguous `qux.rs`+`qux/mod.rs`/reserved-name `con` module)
- Test: `tests/e2e_golden.rs`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `run()` orchestration: `resolve_output_root` → `discover` → `analyze` → `sort` → `render_all`+`render_graph_json` → `read_manifest` → `detect_orphans` (stderr) → `commit` (or plan-only for dry-run) → stdout summary `workspace/packages/targets/modules/dependencies/external + create/update/delete + warnings + status: success`
  - Golden snapshots at `tests/snapshots/` (Markdown bytes + JSON bytes + manifest bytes + sorted diagnostics) — created by this task, asserted byte-equal thereafter

- [ ] **Step 1: Write the failing tests**

```rust
// tests/e2e_golden.rs
#[test]
fn full_run_produces_golden_bytes() { /* run(full-workspace, external=true) => snapshot files match byte-for-byte */ }
#[test]
fn repeated_runs_are_identical() { /* run twice => identical md/json/manifest/diagnostics bytes */ }
#[test]
fn dry_run_leaves_tree_untouched() { /* --dry-run => zero files created, plan printed */ }
#[test]
fn stdout_stderr_split() { /* harness captures: stdout has "status: success" + counts, no "warning["; stderr has warning[CFG001]/warning[SCOPE001], no JSON */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test e2e_golden`
Expected: FAIL — `run()` / fixtures / snapshots missing.

- [ ] **Step 3: Implement `run()` wiring + fixture + generate snapshots** (wire bin to print summary to stdout, diagnostics to stderr; `--clean` path requires valid manifest; `--dry-run --clean` observational; `source` fields workspace-relative `/`; run once to create `tests/snapshots/`, eyeball diff for spec conformance, then lock)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test` (full suite)teriore
Expected: PASS — entire workspace green; run `cargo test` twice and diff `tests/snapshots/` — identical.

- [ ] **Step 5: Commit**

```bash
git add src/bin/rsdeps2obsidian.rs src/lib.rs tests/e2e_golden.rs tests/fixtures/full-workspace/ tests/snapshots/
git commit -m "feat: end-to-end wiring and golden fixtures"
```

---

## Acceptance mapping (spec AC1–AC34 → tasks)

AC1 workspace discovery → 4; AC2 multi-target → 4; AC3 modules → 5; AC4 `#[path]` → 5; AC5 imports → 5; AC6 re-export → 5/6; AC7 dedup → 6; AC8 stable IDs → 2/6; AC9 rendering → 7; AC10 clean → 8; AC11 safe paths → 3/8; AC12 dry-run → 8; AC13 determinism → 6/7/9; AC14 cfg → 6; AC15 recovery → 8; AC16 orphans → 8; AC17 domain types → 2; AC18 target mapping → 2/4; AC19 no-arg CLI → 1; AC20 ordering → 6; AC21 windows policy → 3; AC22 pipes → 1/9; AC23 scope collision → 6; AC24 handshake → 1/7; AC25 output base → 1/3; AC26 reversible mapping → 3; AC27 symlink root → 3/8; AC28 orphan marker → 7/8; AC29 version-distinct externals → 2/6; AC30 ambiguous layout → 5; AC31 workspace-relative sources → 6/7; AC32 external JSON determinism → 7; AC33 diag order → 6; AC34 cleanup ordering → 8.
