# C# Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a C# backend (`CsAnalyzer`) behind the existing `--lang` as value `csharp`, reusing the shared tree-sitter helper and the entire render/commit pipeline.

**Architecture:** Add `tree-sitter-c-sharp 0.23` only; new `src/discovery_cs.rs` (csproj/sln → `Package`/`Target`/`ExternalDep`) and `src/resolver_cs.rs` (`CsAnalyzer: Analyzer`); replace the `discover_for`/`analyzer_for` csharp arms (currently `Err`, added by the `--lang` plumbing — verify exact texts before replacing); C# goldens under `tests/snapshots/cs/`. No helper-signature changes expected; no handshake change (`1/0.5.0/3` already multilang).

**Tech Stack:** Rust (edition 2021), `tree-sitter 0.27` (present) + `tree-sitter-c-sharp 0.23` (new; `LANGUAGE` export), `serde`+`serde_json` for MSBuild XML? — NO: csproj is XML, not JSON. XML parsing needs a crate: use `quick-xml` (check `Cargo.toml`/`Cargo.lock` first — add `quick-xml` ONLY if absent; versions/elements needed are tiny: `TargetFramework(s)`, `PackageReference`/`Version`/`VersionOverride`, `ProjectReference@Include`, `Compile@Include|Remove`, `OutputType`, `AssemblyName`, `ImplicitUsings`, `LangVersion`-ignored). No MSBuild evaluation, no shell-outs (file-only, never `dotnet`).

**Spec:** `docs/superpowers/specs/2026-10-10-csharp-backend-design.md` (§§1–9; Python/TS specs are precedent only).

## Global Constraints

- Handshake UNCHANGED: `protocol_version = 1`, `spec_version = "0.5.0"`, `schema_version = 3`, `generator = "cargo-obsidian-graph"`. No re-stamp task; Rust + Python + TS goldens must show ZERO hunks (assert by `git status` on their snapshot dirs).
- `--lang` value is exactly `csharp` (CLI-safe spelling); autodetect gains `*.sln`/`*.csproj` between Cargo.toml and pyproject (priority: `Cargo.toml` > `*.sln`/`*.csproj` > `pyproject.toml`/`setup.py` > `package.json`); single value (repeats → clap error, settled).
- Reuse `TargetKind::{Lib, Bin}` + tokens; `ModuleId` shape (dotted namespace → `::` joins; namespace-less files = project-relative path segments + stem); `ExternalCrateId name@version`; SCOPE001 + EXT001 exact texts reused verbatim (`EXT001`: `unresolved external: <src> -> <path>; no pinned version`); CFG001 exact text reused once per run when `#if` is encountered; NO new diagnostic codes.
- `AnalysisInput`/`Analyzer`/graph/diag APIs unchanged (per-target csproj re-read precedent from TS tsconfig handling — no struct changes).
- Multi-target suffix `-<tfm>` (verbatim TFM string) ONLY when plural TFMs; single-TFM keeps plain `{pkg}::lib::{pkg}`; per-(project,TFM) full module sets; `#if` parsed inclusively, no evaluation.
- Determinism rules unchanged. No golden hand-editing. `Directory.Build.props` NOT evaluated (unresolved diagnostic where versions live only there).

## Review Focus

- Same namespace declared in TWO referenced projects, both imported — expect ambiguity diagnostic + no edge (never a fabricated cross-project link), pinned in Task 3's tests.
- `using static` naming an unknown type — expect SCOPE001 (full path unresolvable), not an edge to a guessed module, pinned in Task 3's tests.
- Floating/range `PackageReference` version (`[6.*, *)`, `6.*`) — expect unresolved diagnostic + no edge (no NuGet solver), pinned in Task 3's tests.
- `global using` in file A enabling resolution of an import in file B — expect the edge (project-level context proven), pinned in Task 3's tests.
- Legacy csproj with explicit `Compile` list (Include 2 files, recursive glob would find 3) — expect exactly the 2 listed files as modules, pinned in Task 3's tests.

---

### Task 1: tree-sitter-c-sharp dep + query coverage

**Files:**
- Modify: `Cargo.toml`/`Cargo.lock` (add `tree-sitter-c-sharp 0.23` ONLY), `tests/ts_parse.rs` (additive C# cases — existing assertions untouched)
- Test: `tests/ts_parse.rs` (extended)

**Interfaces:**
- Consumes: `ts_parse::{parse, query_nodes}` (unchanged — no helper changes expected; if proven necessary, smallest possible + documented reason).
- Produces: dep available; proof that `LANGUAGE` parses block/file-scoped namespaces, all `using` shapes, classes, top-level statements, and that `(using_directive)` / `(namespace_declaration)` / `(file_scoped_namespace_declaration)` / `(class_declaration)` capture through the shared helper.

- [ ] **Step 1: Write the failing test**

```rust
// tests/ts_parse.rs (append)
#[test]
fn csharp_namespace_and_using_queries_capture() {
    // src: "using System;\nusing static System.Math;\nusing IO = System.IO;\nglobal using System.Collections.Generic;\nnamespace MyApp.Services\n{\n    public partial class Handler { }\n}\n"
    //   via tree_sitter_c_sharp::LANGUAGE: has_error false; (using_directive) => 4; (namespace_declaration) => 1; (class_declaration) => 1
    // file-scoped src: "namespace MyApp.Other;\npublic class Worker {}\n" => (file_scoped_namespace_declaration) => 1, (namespace_declaration) => 0
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test ts_parse`
Expected: FAIL — `tree_sitter_c_sharp` crate not found.

- [ ] **Step 3: Implement dep + tests** (add grammar dep; append C# cases; `cargo build` green)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test ts_parse && cargo test && cargo clippy --all-targets`
Expected: PASS, full suite green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock tests/ts_parse.rs
git commit -m "feat: tree-sitter-c-sharp dep and query coverage"
```

### Task 2: C# project discovery

**Files:**
- Create: `src/discovery_cs.rs`
- Test: `tests/discovery_cs.rs` (+ `tests/fixtures/cs-*-workspace/` dirs)

**Interfaces:**
- Consumes: `Config`, `DiscoveredWorkspace`, `ExternalDep`, `Package`, `Target`, `TargetKind`, `PackageId`, `make_target_id`, `discover_for` (replace ONLY its csharp `Err` arm — Rust/Python/Typescript arms byte-identical).
- Produces:
  - `src/discovery_cs.rs: pub fn discover_cs(config: &Config) -> Result<DiscoveredWorkspace, String>` (XML via `quick-xml` ONLY if absent from Cargo.toml/lock — check first, add nothing else)
  - Root rule: nearest `*.sln` (members = projects) else nearest `*.csproj` walking DOWN from input (never above input root) else CWD; loose `*.csproj` = own package each; multiple `.sln` → union of members, deduped
  - Package name: `<AssemblyName>` if present else csproj file stem; `manifest_path` = csproj path
  - Lib targets: one per project, or per (project, TFM) with `-<tfm>` suffix ONLY when `TargetFrameworks` plural (`;`-split, sorted, verbatim strings); `root_source` = project dir; TFM list recorded for resolver use — WITHOUT new struct fields (resolver re-reads the csproj per target; precedent: TS tsconfig re-read)
  - Bin targets: `OutputType=Exe` (case-insensitive) → `{pkg}::bin::{name}` with `root_source` = project dir; Exe projects get Bin target(s) ONLY — no Lib (all their modules belong to the Bin tree; Rust-`main.rs` precedent); multi-TFM Exe splits per TFM like Lib (`Cli-net8.0`); library projects get Lib target(s) ONLY
  - `external_deps`: exact-pin `Version` attributes + `packages.lock.json` + `Directory.Packages.props` (`Version`/`VersionOverride`) + legacy `packages.config`; floating/range/undefined → ignored for versions (resolver emits unresolved diagnostic); `Directory.Build.props` NOT read (documented code comment); deduped + sorted; targets sorted `(package, kind-token, name)`; malformed csproj XML → hard `Err` (fail-closed, mirrors tsconfig rule)

- [ ] **Step 1: Write the failing test**

```rust
// tests/discovery_cs.rs
#[test]
fn roots_names_and_targets() { /* sln with lib Web.csproj (net8.0) + Exe Cli.csproj (net8.0;net48) + loose Tool.csproj (library) => packages [Cli, Tool, Web]; Lib Web::lib::Web; Bin Cli::bin::Cli-net8.0 + Cli::bin::Cli-net48 (Exe: Bin-only, no Lib); loose library Tool => Lib Tool::lib::Tool, zero Bins */ }
#[test]
fn versions_and_refs() { /* exact Version pin + packages.lock.json + Directory.Packages.props VersionOverride + packages.config entry => external_deps with exact versions; floating [6.*) + Build.props-only versions absent (unresolved later, not here) */ }
#[test]
fn malformed_csproj_fails_closed() { /* garbage XML => Err naming the file */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test discovery_cs`
Expected: FAIL — `discovery_cs` module missing.

- [ ] **Step 3: Implement `src/discovery_cs.rs: discover_cs`** (per above; `pub mod discovery_cs;` + wire `discover_for` csharp arm)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test discovery_cs && cargo test && cargo clippy --all-targets`
Expected: PASS, full suite green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/discovery_cs.rs src/lib.rs tests/discovery_cs.rs tests/fixtures/cs-*-workspace/
git commit -m "feat: csharp project discovery"
```

### Task 3: `CsAnalyzer` extraction + resolution

**Files:**
- Create: `src/resolver_cs.rs`
- Test: `tests/resolver_cs.rs` (+ `tests/fixtures/cs-resolve/` tree)

**Interfaces:**
- Consumes: `AnalysisInput` (unchanged), `ts_parse::{parse, query_nodes}`, `make_module_id`/`make_external_id`, domain types, `graph::{dedup_dependencies, sort_graph}`, `diag::sort_diagnostics`, SCOPE001/EXT001/CFG001 texts, ModuleId §9.3 shape (root `{pkg}::{tgt}`).
- Produces:
  - `src/resolver_cs.rs: pub struct CsAnalyzer;` + `impl Analyzer for CsAnalyzer` (per-target walk → extract → resolve → dedup → sort via shared fns) + replacing `analyzer_for`'s csharp `Err` arm (others byte-identical)
  - Walk: `*.cs` recursively under target root minus `obj/`+`bin/` (unless explicit `Compile` Include/Remove lists found in the target's csproj re-read — then exactly those files, same fail-closed/missing semantics as other backends); skip `obj/`+`bin/` always
  - Per file: parse once; each `namespace_declaration`/`file_scoped_namespace_declaration` block = one module (dotted path → segments); namespace-less file (top-level statements only) = one module from relative-path segments + stem; same namespace repeated in one project (partial or not) merges to ONE node (`source_path` = first sorted file, spans per declaration where available); cross-file span navigation documented in code comment
  - Per-target csproj re-read (ancestor walk from `root_source`, same precedent as TS): `ProjectReference` list (for step b below), `ImplicitUsings` enable flag, TFM list (for implicit sets), explicit `Compile` lists
  - Context per target: file usings ∪ project-wide explicit `global using` set (collected in walk phase 1 across the target's files) ∪ pinned per-TFM implicit sets (union only of the target's TFMs; exact lists: `System`, `System.Collections.Generic`, `System.IO`, `System.Linq`, `System.Net.Http`, `System.Threading`, `System.Threading.Tasks` + `System.Text.Json`?? — NO: implementer takes the exact set from the SDK `Microsoft.NET.Sdk` default `ImplicitUsings` list for net6.0–net9.0 (union, sorted, embedded + sortedness unit test + comment citing the SDK source file `Microsoft.NET.Sdk.CSharp.props`); generation source in code comment, as Python stdlib precedent)
  - Extract (private `CsUse`; do NOT reuse other backends' structs): `using X;`, `using static X.Y;` (full type path), `using A = X.Y.Z;` (full path, alias diagnostics-only), `global using` (context only, never an edge by itself)
  - Resolve (longest-prefix throughout): (a) own target's declared namespaces; (b) directly-referenced projects' namespaces (one hop, no transitivity; same namespace in 2+ projects → ambiguity diagnostic SCOPE001 exact text, no edge); (c) `PackageReference` package names exact-first-segment case-insensitive → version known ? `External` : unresolved diagnostic (EXT001 exact text); (d) BCL `System*` pinned prefix list → silent skip (consulted before external, after steps a–b); no match → SCOPE001 exact text; `using static` resolves by full type path through the same order; `#if` encountered → CFG001 exact text once per run (no evaluation)
  - NO new diagnostic codes; NO CFG001-equivalent beyond the reused text

- [ ] **Step 1: Write the failing test**

```rust
// tests/resolver_cs.rs
#[test]
fn modules_namespaces_and_partials() { /* block ns + file-scoped ns + two partial files same ns (one node, first-sorted path) + two namespaces one file (two nodes, shared path) + top-level Program.cs (stem module) => exact ModuleIds */ }
#[test]
fn import_semantics() { /* using own ns => Internal; using static known type => Internal full path; alias => Internal + alias never an ID; global using in file A enables edge from file B; implicit System.* => silent; unpinned NuGet => EXT001 exact; floating version => unresolved diagnostic; cross-project namespaced import => Internal referenced module; same ns in two projects => SCOPE001; #if present => exactly one CFG001 */ }
#[test]
fn review_focus_pins() { /* same-ns-two-projects ambiguity (no edge); using-static-unknown => SCOPE001; legacy Compile Include 2-of-3 files => exactly 2 modules */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test resolver_cs`
Expected: FAIL — `resolver_cs` module missing.

- [ ] **Step 3: Implement `src/resolver_cs.rs`** (walk/extract/resolve per above; `pub mod resolver_cs;` + `analyzer_for` csharp arm)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test resolver_cs && cargo test && cargo clippy --all-targets`
Expected: PASS, full suite green.

- [ ] **Step 5: Commit**

```bash
git add src/resolver_cs.rs src/lib.rs tests/resolver_cs.rs tests/fixtures/cs-resolve/
git commit -m "feat: csharp module and import resolution"
```

### Task 4: C# e2e goldens

**Files:**
- Create: `tests/fixtures/cs-workspace/` (`.sln` with lib Web.csproj + Exe Cli.csproj referencing Web + loose Tool.csproj; partials; file-scoped namespaces; aliases; static + global usings; pinned + floating + CPM-versioned + lockfile externals; top-level Program.cs; multi-namespace file; multi-TFM project to pin suffix rule)
- Create: `tests/snapshots/cs/`
- Test: `tests/ts_golden.rs` — NO: `tests/cs_golden.rs` (name must match language; do not reuse another backend's test file)

**Interfaces:**
- Consumes: `run()` + `--lang csharp` (all prior tasks).
- Produces: golden bytes + determinism proof, mirroring `tests/py_golden.rs` shape (update mode documented single-threaded; NO hand-editing; ZERO hunks outside `tests/fixtures/cs-workspace/`, `tests/snapshots/cs/`, `tests/cs_golden.rs`).

- [ ] **Step 1: Write the failing test**

```rust
// tests/cs_golden.rs
#[test]
fn cs_full_run_produces_golden_bytes() { /* run(cs-workspace, external=true) matches tests/snapshots/cs/ byte-for-byte */ }
#[test]
fn cs_repeated_runs_identical_and_dry_run_clean() { /* twice identical; --dry-run zero files */ }
#[test]
fn cs_stdout_stderr_split() { /* stdout counts+status, no warnings/JSON; stderr carries SCOPE001/EXT001/CFG001 lines as fixture dictates, no JSON */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test cs_golden`
Expected: FAIL — fixture/`run()` csharp path missing.

- [ ] **Step 3: Implement fixture + run once in update mode, EYEBALL `git diff tests/snapshots/cs/`** (every note/edge/diagnostic traces to Tasks 2–3 rules; handshake `1/0.5.0/3`; assert `git status --porcelain tests/snapshots/tree/ tests/snapshots/py/ tests/snapshots/ts/` shows NOTHING), then lock; bin-level `--lang csharp` tempdir test included.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test cs_golden && cargo test && cargo test && cargo clippy --all-targets` (suite twice for determinism)
Expected: PASS throughout.

- [ ] **Step 5: Commit**

```bash
git add tests/cs_golden.rs tests/fixtures/cs-workspace/ tests/snapshots/cs/
git commit -m "feat: csharp end-to-end goldens"
```

---

## Acceptance mapping (spec §§1–9 → tasks; py/ts done)

§1 intent/priority → plan structure (per-file first: Tasks 2–3 build nodes before Task 4 locks edges). §2 goals → T2 (discovery/sln), T3 (namespaces/partials/usings/refs/versions/edges). §3 non-goals → T2 (legacy best-effort + glob fallback + no `dotnet` subprocess), T3 (no generators/analyzers handling — syntax only). §4 anti-goals → T3 (ambiguity-no-edge rule). §5 constraints (single binary, `csharp` value, unchanged structs, S1–S8, no new deps beyond grammar+XML) → T1/T2/T3. §6 decided-hows → T2 (sln-union, TFM suffix, Exe bins, float/range unresolved, Build.props out) + T3 (namespace identity/merges, static/alias/global/implicit rules, top-level fallback, version sources). §7 builder-left → T3 (ImplicitUsings exact lists + sortedness test; TFM verbatim; fixture shape; no probing statement in code). §8 open → T1 (MSRV from build env), T2 (glob exclusions `obj/`+`bin/`; outside-root refs unresolved-no-traversal), §9 resolutions → T3 (all seven: owner mapping order, global/implicit context-only, Id shape/merges, BCL skip-before-external, per-TFM duplication + CFG001 reuse, direct-only ProjectReference edges, CPM+packages.config read / Build.props out / walk-down scope / autodetect order).
