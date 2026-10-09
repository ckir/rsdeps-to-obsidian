# Multi-language support (Python + TypeScript) — Design

**Status:** conversational design approved, written spec for review
**Date:** 2026-10-09
**Prior spec:** `docs/superpowers/specs/2026-10-09-rsdeps-obsidian-mvp-design.md`
(v0.4 MVP, Rust-only) plus Post-MVP amendment A (cross-package edges +
prefix attribution, same file)
**Scope:** Python backend first, TypeScript backend second; each
independently shippable; Rust behavior frozen (goldens byte-identical)
**Feasibility basis:** spike 2026-10-09 — `tree-sitter 0.27` loads
`tree-sitter-python 0.25` and `tree-sitter-typescript 0.23.2` cleanly and
captures all required import forms (throwaway probe, deleted)

## 0. Agreed understanding

Extend `rsdeps2obsidian` from Rust-only to Rust + Python + TypeScript while
reusing the entire back half of the pipeline (graph model, ordering, filename
encoding, Markdown/JSON/manifest rendering, atomic commit, diagnostics
ordering, e2e harness). Each language gets a project-discovery module and an
`Analyzer` implementation over one shared tree-sitter parse helper. Success =
per-language golden suites green with the same determinism and safety
invariants as the Rust backend (S1–S8 hold for all languages).

### Binding decisions

1. **Shape:** single binary, three Analyzers (`SynAnalyzer`, `PyAnalyzer`,
   `TsAnalyzer`), shared `src/ts_parse.rs` helper. No workspace-crate split,
   no data-driven generic engine.
2. **Target semantics:** reuse `TargetKind::{Lib, Bin}` + `lib`/`bin` tokens
   for all languages (no new tokens, no ID-shape change). Python: import
   roots → `Lib`, scripts/entry modules → `Bin`. TypeScript: tsconfig
   project entry → `Lib`, `package.json` `bin` entries → `Bin`.
3. **External versions:** versioned `ExternalCrateId(name@version)` only from
   a trustworthy source per ecosystem (Cargo resolve; TS
   `package-lock.json`/`npm-shrinkwrap`/`yarn.lock` else
   `node_modules/<name>/package.json`; Python `==` pins and
   poetry/pdm/uv lockfiles). Otherwise an unresolved-import diagnostic and
   no edge (S6-consistent pin-or-diagnostic rule).
4. **TypeScript resolution scope:** relative (lexical) + bare→external +
   `export`-from + `require()` + dynamic `import()` + `tsconfig paths`
   prefix mapping. Package `exports`-map subtleties deferred.
5. **Handshake:** `protocol_version` 1, `schema_version` 3 (same
   `WorkspaceGraph`), `spec_version` `0.4.0` → `0.5.0`; all emitters and
   goldens re-stamped.
6. **Cross-language monorepos:** separate runs per language; no
   cross-language edges (documented non-goal).

## 1. Architecture & backend selection

Single package, single binary. New modules: `src/ts_parse.rs` (shared
`parse(language, src) -> Tree` + query runner yielding captured nodes with
byte ranges), `src/discovery_py.rs`, `src/resolver_py.rs` (`PyAnalyzer`),
`src/discovery_ts.rs`, `src/resolver_ts.rs` (`TsAnalyzer`). Existing
`metadata.rs`/`resolver.rs` become the Rust backend, untouched except
`--lang` plumbing.

`--lang auto|rust|python|typescript` (default `auto`). Autodetect scans
project files in priority `Cargo.toml` > `pyproject.toml`/`setup.py` >
`package.json`, first match wins; explicit `--lang` overrides silently
(documented); no flag + multiple project files → highest-priority one +
Info diagnostic naming the ignored others. `AnalysisInput` gains NO new
fields.

New dependencies (all verified resolvable 2026-10-09): `tree-sitter 0.27`,
`tree-sitter-python 0.25`, `tree-sitter-typescript 0.23`. Exact MSRV floor
imposed by `tree-sitter 0.27` is recorded by the implementation plan, not
here.

## 2. Python backend (`PyAnalyzer`)

**Discovery.** Project root = nearest dir containing `pyproject.toml`, else
`setup.py`/`setup.cfg`/`requirements*.txt`, else CWD. `Package` name from
`[project] name`, fallback root dir name; `manifest_path` = the
pyproject/setup.py path, fallback root. Import roots: `src/` if it exists
(src-layout), else root (flat-layout); first existing wins. One `Lib`
target per import root named after the package
(`{pkg}::lib::{pkg}`, root source = the import root). `Bin` targets:
top-level `*.py` scripts outside packages + `__main__.py` entry modules,
one each (`{pkg}::bin::{stem}`); the `console_scripts` table is consulted
only for naming, never executed. Namespace packages (PEP 420, no
`__init__.py`) are plain directory modules.

**IDs.** `ModuleId` = `{pkg}::{tgt}::` + dotted path with `.` → `::`;
`__init__.py` denotes the package module itself (no extra segment).

**Extraction** (tree-sitter): `import a.b [as c]`, `from a.b import x
[as y]`, `from .[.]* import …` (relative level = dot count). Aliases are
diagnostics-only, as with Rust. Dynamic imports (`importlib`,
`__import__`) are ignored with no diagnostic (documented).

**Resolution.** Absolute imports against the import-root tree with the
amendment-A longest-prefix rule reused verbatim; relative imports anchored
at the importing module, then the same rule; third-party first segments →
`External(name@version)` iff pin/lockfile proves the version, else
unresolved-import diagnostic + no edge; stdlib names (static list seeded
from `sys.stdlib_module_names` 3.11–3.14, exact list pinned in the
implementation) → silently skipped, no node, no diagnostic;
`TYPE_CHECKING`-only imports treated normally (the single-CFG001 rule stays
Rust-only, documented).

## 3. TypeScript backend (`TsAnalyzer`)

**Discovery.** Project root = nearest dir with `package.json`, else
`tsconfig.json`, else CWD. `Package` name from `package.json` `name`,
fallback dir name; private/scoped names kept verbatim (`@org/web` — `/`
is legal in IDs, only `::` is structural). One `Lib` target per
`tsconfig.json` (`{pkg}::lib::{name}`, root = `compilerOptions.rootDir`
or the tsconfig dir; target name = package name unless disambiguated by
multiple tsconfigs, in which case the tsconfig stem, e.g. `tsconfig.node`
→ target `node` — deterministic, pinned in implementation). `Bin` targets
from the `package.json` `bin` table (each file → `{pkg}::bin::{name}`);
absent table → no Bin targets. Multiple tsconfigs are never merged.

**IDs.** `ModuleId` from the file's project-relative path minus extension,
`/` → `::` (`src/routes/user.ts` → `{pkg}::{tgt}::src::routes::user`);
`index.*` barrels denote the directory module (segment dropped, mirroring
`__init__.py`); extensions stripped (`.ts/.tsx/.mts/.cts`); `.d.ts` files
skipped silently (type-only, documented); compiled `.js` outputs ignored
(sources only, documented).

**Extraction.** `import … from '…'` (default/named/namespace), `export …
from '…'` + `export *`, `require('…')` calls, dynamic `import('…')` as use
edges (static string only; non-literal → ignored, no diagnostic). `import
type` is a normal edge (no type-graph distinction at module granularity,
documented).

**Resolution.** Relative specifiers lexically against the importing file
(extension probing `.ts/.tsx/.d.ts` + `/index` barrels, first-hit-wins in
that pinned order); `tsconfig` `paths` prefixes longest-prefix-first,
first-match-wins (ordering pinned in implementation); bare specifiers →
`External(name@version)` from lockfiles, else
`node_modules/<name>/package.json`, else unresolved-import diagnostic + no
edge; `node:`-prefixed and core modules (`fs`, `path`, … — pinned list,
same silent-skip treatment as Python stdlib). Package `exports`-map
subtleties deferred (unresolved diagnostic when hit, documented).

## 4. Shared changes, testing & sequencing

**Shared code.** `config.rs` gains `--lang`; `TargetKind` docs gain the
§0.2 reuse semantics; `ExternalCrateId` docs gain the §0.3 per-ecosystem
version-source rule; `lib.rs` bumps `SPEC_VERSION` to `"0.5.0"` and holds
the `rust|python|typescript` → constructor registry.

**Diagnostics.** `SCOPE001` (same text) for unresolvable ownership in all
languages; new code `EXT001` (severity Warning) for version-unprovable
externals (pin-or-diagnostic cases), distinguishing them from ownership
ambiguity, with exact text `unresolved external: <src> -> <path>; no
pinned version` (plus the standard `at <file>:<line>` suffix used by
`SCOPE001` diagnostics); stdlib/core skips silent by design; ordering
rules unchanged.

**Testing.** Per-language fixture workspaces mirroring the Rust e2e shape
(where a form has no equivalent — Python has no `pub use` — the fixture
documents the omission); `tests/py_golden.rs` + `tests/ts_golden.rs` with
`tests/snapshots/{py,ts}/`; determinism double-run per language; unit
tests for anchor/paths/probing rules. Rust goldens stay byte-identical
(no-op proof).

**Sequencing.** Python backend first (lands `ts_parse`, `--lang`
plumbing, 0.5.0 re-stamp), TypeScript second. Each language merges only
when its goldens are green — independently shippable, sequential
spec→plan→implement cycles per the brainstorming decomposition rule.

## 5. Open items for plan stage

- Exact MSRV floor imposed by `tree-sitter 0.27` (record from `cargo build`
  environment, not this spec).
- Pinned stdlib list (Python 3.11–3.14 `sys.stdlib_module_names`) and
  pinned TS core-module list (exact contents).
- `tsconfig paths` longest-prefix ordering + extension-probing order
  (pinned constants, §3 states intent).
- Multi-tsconfig target-naming rule beyond the stem convention (§3 states
  intent; collisions, e.g. `tsconfig.app.json` vs nested same-stem files,
  need the deterministic tiebreak).
- Python `Bin` script discovery edge cases (namespace-package `__main__`,
  `scripts/` dirs, non-`.py` executables — in or out).
- Whether `--lang` accepts repeats / comma lists in the future (no; single
  value in this design — cross-language stays separate-runs).
