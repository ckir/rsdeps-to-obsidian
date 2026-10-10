---
graph_type: rust-dependency-root
protocol_version: 1
spec_version: 0.5.0
schema_version: 3
generator: cargo-obsidian-graph
---

# Workspace dependencies

## Packages

- shop

## Targets

- shop::bin::cli
- shop::bin::ghost
- shop::lib::node
- shop::lib::shop

## Diagnostics

- [Warning] EXT001: unresolved external: shop::shop::main -> ghostpkg; no pinned version
- [Warning] EXT001: unresolved external: shop::node::main -> ghostnode; no pinned version
- [Warning] MOD004: ambiguous ts layout: both @@WORKSPACE@@/src/user.ts and @@WORKSPACE@@/src/user.tsx exist; using @@WORKSPACE@@/src/user.ts
- [Warning] SCOPE001: scope collision: shop::shop::main -> ./ghost; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: shop::shop::main -> shop/ghost; unresolved syntactic ownership
- [Warning] TS001: unfollowed tsconfig extends: @@WORKSPACE@@/tools/missing-base.json; paths may be incomplete
- [Error] IO001: failed to read @@WORKSPACE@@/missing.js: No such file or directory (os error 2)
