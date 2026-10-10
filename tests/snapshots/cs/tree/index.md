---
graph_type: rust-dependency-root
protocol_version: 1
spec_version: 0.5.0
schema_version: 3
generator: cargo-obsidian-graph
---

# Workspace dependencies

## Packages

- Cli
- Tool
- Web

## Targets

- Cli::bin::Cli-net48
- Cli::bin::Cli-net8.0
- Tool::lib::Tool
- Web::lib::Web

## Diagnostics

- [Warning] CFG001: conditional compilation detected - graph reflects unexpanded source
- [Warning] EXT001: unresolved external: Web::Web::Web::Unpinned -> Floaty.Thing; no pinned version
- [Warning] EXT001: unresolved external: Web::Web::Web::Unpinned -> RangeLib.Thing; no pinned version
- [Warning] EXT001: unresolved external: Web::Web::Web::Unpinned -> BuildOnly.Thing; no pinned version
- [Warning] SCOPE001: scope collision: Cli::Cli-net48::Cli::Worker -> Shared.Models; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: Cli::Cli-net8.0::Cli::Worker -> Shared.Models; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: Web::Web::Web::Remnant -> NoSuchLib.Thing; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: Web::Web::Web::Unknown -> Bogus.Type; unresolved syntactic ownership
