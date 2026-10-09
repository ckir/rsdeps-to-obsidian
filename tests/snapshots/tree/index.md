---
graph_type: rust-dependency-root
protocol_version: 1
spec_version: 0.4.0
schema_version: 3
generator: cargo-obsidian-graph
---

# Workspace dependencies

## Packages

- api
- core

## Targets

- api::bin::alpha
- api::bin::beta
- api::lib::api
- core::lib::core

## Diagnostics

- [Warning] CFG001: conditional compilation detected - graph reflects unexpanded source
- [Warning] MOD002: ambiguous module layout: both @@WORKSPACE@@/crates/api/src/qux.rs and @@WORKSPACE@@/crates/api/src/qux/mod.rs exist; using @@WORKSPACE@@/crates/api/src/qux.rs
