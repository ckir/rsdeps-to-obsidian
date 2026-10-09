# rsdeps-to-Obsidian MVP — Design

**Status:** conversational design approved, written spec for review
**Date:** 2026-10-09
**Source:** `docs/mvp/rust-obsidian-dependency-graph-mvp-v0.4.md` (v0.4.0)
**Scope:** full v0.4, all 34 acceptance criteria, implemented in phased slices
**Architecture:** B — core lib + thin binary

## 0. Agreed understanding

Build a greenfield Rust CLI in this repo implementing the v0.4 MVP:
Cargo workspace module-structure analysis → Obsidian Markdown + machine-readable
graph/manifest, with safe incremental regeneration. Module-level syntactic
approximation only (no rustc semantics, macro expansion, cfg evaluation, or type
graph). Success = §44 definition of done via `rsdeps2obsidian <monorepo>
--output <vault>`, deterministic bytes, plus all AC1–AC34.

### Binding decisions

1. **Handshake:** `protocol_version = 1`, `spec_version = "0.4.0"`,
   `schema_version = 3` emitted everywhere (JSON, manifest, all frontmatter).
   Rationale: v0.4 text is inconsistent (`spec 0.3.0` in §§6/16–19/40/45 vs
   `0.4.0` in §47.4; `schema 2` in §§41/45/48 vs `schema 3` in §§10–19/47.4).
   We follow the document version (0.4.0) and the majority schema (3).
   All copied examples are re-stamped; stale `schema_version: 1` never emitted.
2. **Naming:** Cargo package + built binary = `rsdeps2obsidian` (deviation from
   spec §7 `cargo-obsidian-graph`, per owner). All emitted
   `generator: cargo-obsidian-graph` fields and the §28 orphan marker stay
   `cargo-obsidian-graph` for vault-tooling compatibility and exact-marker
   detection (`generator` + `protocol_version: 1`).
3. **Toolchain:** modern stable (host cargo 1.99 / rustc 1.99; floor ~1.80+,
   edition 2021 or 2024), open dependency choice: `clap 4`, `cargo_metadata`,
   `syn 2`, `serde`/`serde_json`, `tempfile`, `thiserror`/`anyhow`.
4. **Renderer contract:** renderer consumes the canonical `WorkspaceGraph`
   only, never reparses source or touches syn types.

## 1. Architecture & crate layout

Single Cargo package `rsdeps2obsidian` with lib + thin binary:

```text
src/
  lib.rs          # public surface: Analyzer, graph, render, manifest entry points
  ids.rs          # PackageId/TargetId/ModuleId/ExternalCrateId newtypes
  domain.rs       # SourceSpan, Package, Target, Module, Dependency, WorkspaceGraph, Diagnostic
  config.rs       # CLI-validated config (workspace input, output, flags)
  metadata.rs     # cargo_metadata wrapper (format-version 1), workspace root
  discovery.rs    # lib/bin target discovery, per-target roots
  resolver.rs     # SynAnalyzer: parse + mod/use walk + ownership resolution
  graph.rs        # dedup + total-order sorting + validation
  diag.rs         # diagnostic interning, counting, deterministic sort
  paths.rs        # output-root + generated-path safety, filename mapping, windows policy
  render.rs       # markdown + JSON serialization from WorkspaceGraph
  manifest.rs     # manifest read/validate/diff/commit-record
  commit.rs       # atomic stage→publish→manifest→cleanup, clean, dry-run, orphan scan
src/bin/
  rsdeps2obsidian.rs  # arg parse, stream routing, exit codes; delegates to lib
tests/
  unit + fixture + golden + crash/recovery (see §4)
```

Dependency direction: `bin → lib`; `render/manifest/commit → graph/domain/ids`;
`resolver → metadata/discovery/domain`; `paths/diag` are leaves. No cycles.

```rust
pub trait Analyzer {
    fn analyze(&self, input: &AnalysisInput) -> Result<WorkspaceGraph, AnalysisError>;
}
pub struct SynAnalyzer;
```

`SynAnalyzer` is the only MVP implementor; a future rustc/Rust-Analyzer backend
implements the same trait without changing the renderer contract.

Binary responsibilities (thin): parse args, resolve workspace input vs
`--manifest-path` (mutually exclusive, neither → `.`), resolve relative
`--output` (default `./obsidian-graph`) against **process CWD**, enforce
stdin-never-read / stdout-human / stderr-diagnostics split, map errors to exit
`0/1/2`.

## 2. Resolver, dependencies & graph semantics

- **Targets (§§9.2/13):** from Cargo metadata; `library → lib`, `binary → bin`
  tokens; exact package/target names preserved (no case fold, no `-`→`_`);
  constructor rejects components containing `::`. Other target kinds → Info
  diagnostic, ignored. Each supported target gets an isolated module namespace
  (lib root vs each `src/bin/*.rs` never collapsed).
- **Module walk (§§11/32):** per target from root source: parse, register root,
  recurse `ItemMod`. Inline modules recurse with `SourceSpan{file: containing
  file, 1-based lines}`. File mods: `#[path]` resolved against containing
  file's directory, canonicalized, containment-checked to workspace roots
  (escape = fatal for that discovery op: PATH001/PATH002). Otherwise Rust
  layout candidates `foo.rs` / `foo/mod.rs`: both → select `foo.rs` + `MOD002`
  warning `ambiguous module layout: both foo.rs and foo/mod.rs exist; using
  foo.rs`, continue; neither → `MOD001` unresolved, continue. Discovered paths
  canonicalized + validated; a file parsed once may back multiple references
  only if logical module identities stay distinct.
- **`use` extraction (§§11.5–11.6):** traverse nested trees; support
  `crate::`, `self::`, `super::`, `{a,b}`, `crate::foo::{a,b}`, `as` aliases
  (alias in diagnostics only, never a new ID). `pub use` → `ReExport`, else
  `Use`; `mod foo;` edges → `Module`. Dedup key `(from, to)` single winner by
  `ReExport > Use > Module`.
- **Ordering (§20):** packages by `PackageId`; targets by
  `(package, kind-token, name)`; modules by `ModuleId`; dependencies by
  `(from ↑, Internal<External, target-string ↑, Module<Use<ReExport)` via an
  explicit comparator (never enum discriminants); diagnostics per §4 below. No
  hash-map iteration order leaks to renderers (collect → sort at boundaries;
  `HashMap` internally, `BTreeMap`/sorted `Vec` at serialization).
- **Scope resolution (§§11.7/33):** `crate/self/super` against known tree;
  first-segment match against Cargo dep packages → `External(<name>@<version>)`
  (name = Cargo package name not alias; distinct versions = distinct nodes).
  Unresolvable ownership → `SCOPE001`
  `scope collision: <src-module> -> <path>; unresolved syntactic ownership`
  on stderr, unresolved-count++, no edge (S6). Missing stable name/version →
  unresolved diagnostic, never invented ID.
- **cfg (§12):** any `#[cfg(...)]` on an encountered `mod`/`use` → exactly one
  run-level `CFG001` warning
  `conditional compilation detected - graph reflects unexpanded source`;
  unresolved-count++; no evaluation, no per-feature graphs.

## 3. Rendering, safety, commit/recovery

- **Filename mapping (§15, normative):** split `ModuleId` on `::`, encode each
  literal `_` → `___` within the segment, join with `__`, append `.md`.
  `api::my__module::handler` → `api__my______module__handler.md`. Never use a
  raw logical ID as a filename; manifest stores both `module_id` and
  `filename`. Decoding treats `___` runs as escaped underscores; empty
  identifier segments invalid so the separator is unambiguous.
- **Windows policy (§14.3):** on the encoded component, in order: (1) double a
  leading `_` that would form the escape prefix, (2) prefix reserved base
  (`CON/PRN/AUX/NUL/COM1-9/LPT1-9`, case-insensitive) with `_`
  (`CON→_CON`, `_CON→__CON`), (3) trailing space/period → `_`, (4) on
  collision re-apply underscore-doubling until distinct. Case-preserving
  otherwise; no hash/counter/order-dependent suffix. Overlong → diagnostic +
  skip file, never nondeterministic truncation.
- **Notes:** every generated `.md` (module, external, root) carries
  `generator: cargo-obsidian-graph` + handshake `1/0.4.0/3`. Module notes add
  `graph_type: rust-module`, `module_id/target_id/package_id`, `source` as
  workspace-root-relative `/`-normalized path (containing file for inline
  modules; omitted only if unsafe). Body: `# <short name>`, `## Depends on` /
  `## Used by` wikilinks `[[<file>|<id>]]` in dependency order. External notes
  (only with `--external`) use `graph_type: rust-external-crate`; without the
  flag externals persist in JSON but emit no Markdown (AC32). Root note
  (`index.md`, `graph_type: rust-dependency-root`) lists packages/targets/diagnostics
  deterministically.
- **Machine artifacts (§§18–19/24):** JSON graph + manifest share the handshake
  envelope `{protocol 1, spec "0.4.0", schema 3, generator
  "cargo-obsidian-graph"}`; manifest adds `generated_files[]` (sorted) and
  `module_records[{module_id, filename}]` (sorted by `module_id`). Unknown
  `protocol/schema` → consumer must reject.
- **Output-root safety (§§14.1/29):** relative `--output` vs process CWD;
  inspect every existing chain component — any symlink → fatal
  `unsafe output path`, exit 1 (no `--allow-symlink` in v0.4). Existing root →
  canonicalize; missing root → canonicalize nearest existing non-symlink
  parent, append lexical tail. Per-file (§14.2): reject absolute/`..`, reject
  symlink components, enforce lexical + canonical component-aware containment
  (never string-prefix). Manifest subject to identical rules.
- **Atomic commit (§27, 19-step normative order):** analyze → validate graph →
  validate every output path incl. manifest → compute desired set → read +
  validate previous manifest (missing/invalid → deletions disabled for the run)
  → diff create/update/delete → stage Markdown temps in same filesystem →
  flush/fsync → atomic rename each → stage/flush/fsync/rename manifest as the
  commit record → only then delete obsolete previous-manifest entries inside
  the root; cleanup failures are warnings. Crash before manifest rename → old
  manifest authoritative, staged files are orphan candidates. Crash after →
  stale files are orphans. No cross-filesystem renames.
- **Orphans (§28):** candidate iff inside root + `.md` + frontmatter contains
  `generator: cargo-obsidian-graph` **and** `protocol_version: 1` + absent from
  valid previous manifest. YAML-field check only (no HTML comments/filenames).
  Report `orphan generated file: <name>` on stderr; never auto-delete; `--clean`
  never deletes orphans (S1/S8).
- **`--clean` (§25):** requires a valid manifest; missing → delete nothing +
  report; invalid → delete nothing + exit 1; each entry re-validated for
  containment + symlinks, abort on escape; never touches unrelated `.md` or
  orphans.
- **`--dry-run` (§26/S7):** strictly write-free — zero workspace/output/
  manifest writes, zero deletions (incl. `--dry-run --clean`); compute + print
  `create/update/delete/diagnostics` plan; no temp writes required.

## 4. Diagnostics, errors, streams, testing & phasing

- **Diagnostics (§22):** `{severity: Info|Warning|Error, code, message,
  source_span?}`; codes `CFG001/SCOPE001/MOD001/MOD002/PATH001/PATH002/
  ORPHAN001/CLEAN001/IO001`. Total order: severity `Info<Warning<Error` →
  code → workspace-relative `/` path → line (missing first) → message.
- **Fatal (exit 1):** bad CLI combo, metadata failure, root-resolution failure,
  unsafe output path, invalid manifest under `--clean`, path escape,
  unrecoverable I/O. **Warnings:** CFG001, SCOPE001, MOD002, ORPHAN001,
  unsupported-target info, unresolved imports. Path-safety failures are never
  downgraded to warnings. Exit 2 reserved for protocol/compat. Warnings never
  flip a successful exit.
- **Streams (§8):** stdin never read; stdout = human summary + dry-run plan
  (`workspace/packages/targets/modules/dependencies/external +
  create/update/delete + warnings + status: success`, cf. §39); stderr = all
  warnings/errors; stdout is never JSON in v0.4. Scope collisions warn on
  stderr without changing stdout's role.
- **Security invariants S1–S8** hold as specified: manifest-only deletion, no
  source modification, no manifest guessing, no traversal, no symlink escape,
  no fabricated edges, no dry-run mutation, manifest-as-commit-record.
- **Tests (§37):** unit (IDs, mapping incl. `__` cases, reserved names +
  `_CON` collisions, containment incl. `/vault/graph` vs `/vault/graph2`,
  manifest validation, ordering, dedup precedence, diag sort); fixtures
  (workspace with lib + multi-bin + inline/nested/`#[path]`/use-trees/alias/
  `pub use`/dupes/externals incl. two serde versions/cfg/ambiguous-layout/
  reserved/symlink-escape/invalid-manifest/orphans); golden (Markdown + JSON +
  manifest bytes + diag order, double-run identical); crash/recovery (interrupt
  after Markdown publish / after manifest temp / after rename → old-manifest
  authoritative, orphans reported-not-deleted, clean never deletes orphans).
- **Phasing (§42 order):** ① CLI+config ② metadata+IDs ③ path-safety ④
  discovery+Syn+resolution ⑤ use/external+graph sort/dedup+diag ⑥
  JSON/manifest+markdown ⑦ commit/orphan/clean/dry-run ⑧ fixtures+golden+crash
  tests. Performance: one metadata call, ~one parse per file, hash-maps
  internally + sorted boundaries, no quadratic lookup/dedup/validation.
- **Acceptance:** AC1–AC34 verified against §44 (`rsdeps2obsidian
  ~/src/my-monorepo --output ~/vault/architecture` safe + deterministic,
  14-item output checklist). Known spec deviations recorded in §0 (handshake
  re-stamp, binary rename); no other deviations without owner approval.

## 5. Open items for plan stage

- Exact `clap` surface spellings (`--external/--verbose` semantics) — trivial,
  settled in plan.
- Root note filename is `index.md` (per §24 manifest example listing
  `"index.md"`); plan does not revisit this.
- Golden-fixture workspace contents under `tests/` — plan enumerates.
- Golden-fixture workspace contents under `tests/` — plan enumerates.
