// Deliberate ambiguous layout: qux.rs and qux/mod.rs both exist.
// Discovery selects qux.rs and emits exactly one MOD002 warning.
pub struct QuxAlt;
