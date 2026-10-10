# C# / .NET Backend — Design

**Status:** approved design pending owner review (builder check passed 2026-10-10 — 7 questions, all settled in §9, no open items)
**Date:** 2026-10-10
**Prior specs:** `2026-10-09-rsdeps-obsidian-mvp-design.md` (Rust, v0.4),
`2026-10-09-multilang-python-typescript-design.md` (Python + TypeScript, v0.5)
**Scope:** C# backend only (`CsAnalyzer`), general-purpose across the .NET
ecosystem (not scoped to one codebase)
**Feasibility basis:** spike 2026-10-10 — `tree-sitter-c-sharp 0.23.5` loads
under `tree-sitter 0.27` and captures all load-bearing forms (all `using`
shapes, block + file-scoped namespaces, classes incl. `partial`, top-level
statements as `global_statement`). Throwaway probe, deleted.

## 1. Intent and why

Bring the dependency-graph product to the third ecosystem: analyze C#
projects/solutions and render module graphs as Obsidian notes through the
existing pipeline. The builder's priority, decided with the owner: **per-file
coverage first, cross-project edges second** — edges are only as good as
their endpoints, and `ProjectReference` edges are nearly free once nodes are
solid (MSBuild states them explicitly).

## 2. Goals

- Discover .NET projects by walking for `*.csproj` (SDK-style fully parsed,
  legacy formats best-effort); use `.sln` for solution scoping when present,
  never require it.
- Discover C# modules from declared namespaces (one module per namespace
  block, sharing a file where they co-occur), with file-stem fallback for
  namespace-less files (top-level statements).
- Merge `partial` classes into one module pointing at the first sorted file
  (no model/schema change; schema stays 3).
- Extract `using` directives (plain, static, alias, global), resolve against
  per-file usings ∪ explicit global usings ∪ pinned per-TFM implicit-usings
  lists; aliases diagnostics-only.
- Resolve `using static` and aliases by full path like normal imports.
- Take versions from `PackageReference` pins and lock files; unpinned
  externals get the unresolved diagnostic (pin-or-diagnostic, as Python).
- Emit cross-project `Internal` edges from `ProjectReference`.
- Reuse Lib/Bin target semantics, `ModuleId` shape, SCOPE001/EXT001,
  handshake `1/0.5.0/3` (no version bump — same graph shape, same spec
  generation as Python/TS).

## 3. Non-goals

- Legacy non-SDK `csproj` full fidelity (best-effort parse; missing explicit
  `Compile` lists fall back to recursive `*.cs` glob, documented).
- Source generators and analyzer-driven code (invisible to syntax).
- Cross-language edges (separate runs, as with all backends).
- New diagnostic codes unless a gap forces one (none foreseen).

## 4. Anti-goals

- A backend that only works on one solution layout (must handle SDK-style,
  loose projects, multi-targeting, file-scoped namespaces out of the box).
- Fabricated edges for ambiguous cross-project names (SCOPE001 instead).

## 5. Constraints

- Single binary, one `Analyzer` per language (`CsAnalyzer`); shared
  tree-sitter helper; per-language discovery module (`discovery_cs.rs`,
  `resolver_cs.rs`); `--lang` gains `csharp` (CLI-safe spelling).
- `AnalysisInput` unchanged. Determinism, safety invariants S1–S8, and the
  no-golden-hand-editing rule all carry over. Rust/Python/TS goldens stay
  byte-identical.
- New deps: `tree-sitter-c-sharp 0.23` only (verified); record MSRV impact
  at plan stage.

## 6. Decided how (owner-approved direction, my calls marked)

- **(my call)** `.sln` scoping only: solution members analyzed as packages;
  loose `*.csproj` files each become own packages. No solution-wide merge.
- **(my call)** Multi-targeting (`TargetFrameworks` plural): one Lib target
  per (project, TFM) with `-net8.0`-style name suffix, ONLY when
  multi-targeted; single-TFM projects keep the plain `{pkg}::lib::{pkg}`
  shape. Same-name lib/bin-style collisions follow the established
  injective-filename machinery (no new code expected).
- **(my call)** `Bin` targets: executables (`OutputType=Exe`, `dotnet run`
  projects) → `{pkg}::bin::{name}`; libraries have no Bin targets.
- **(my call)** `PackageReference` with floating/range versions
  (`[6.*, *)`) → unresolved diagnostic (same pin-or-diagnostic rule, no
  NuGet version solver in MVP scope); exact pins + `packages.lock.json` →
  versioned externals.
- **(my call)** Namespace↔folder mismatch: identity always follows the
  DECLARED namespace, never the path. Same fully-qualified namespace
  declared in two projects → ambiguity diagnostic (member-map precedent),
  no edge.
- **(my call)** `using static` resolves by its full type path (normal edge
  when the type's module is known); `using` aliases contribute their full
  path to diagnostics only, never new IDs (precedent: TS/Python aliases).
- **(my call)** `global using` in any file = project-level context for all
  modules of that target; pinned per-TFM `ImplicitUsings` lists
  (`System`, `System.Collections.Generic`, etc., exact lists pinned at
  plan stage from the SDK props) unioned the same way, silently.
- **(my call)** Top-level-statement files and multi-namespace files share
  `source_path`; per-declaration navigation uses `SourceSpan` (already
  per-module capable).

## 7. Left to the builder

- Exact per-TFM `ImplicitUsings` lists and their SDK source reference.
- `TargetFramework` string normalization for suffixes (e.g. `netstandard2.0`
  verbatim vs shortened — pick verbatim).
- Fixture workspace shape mirroring the established e2e pattern (SDK-style
  multi-project solution + loose project + partials + file-scoped
  namespaces + aliases + static/global usings + pinned/unpinned externals).
- Whether `dotnet --list-sdks`-style environment probing is needed (no —
  file-only analysis, never shell out; state it in code comments).
- Test-file parallel to `tests/py_golden.rs` / `tests/ts_golden.rs` under
  `tests/snapshots/cs/`.

## 8. Open items for plan stage

- MSRV impact of `tree-sitter-c-sharp 0.23` (record from build env).
- Legacy-`Compile`-list fallback glob rule (recursive `*.cs` minus
  `obj/`+`bin/` — confirm exclusion list).
- `ProjectReference` chains that leave the analyzed root (unresolved
  diagnostic with the referenced path, no traversal outside — confirm).

## 9. Builder-check resolutions (my calls, 2026-10-10)

The builder check asked seven questions; all resolved here from
established precedent. Nothing below changes §§1–8 except where noted.

1. **Using → owner mapping.** Longest-prefix match in order: (a) own
   target's declared namespaces; (b) namespaces declared by directly
   referenced projects (`ProjectReference` closure, one hop — §6);
   (c) `PackageReference` package names by exact first-segment match
   (case-insensitive); (d) BCL skip list → silent skip. No match anywhere
   → SCOPE001, no edge. A using can therefore never fabricate a
   cross-project edge (anti-goal §4 holds).
2. **Global/implicit usings emit no edges.** Only explicit per-file `using`
   directives produce edges; global and implicit sets are resolution
   context only — ambient imports carry no structural information and
   per-module edges for them would flood the graph.
3. **ModuleId shape and merges.** Dotted namespace → `::` joins
   (`MyApp.Services` → `{pkg}::{tgt}::MyApp::Services`); nested namespaces
   use the full dotted path each; same namespace in several files of one
   project (partial or not) merges to ONE node (`source_path` = first
   sorted file); two namespaces in one file are distinct IDs sharing the
   file (spans differ, filenames stay injective); namespace-less files use
   project-relative path segments + stem (TypeScript precedent).
4. **BCL treatment.** `System*` (pinned prefix list, plan stage) silently
   skipped — never edges, never diagnostics (Python-stdlib precedent).
   Skip is consulted before external lookup.
5. **Multi-target duplication.** Each (project, TFM) Lib target gets the
   full module set (separate namespaces, as lib/bin today); per-TFM
   conditional references apply per target. `#if` regions are parsed
   inclusively with NO cfg evaluation; encountering `#if` emits the
   existing CFG001 text exactly once per run (Rust precedent).
6. **ProjectReference granularity.** Every importer of a solely-owned
   referenced namespace gets a direct Internal edge to that module;
   transitive references are NOT followed (documented); same namespace in
   two projects → ambiguity diagnostic, no edge (§6 stands).
7. **Central management + discovery scope.** `Directory.Packages.props`
   (incl. `VersionOverride`) and legacy `packages.config` are read for
   versions; `Directory.Build.props` evaluation is a documented non-goal
   (versions defined only there → unresolved diagnostic). Discovery walks
   down from the input root for `*.csproj` (never above it); multiple
   `.sln` files → union of member projects, deduped; `--lang auto`
   priority becomes Cargo.toml > `*.sln`/`*.csproj` > pyproject/setup.py >
   package.json.
