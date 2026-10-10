# TypeScript Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a TypeScript backend (`TsAnalyzer`) behind the existing `--lang`, reusing the shared tree-sitter helper and the entire render/commit pipeline.

**Architecture:** Add `tree-sitter-typescript 0.23` only; new `src/discovery_ts.rs` (package.json/tsconfig → `Package`/`Target`/`ExternalDep`) and `src/resolver_ts.rs` (`TsAnalyzer: Analyzer`); replace the `discover_for`/`analyzer_for` TypeScript `Err` arms; TS goldens under `tests/snapshots/ts/`. No helper-signature changes expected; no handshake change (`1/0.5.0/3` already multilang).

**Tech Stack:** Rust (edition 2021), `tree-sitter 0.27` (present) + `tree-sitter-typescript 0.23` (new; `LANGUAGE_TYPESCRIPT` + `LANGUAGE_TSX`), existing `serde_json` for package.json/tsconfig/package-lock parsing (check `Cargo.toml` first — add nothing else).

**Spec:** `docs/superpowers/specs/2026-10-09-multilang-python-typescript-design.md` (§§0, 1, 3, 4; §2 Python done and merged — behavior precedent where noted, not to be reimplemented).

## Global Constraints

- Handshake UNCHANGED: `protocol_version = 1`, `spec_version = "0.5.0"`, `schema_version = 3`, `generator = "cargo-obsidian-graph"`. No re-stamp task; Rust + Python goldens must show ZERO hunks (assert by `git status` on their snapshot dirs).
- Reuse `TargetKind::{Lib, Bin}` + tokens; `ModuleId` shape; `ExternalCrateId name@version`; EXT001 exact text; SCOPE001 exact text; `AnalysisInput`/`Analyzer` unchanged; determinism rules unchanged.
- New code `MOD004` (Warning), exact text `ambiguous ts layout: both <first> and <second> exist; using <first>` where `<first>` is the first-hit candidate in the pinned probe order below. Mirrors MOD002/MOD003 style.
- Probe order (pinned, first-hit-wins): `x.ts`, `x.tsx`, `x.mts`, `x.cts`, `x/index.ts`, `x/index.tsx`, `x/index.mts`, `x/index.cts`, `x.d.ts`, `x/index.d.ts`. Relative specifier `./x` tries exactly this sequence; first existing regular file wins (+ MOD004 iff a LATER candidate also exists — i.e. warn whenever the hit was not unique).
- `tsconfig paths` mapping: sort keys by descending literal-prefix length (text before `*`), ties broken lexically; first key whose prefix matches wins; `*` substituted; result resolved through the probe order against `baseUrl` (default: tsconfig dir). One mapping attempt per import (no chained remapping).
- Bare specifiers: lockfiles (`package-lock.json`, `npm-shrinkwrap.json`, `yarn.lock`) → `node_modules/<top-level>/<name>/package.json` version → else EXT001 (pin-or-diagnostic, same shape as Python). `node:`-prefixed and core names silently skipped. Package `exports`-map subtleties deferred (found file = edge, documented approximation).
- Own-package bare rule (same as Python ruling): first segment equals own package name (verbatim compare — scoped npm names match verbatim) → strip and longest-prefix-resolve remainder in own Lib trees; zero/multi → SCOPE001 (never EXT001).
- No CFG001 for TypeScript. No golden hand-editing. `--lang` single value (repeats → clap error, settled).

## Review Focus

- `user.ts` vs `user/index.ts` vs `user.tsx` triple collision — expect first-hit (`user.ts`) + one MOD004 naming both losers... (exact: names `<first>` hit + `<second>`; with two losers the message names the hit and the first loser — pin exact behavior in Task 3's test).
- Overlapping `paths` prefixes (`@/*` vs `@/deep/*`) — expect longest-prefix-first mapping, pinned in Task 3's tests.
- `require()` with a non-literal argument — expect silently ignored (no edge, no diagnostic), pinned in Task 3's tests.
- Unpinned bare dep WITH `node_modules/<name>/package.json` present — expect versioned edge from node_modules (fallback before diagnostic), pinned in Task 3's tests (fake node_modules dir in fixture).
- `export * from './x'` with `x` unresolvable — expect SCOPE001 (export-from names a module like import does), pinned in Task 3's tests.

---

### Task 1: tree-sitter-typescript dep + TS/TSX query coverage

**Files:**
- Modify: `Cargo.toml`/`Cargo.lock` (add `tree-sitter-typescript 0.23` ONLY), `tests/ts_parse.rs` (additive TS/TSX cases — no existing assertions touched)
- Test: `tests/ts_parse.rs` (extended)

**Interfaces:**
- Consumes: `ts_parse::{parse, query_nodes}` (unchanged — no helper changes expected; if the implementer proves a helper change necessary, smallest possible + documented reason in report, else BLOCKED-and-ask is wrong: just do it minimally and document).
- Produces: dep available; proof that `LANGUAGE_TYPESCRIPT` + `LANGUAGE_TSX` parse and that `(import_statement)` / `(export_statement)` / `require`-call queries capture through the shared helper.

- [ ] **Step 1: Write the failing test**

```rust
// tests/ts_parse.rs (append)
#[test]
fn ts_and_tsx_import_queries_capture() {
    // TS src: "import x from './a';\nexport * from './c';\nconst z = require('dep');\n"
    //   via LANGUAGE_TYPESCRIPT: has_error false; (import_statement) => 1; (export_statement) => 1
    // TSX src: "import A from './A';\nexport const B = () => <div/>;\n"
    //   via LANGUAGE_TSX: has_error false; (import_statement) => 1
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test ts_parse`
Expected: FAIL — `tree_sitter_typescript` crate not found.

- [ ] **Step 3: Implement dep + tests** (add grammar dep; append TS/TSX cases; `cargo build` green)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test ts_parse && cargo test && cargo clippy --all-targets`
Expected: PASS, full suite green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock tests/ts_parse.rs
git commit -m "feat: tree-sitter-typescript dep and query coverage"
```

### Task 2: TypeScript project discovery

**Files:**
- Create: `src/discovery_ts.rs`
- Test: `tests/discovery_ts.rs` (+ `tests/fixtures/ts-*-workspace/` dirs)

**Interfaces:**
- Consumes: `Config`, `DiscoveredWorkspace`, `ExternalDep`, `Package`, `Target`, `TargetKind`, `PackageId`, `make_target_id`, `discover_for` (replace its TypeScript `Err` arm with `discover_ts`; Rust/Python arms untouched).
- Produces:
  - `src/discovery_ts.rs: pub fn discover_ts(config: &Config) -> Result<DiscoveredWorkspace, String>` (root = nearest `package.json`, else nearest `tsconfig.json`, else CWD; `serde_json` only — verify present first, add nothing)
  - Package name verbatim (`@org/web` kept, incl. `/`); `manifest_path` = package.json path, fallback tsconfig path, fallback root
  - Lib targets: every `tsconfig*.json` under root EXCEPT under `node_modules/` — root `tsconfig.json` → target named package name; `tsconfig.<stem>.json` → target named `<stem>`; same-stem collision across dirs → first sorted path wins, others skipped (code comment, no diagnostic); `root_source` = `compilerOptions.rootDir` if present else tsconfig dir; malformed tsconfig JSON → hard `Err` (fail-closed, mirrors Python manifest rule)
  - Bin targets from `package.json` `bin` table (string values = file paths; object-form values likewise; missing file → emit Bin anyway, resolver fail-closed — mirrors Python miss-arm precedent); absent table → no Bins; non-JS executables out (documented code comment)
  - `external_deps`: versions from `package-lock.json`/`npm-shrinkwrap.json`/`yarn.lock` first, then top-level `node_modules/*/package.json` scan (top level only, nested `node_modules` ignored — documented approximation); deduped + sorted; targets sorted `(package, kind-token, name)`

- [ ] **Step 1: Write the failing test**

```rust
// tests/discovery_ts.rs
#[test]
fn roots_names_and_tsconfigs() { /* package.json @org/web + tsconfig.json (rootDir src) + tsconfig.node.json => Lib @org/web::lib::web (root src/) + Lib @org/web::lib::node; scoped name verbatim */ }
#[test]
fn bins_and_missing() { /* bin {app: ./dist/app.js(existing), ghost: ./nope.js} => Bin app + Bin ghost (both emitted); no table => zero Bins */ }
#[test]
fn versions_lock_then_modules() { /* package-lock pins lodash@4.17.21; node_modules/react/package.json 18.2.0 with NO lock entry; bare unpinned-elsewhere name absent => external_deps exactly {lodash, react} sorted */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test discovery_ts`
Expected: FAIL — `discovery_ts` module missing.

- [ ] **Step 3: Implement `src/discovery_ts.rs: discover_ts`** (per above; per-target tsconfig values are CONSUMED here for `root_source`, but `paths`/`baseUrl` interpretation stays in Task 3 — discovery does NOT pre-resolve aliases; `pub mod discovery_ts;` + wire `discover_for` TS arm)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test discovery_ts && cargo test && cargo clippy --all-targets`
Expected: PASS, full suite green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/discovery_ts.rs src/lib.rs tests/discovery_ts.rs tests/fixtures/ts-*-workspace/
git commit -m "feat: typescript project discovery"
```

### Task 3: `TsAnalyzer` extraction + resolution

**Files:**
- Create: `src/resolver_ts.rs`
- Test: `tests/resolver_ts.rs` (+ `tests/fixtures/ts-resolve/` tree)

**Interfaces:**
- Consumes: `AnalysisInput` (unchanged), `ts_parse::{parse, query_nodes}`, `make_module_id`/`make_external_id`, domain types, `graph::{dedup_dependencies, sort_graph}`, `diag::sort_diagnostics`, MOD004/EXT001/SCOPE001 texts, ModuleId §9.3 shape (root `{pkg}::{tgt}`).
- Produces:
  - `src/resolver_ts.rs: pub struct TsAnalyzer;` + `impl Analyzer for TsAnalyzer` (per-target walk → extract → resolve → dedup → sort via shared fns) + replacing `analyzer_for`'s TypeScript `Err` arm (Rust/Python arms untouched)
  - Walk: `.ts/.tsx/.mts/.cts` under target root (skip `node_modules/` everywhere, skip `.d.ts` silently); module id = tsconfig-relative path segments, `/`→`::`, extension stripped; `index.*` drops its segment (barrel = directory module); NO MOD003-analog for `user.ts` vs `user.tsx` same-dir siblings — both are walked as DISTINCT modules (different paths), collisions handled only at probe time by MOD004
  - tsconfig location per target: ancestor walk from `root_source` (nearest `tsconfig*.json`, skipping `node_modules/`); values re-read here (no `AnalysisInput` change); missing tsconfig → defaults (root = target root, no paths map)
  - Extract (private `TsUse`; do NOT reuse other backends' use structs): `import … from 's'`, `export … from 's'` + `export *`, `require('literal')`, dynamic `import('literal')` (static string only; non-literal → silently ignored, no edge, no diagnostic), `import type` = normal edge
  - Resolve: relative → probe order (Global Constraints) from importing file's dir; on hit: MOD004 iff a later candidate also exists; `paths` prefixes (sorted longest-first, lexical ties) tried before relative?? — NO: relative specifiers (`./`, `../`) NEVER consult `paths`; bare specifiers consult `paths` first, then lockfile/node_modules/external rules; bare own-package-name → own-Lib-tree longest-prefix (Python ruling precedent), zero/multi → SCOPE001; third-party bare with version → `External`, without → EXT001; `node:`-prefixed + core static list → silent skip (list = Node.js builtins per docs: assert async_hooks assert buffer child_process cluster console crypto dgram dns domain events fs http http2 https inspector module net os path perf_hooks process querystring readline repl stream string_decoder timers tls trace_events tty url util v8 vm worker_threads zlib — embedded sorted + sortedness unit test + spot members `fs path crypto`; generation source in code comment); exports-map deferred (found file = edge)
  - NO CFG001 (comment cites Rust-only rule, as Python does)

- [ ] **Step 1: Write the failing test**

```rust
// tests/resolver_ts.rs
#[test]
fn modules_and_ids() { /* src/routes/user.ts + src/routes/index.ts + app.tsx + types.d.ts(skipped) + node_modules/x/index.ts(skipped) => ModuleIds {pkg}::{tgt}::src::routes::user, ::src::routes (barrel, no index segment), ::app; .d.ts absent */ }
#[test]
fn probe_order_and_mod004() { /* user.ts + user/index.ts + user.tsx => edge to user.ts-file module + exactly one MOD004 Warning naming hit + first loser; missing ./ghost => SCOPE001 */ }
#[test]
fn import_semantics() { /* export-from => edge; export * from './ghost' => SCOPE001; require('lit') => edge; require(x) silent; dynamic import('lit') => edge; import type => normal edge; paths @/deep/* beats @/*; bare pinned => External; bare node_modules-only version => External(that version); bare unpinned => EXT001 exact; node:fs + bare fs => silent; own-pkg bare => Lib edge, ghost => SCOPE001 */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test resolver_ts`
Expected: FAIL — `resolver_ts` module missing.

- [ ] **Step 3: Implement `src/resolver_ts.rs`** (walk/extract/resolve per above; `pub mod resolver_ts;` + `analyzer_for` TS arm)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test resolver_ts && cargo test && cargo clippy --all-targets`
Expected: PASS, full suite green.

- [ ] **Step 5: Commit**

```bash
git add src/resolver_ts.rs src/lib.rs tests/resolver_ts.rs tests/fixtures/ts-resolve/
git commit -m "feat: typescript module and import resolution"
```

### Task 4: TypeScript e2e goldens

**Files:**
- Create: `tests/fixtures/ts-workspace/` (package.json named + scoped? one package: name `shop`, tsconfig rootDir+paths, src files incl. barrel + collision pair + tsx + skipped .d.ts, require/dynamic/import-type forms, bin entries hit+miss, lockfile pinned + node_modules-only dep + unpinned bare, `node:fs` import, multi-tsconfig `tsconfig.node.json` with distinct target)
- Create: `tests/snapshots/ts/`
- Test: `tests/ts_golden.rs`

**Interfaces:**
- Consumes: `run()` + `--lang typescript` (all prior tasks).
- Produces: golden bytes + determinism proof, mirroring `tests/py_golden.rs` shape (update mode documented single-threaded; NO hand-editing; ZERO hunks outside `tests/fixtures/ts-workspace/`, `tests/snapshots/ts/`, `tests/ts_golden.rs`).

- [ ] **Step 1: Write the failing test**

```rust
// tests/ts_golden.rs
#[test]
fn ts_full_run_produces_golden_bytes() { /* run(ts-workspace, external=true) matches tests/snapshots/ts/ byte-for-byte */ }
#[test]
fn ts_repeated_runs_identical_and_dry_run_clean() { /* twice identical; --dry-run zero files */ }
#[test]
fn ts_stdout_stderr_split() { /* stdout counts+status, no warnings/JSON; stderr carries MOD004/EXT001/SCOPE001 lines as fixture dictates, no CFG001, no JSON */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test ts_golden`
Expected: FAIL — fixture/`run()` typescript path missing.

- [ ] **Step 3: Implement fixture + run once in update mode, EYEBALL `git diff tests/snapshots/ts/`** (every note/edge/diagnostic traces to Tasks 2–3 rules; handshake `1/0.5.0/3`; assert `git status --porcelain tests/snapshots/tree/ tests/snapshots/py/` shows NOTHING), then lock; bin-level `--lang typescript` tempdir test included.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test ts_golden && cargo test && cargo test && cargo clippy --all-targets` (suite twice for determinism)
Expected: PASS throughout.

- [ ] **Step 5: Commit**

```bash
git add tests/ts_golden.rs tests/fixtures/ts-workspace/ tests/snapshots/ts/
git commit -m "feat: typescript end-to-end goldens"
```

---

## Acceptance mapping (spec §§0,1,3,4 → tasks; §2 Python done)

§0.1 shape → plan structure; §0.2 Lib/Bin → T2; §0.3 versions → T2 (lockfiles+node_modules) + T3 (EXT001); §0.4 TS scope (relative/bare/export-from/require/dynamic/paths; exports-map deferred) → T3; §0.5 handshake unchanged → no task (asserted in T4 Step 3 eyeball); §0.6 separate runs → existing `--lang`. §1 helper reuse → T1 (dep only); modules → T2–T4; `--lang` exists (extend nothing); deps → T1 (`tree-sitter-typescript 0.23` only). §3 discovery → T2; IDs → T2/T3; extraction → T3; resolution (probe order, paths ordering, bare/external/core, own-package, MOD004) → T3. §4 shared (registry arms → T2/T3; EXT001 → T3; testing → T4). §5: MSRV already recorded (T1 verifies floor still satisfied — `cargo build` green suffices, no new field); core list → T3 (rule above); multi-tsconfig naming → T2 (stem rule + first-sorted-wins); Bin edge cases → T2 (table-driven; non-JS executables out, code comment); `--lang` repeats → settled (clap error).
