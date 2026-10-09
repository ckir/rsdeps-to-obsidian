# Python Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Python backend (`PyAnalyzer`) to `rsdeps2obsidian` behind `--lang`, reusing the entire render/commit pipeline.

**Architecture:** New `src/ts_parse.rs` shared tree-sitter helper; new `src/discovery_py.rs` (project → `Package`/`Target`/`ExternalDep`) and `src/resolver_py.rs` (`PyAnalyzer: Analyzer`); `run()` dispatches discovery + analyzer per resolved language. Rust path untouched except `--lang` plumbing and the 0.5.0 re-stamp.

**Tech Stack:** Rust (edition 2021), `tree-sitter 0.27` + `tree-sitter-python 0.25` (both verified resolvable), existing `clap`/`serde`/`toml` (add `toml` only if not already present — check `Cargo.toml`/`Cargo.lock` first; no other new deps).

**Spec:** `docs/superpowers/specs/2026-10-09-multilang-python-typescript-design.md` (§§0–2, §4; §3 TypeScript is explicitly NOT this plan — a separate TS plan follows).

## Global Constraints

- Handshake after this plan: `protocol_version = 1`, `spec_version = "0.5.0"`, `schema_version = 3`, `generator = "cargo-obsidian-graph"` everywhere (code, goldens, manifest).
- `TargetKind::{Lib, Bin}` + `lib`/`bin` tokens reused unchanged; no new kind tokens; `ModuleId` shape `{pkg}::{tgt}::{path}`; `ExternalCrateId` stays `name@version`.
- New diagnostic code `EXT001` (Warning), exact text `unresolved external: <src> -> <path>; no pinned version` (+ standard `at <file>:<line>` suffix). `SCOPE001` text/behavior unchanged in all languages. Single-run `CFG001` stays Rust-only.
- `LANG001` (Info), exact text `multiple project files detected; analyzing <chosen> (<lang>), ignoring: <others>` — emitted only when autodetect sees >1 project file AND no explicit `--lang`; single-project runs are silent.
- `AnalysisInput{workspace_root, targets, deps}` unchanged. `Analyzer` trait unchanged. Determinism rules unchanged (sort at boundaries, no hashmap-order leaks).
- Pin-or-diagnostic: versioned `External` node only from `==` pins or poetry/pdm/uv lockfiles; otherwise EXT001 + no edge. Stdlib/core names silently skipped (no node, no diagnostic).
- No golden hand-editing (regen via update mode + eyeball + lock). Rust goldens change ONLY in `0.4.0`→`0.5.0` lines (assert by diff inspection).
- `--lang` takes a single value (`auto|rust|python|typescript`, default `auto`); repeats → last wins (clap default, documented in `--help` text written by implementer).

## Review Focus

- Project with BOTH `src/` dir AND top-level importable `.py` files — expect src-layout win (first-existing-wins), flat files ignored for Lib target, pinned in Task 3's tests.
- `pkg.py` file vs `pkg/__init__.py` package in the same dir — expect directory wins + one Warning `MOD003` with exact text `ambiguous python layout: both <file> and <pkgdir>/__init__.py exist; using package`, pinned in Task 4's tests.
- Relative import climbing above the import root (`from ... import x` at top level) — expect unresolved diagnostic + no edge (never a fabricated parent), pinned in Task 4's tests.
- `if TYPE_CHECKING:`-only import of an uninstalled third-party name — expect treated normally (EXT001, not silently skipped), pinned in Task 4's tests.
- `requirements.txt` with `-r other.txt`, `-e .`, `pkg; python_version>"3.8"`, `#` comments, blank lines — expect no choke; only `name==version` lines yield versions, pinned in Task 3's tests.

---

### Task 1: tree-sitter deps + shared parse helper

**Files:**
- Modify: `Cargo.toml` (add `tree-sitter 0.27`, `tree-sitter-python 0.25`; add `package.rust-version` = the `rust-version` from the vendored `tree-sitter-0.27` registry source after fetch — read it, do not guess; if the repo already declares one, keep the max)
- Create: `src/ts_parse.rs`
- Test: `tests/ts_parse.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `src/ts_parse.rs: pub fn parse(language: tree_sitter::Language, src: &str) -> Result<tree_sitter::Tree, String>` (`set_language`/`parse` failures → `Err`, never panic)
  - `src/ts_parse.rs: pub struct MatchedNode { pub capture: String, pub text: String, pub start_row: usize, pub end_row: usize }` (rows 0-based as reported; consumers add 1 for `SourceSpan`)
  - `src/ts_parse.rs: pub fn query_nodes(tree: &tree_sitter::Tree, language: &tree_sitter::Language, src: &str, query_src: &str) -> Result<Vec<MatchedNode>, String>` (bad query → `Err` with the query error)

- [ ] **Step 1: Write the failing test**

```rust
// tests/ts_parse.rs
#[test]
fn python_import_queries_capture_with_rows() {
    // src = "import os\nfrom pkg.mod import Thing as T\n"
    // parse(tree_sitter_python::LANGUAGE.into(), src) is Ok, has_error false
    // query_nodes("(import_statement) @imp") => 1 node, text "import os", start_row 0
    // query_nodes("(import_from_statement) @from") => 1 node, start_row 1
}
#[test]
fn bad_query_is_err_and_error_tree_no_panic() {
    // query_nodes("(unclosed") is Err; parse of "def f(:\n" is Ok with has_error true
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test ts_parse`
Expected: FAIL — `ts_parse` module / crates not found.

- [ ] **Step 3: Implement deps + `src/ts_parse.rs`** (`parse`, `MatchedNode`, `query_nodes` per signatures; `pub mod ts_parse;` in `lib.rs`; set `rust-version` per vendored source)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test ts_parse && cargo test && cargo clippy --all-targets`
Expected: PASS, full suite green, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/ts_parse.rs src/lib.rs tests/ts_parse.rs
git commit -m "feat: tree-sitter deps and shared parse helper"
```

### Task 2: `--lang` plumbing, registry, 0.5.0 re-stamp

**Files:**
- Modify: `src/config.rs` (`CliArgs` + `Config` + `--lang`), `src/lib.rs` (`SPEC_VERSION`, registry, `discover_for`, `run()` dispatch), `src/bin/rsdeps2obsidian.rs` (flag threading, unchanged summary shape)
- Test: `tests/lang_select.rs`

**Interfaces:**
- Consumes: `Config`, `DiscoveredWorkspace`, `Analyzer`/`SynAnalyzer` (existing).
- Produces:
  - `src/config.rs: #[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)] pub enum Lang { Auto, Rust, Python, Typescript }` + `CliArgs.lang: Lang` (default `Auto`) + `Config.lang: Lang`
  - `src/config.rs: pub fn detect_language(dir: &Path) -> Lang` (priority `Cargo.toml` > `pyproject.toml`/`setup.py` > `package.json`; none → `Rust`)
  - `src/lib.rs: pub const SPEC_VERSION: &str = "0.5.0"`
  - `src/lib.rs: pub fn analyzer_for(lang: Lang) -> Result<Box<dyn Analyzer>, String>` (Rust → `SynAnalyzer`; Python/Typescript → `Err("python support not yet implemented")` / `Err("typescript support not yet implemented")` — exact texts, replaced by later plans)
  - `src/lib.rs: pub fn discover_for(lang: Lang, config: &Config) -> Result<DiscoveredWorkspace, String>` (Rust → `metadata::discover`; others → same `Err` texts as above)
  - `run()` resolves (`explicit flag` else `detect_language(workspace_input)`), uses both fns, and appends the `LANG001` Info diagnostic post-analyze exactly when autodetect saw >1 project file with no explicit flag.

- [ ] **Step 1: Write the failing test**

```rust
// tests/lang_select.rs
#[test]
fn detect_priority_and_default() { /* Cargo+pyproject => Rust; pyproject+package.json => Python; package.json only => Typescript; empty dir => Rust */ }
#[test]
fn registry_rust_ok_others_err_exact() { /* analyzer_for/discover_for Rust Ok; Python/Typescript Err with exact texts */ }
#[test]
fn rust_run_still_works_through_registry() { /* run() on tests/fixtures/min-workspace succeeds, summary counts unchanged */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test lang_select`
Expected: FAIL — `Lang`/`detect_language`/`analyzer_for` missing.

- [ ] **Step 3: Implement Task 2 code**

`config` enum + flag + detection; `lib.rs` registry (`analyzer_for`, `discover_for` with Rust arms live, Python/Typescript arms `Err` exact texts) + `run()` dispatch + LANG001 append; `SPEC_VERSION` bump; bin threads the flag, summary/exit contract unchanged.

- [ ] **Step 4: Re-stamp Rust goldens and inspect**

Run e2e update mode once, then inspect `git diff tests/snapshots/` — accept ONLY `0.4.0`→`0.5.0` line changes, else BLOCKED (report the offending hunks, do not lock).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test lang_select && cargo test && cargo clippy --all-targets`
Expected: PASS all, goldens re-stamped, full suite green.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/lib.rs src/bin/rsdeps2obsidian.rs tests/lang_select.rs tests/snapshots/
git commit -m "feat: --lang plumbing, analyzer registry, 0.5.0 re-stamp"
```

### Task 3: Python project discovery

**Files:**
- Create: `src/discovery_py.rs`
- Test: `tests/discovery_py.rs` (+ `tests/fixtures/py-*-workspace/` dirs as needed)

**Interfaces:**
- Consumes: `Config`, `DiscoveredWorkspace{root,packages,targets,external_deps}`, `ExternalDep{name,version}`, `Package`, `Target`, `TargetKind`, `PackageId`, `make_target_id` (all existing); `discover_for` (Task 2 — replace its Python `Err` arm with `discover_py`).
- Produces:
  - `src/discovery_py.rs: pub fn discover_py(config: &Config) -> Result<DiscoveredWorkspace, String>` (root rule pyproject > setup.py/setup.cfg/requirements > CWD; `toml` crate ONLY if absent from `Cargo.lock` — check first, std-only line parsing otherwise documented as approximation is forbidden: use `toml` for pyproject/poetry/pdm/uv locks, plain line scans for setup.cfg/requirements)
  - Package name `[project] name`, fallback root dir name; `manifest_path` = pyproject/setup.py path, fallback root; import root = `src/` if exists else root (first-existing-wins; flat `.py` files ignored for the Lib target when `src/` wins)
  - One Lib target `{pkg}::lib::{pkg}` (`root_source` = import root); Bin targets `{pkg}::bin::{stem}` per top-level `*.py` outside packages + per `__main__.py`; `[project.scripts]` entry `foo = "pkg.mod:main"` whose module file exists → Bin target named `foo` (not stem), else stem; table never executed
  - `external_deps`: `==` pins from `requirements*.txt` + `[project] dependencies`, `name`/`version` pairs from `poetry.lock`/`pdm.lock`/`uv.lock` when present; everything else ignored for versions (resolver emits EXT001)

- [ ] **Step 1: Write the failing test**

```rust
// tests/discovery_py.rs
#[test]
fn src_layout_wins_and_flat_ignored_for_lib() { /* root with src/web/ + stray top.py => Lib root_source ends src/, packages ["web"] */ }
#[test]
fn names_and_bins() { /* no-pyproject dir "demo" => package demo, manifest=root; top-level run.py + pkg/__main__.py => Bin demo::bin::run + demo::bin::__main__-equivalent; [project.scripts] app="web.cli:main" with src/web/cli.py => Bin demo::bin::app */ }
#[test]
fn pins_and_lockfiles() { /* requirements with ==, -r, -e, markers, comments => only == pairs + poetry.lock pairs in external_deps, sorted */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test discovery_py`
Expected: FAIL — `discovery_py` module missing.

- [ ] **Step 3: Implement `src/discovery_py.rs: discover_py`** (root/name/manifest/import-root/Lib/Bin/scripts-table/versions per above; targets sorted `(package, kind-token, name)` like Rust; `pub mod discovery_py;` in `lib.rs`)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test discovery_py && cargo test && cargo clippy --all-targets`
Expected: PASS, full suite green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/discovery_py.rs src/lib.rs tests/discovery_py.rs tests/fixtures/py-*-workspace/
git commit -m "feat: python project discovery"
```

### Task 4: `PyAnalyzer` extraction + resolution

**Files:**
- Create: `src/resolver_py.rs`
- Test: `tests/resolver_py.rs` (+ `tests/fixtures/py-resolve/` tree)

**Interfaces:**
- Consumes: `AnalysisInput` (unchanged), `ts_parse::{parse, query_nodes}`, `make_module_id`/`make_external_id`, all domain types, `graph::{dedup_dependencies, sort_graph}`, `diag::sort_diagnostics`, EXT001 exact text, `ModuleId` §9.3 shape (root `{pkg}::{tgt}`).
- Produces:
  - `src/resolver_py.rs: pub struct PyAnalyzer;` + `impl Analyzer for PyAnalyzer` (per-target walk → extract → resolve → dedup → sort; calls shared `graph`/`diag` fns, never reimplements them)
  - Walk: `.py` files under import root; `__init__.py` = package module (no extra segment); namespace dirs (no `__init__`) walked as packages; `pkg.py` vs `pkg/__init__.py` → directory wins + Warning `MOD003` exact text `ambiguous python layout: both <file> and <pkgdir>/__init__.py exist; using package`, continue; `Bin` file targets = single root module each
  - Extract (private `PyUse`; do NOT reuse `RawUse`): `(import_statement)` full dotted + alias, `(import_from_statement)` module dotted + level (dot count) + imported names + aliases
  - Resolve: absolute vs owning tree longest-prefix (amendment-A rule); relative = anchor at importer by level then same rule (climb-above-root → unresolved diagnostic, no edge); third-party first segment → version in `deps` ? `External` : EXT001 exact text; stdlib static list → silent skip (list = UNION of CPython 3.11–3.14 `sys.stdlib_module_names`, embedded sorted + unit test asserting sortedness and spot members `os sys json typing asyncio pathlib`; generation source documented in code comment); item-level remainder → SCOPE001 exact text (accepted behavior, pinned by test); `TYPE_CHECKING`-only imports treated normally

- [ ] **Step 1: Write the failing test**

```rust
// tests/resolver_py.rs
#[test]
fn modules_and_ids() { /* flat+src fixture: web/__init__, web/api(+handler via from-import file), namespace pkg, Bin script => ModuleIds web::web, web::web::api, ...; __init__ adds no segment */ }
#[test]
fn mod003_and_overclimb() { /* pkg.py + pkg/__init__.py => dir wins + exactly one MOD003 Warning verbatim; from ... import x at top => unresolved diagnostic, no edge */ }
#[test]
fn import_semantics() { /* from . import sib => Internal; from ..pkg import m => Internal; import os => silent (no node, no diag); from pkg import Item => SCOPE001 exact (item remainder); unpinned third-party => EXT001 exact `unresolved external: <src> -> <path>; no pinned version`; pinned (deps) => External(name@version); TYPE_CHECKING-only unpinned => EXT001 */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test resolver_py`
Expected: FAIL — `resolver_py` module missing.

- [ ] **Step 3: Implement `src/resolver_py.rs`** (walk/extract/resolve per above; single-run CFG001 NOT emitted — code comment citing spec Rust-only rule; `pub mod resolver_py;` + register `PyAnalyzer` in `analyzer_for`)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test resolver_py && cargo test && cargo clippy --all-targets`
Expected: PASS, full suite green.

- [ ] **Step 5: Commit**

```bash
git add src/resolver_py.rs src/lib.rs tests/resolver_py.rs tests/fixtures/py-resolve/
git commit -m "feat: python module and import resolution"
```

### Task 5: Python e2e goldens

**Files:**
- Create: `tests/fixtures/py-workspace/` (pyproject named package, src-layout, `__init__`/nested/namespace/relative/alias/`pub`-equivalent-n/a-documented/`TYPE_CHECKING`/item-level/ambiguous-MOD003/unpinned+pinnd externals/`__main__` + top script, requirements with `==` + noise lines)
- Create: `tests/snapshots/py/`
- Test: `tests/py_golden.rs`

**Interfaces:**
- Consumes: `run()` + `--lang python` (all prior tasks).
- Produces: golden bytes (notes + `graph.json` + `manifest.json` + stdout/stderr) + determinism proof, mirroring `tests/e2e_golden.rs` shape (update mode documented single-threaded; NO hand-editing).

- [ ] **Step 1: Write the failing test**

```rust
// tests/py_golden.rs
#[test]
fn python_full_run_produces_golden_bytes() { /* run(py-workspace, external=true) matches tests/snapshots/py/ byte-for-byte */ }
#[test]
fn python_repeated_runs_identical_and_dry_run_clean() { /* twice identical; --dry-run zero files */ }
#[test]
fn python_stdout_stderr_split() { /* stdout counts+status, no warnings/JSON; stderr has EXT001/SCOPE001/MOD003 lines as fixture dictates, no JSON */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test py_golden`
Expected: FAIL — fixture/`run()` python path missing.

- [ ] **Step 3: Implement fixture + run once in update mode, EYEBALL `git diff tests/snapshots/py/`** (every note/edge/diagnostic traces to Tasks 3–4 rules; handshake `1/0.5.0/3`; no Rust-snapshot changes), then lock; bin-level `--lang python` test against temp output dir included.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test py_golden && cargo test && cargo test && cargo clippy --all-targets` (suite twice for determinism)
Expected: PASS throughout.

- [ ] **Step 5: Commit**

```bash
git add tests/py_golden.rs tests/fixtures/py-workspace/ tests/snapshots/py/ src/lib.rs src/bin/rsdeps2obsidian.rs
git commit -m "feat: python end-to-end goldens"
```

---

## Acceptance mapping (spec §§0–2, §4 → tasks)

§0.1 shape → plan structure; §0.2 Lib/Bin → T3; §0.3 versions → T3 (+T4 EXT001); §0.4 TS scope → NOT this plan; §0.5 handshake → T2; §0.6 separate runs → T2 autodetect. §1 helper → T1; modules → T2–T4; `--lang` → T2; deps/MSRV → T1. §2 discovery → T3; IDs → T3/T4; extraction → T4; resolution → T4. §4 shared (`--lang`, docs, SPEC bump, registry) → T2; EXT001 → T4; testing → T5; sequencing → this plan first. §5: MSRV → T1 (vendored source, no guessing); stdlib list → T4 (union + sortedness test); Bin edge cases → T3 (namespace `__main__` in scope as file walk, non-`.py` executables out, documented in code comment); `--lang` repeats → T2 (last wins).
