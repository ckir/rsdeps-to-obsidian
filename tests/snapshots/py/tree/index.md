---
graph_type: rust-dependency-root
protocol_version: 1
spec_version: 0.5.0
schema_version: 3
generator: cargo-obsidian-graph
---

# Workspace dependencies

## Packages

- acme

## Targets

- acme::bin::__main__
- acme::bin::tool
- acme::lib::acme

## Diagnostics

- [Warning] EXT001: unresolved external: acme::acme::acme::ext -> requests; no pinned version
- [Warning] EXT001: unresolved external: acme::acme::acme::ext -> nosuchpkg.thing; no pinned version
- [Warning] EXT001: unresolved external: acme::acme::acme::ext -> nosuchmulti.item1; no pinned version
- [Warning] EXT001: unresolved external: acme::acme::acme::ext -> nosuchmulti.item2; no pinned version
- [Warning] EXT001: unresolved external: acme::acme::acme::sib -> ghostsecond; no pinned version
- [Warning] EXT001: unresolved external: acme::acme::acme::star -> nosuchstar; no pinned version
- [Warning] EXT001: unresolved external: acme::acme::acme::tc -> ghostpkg; no pinned version
- [Warning] MOD003: ambiguous python layout: both @@WORKSPACE@@/src/acme/amb.py and @@WORKSPACE@@/src/acme/amb/__init__.py exist; using package
- [Warning] SCOPE001: scope collision: acme::__main__ -> acme.models; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: acme::acme::acme::views -> acme.api.handler; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: acme::tool -> acme.models; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: acme::tool -> acme.ghost; unresolved syntactic ownership
- [Warning] SCOPE001: scope collision: acme::tool -> .nosuchsib; unresolved syntactic ownership
