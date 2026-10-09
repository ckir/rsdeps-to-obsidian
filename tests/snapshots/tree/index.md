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
- [Warning] SCOPE001: scope collision: api::alpha -> crate::shared::Help; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: api::api -> crate::models::User; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: api::api -> crate::routes::Router; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: api::api -> crate::services::Worker; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: api::api -> core::models; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: api::api -> self::services::Worker; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: api::api::models -> crate::services::Worker; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: api::api::routes -> crate::models::User; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: api::api::services -> crate::services::deep::Job; unresolved syntactic ownership
