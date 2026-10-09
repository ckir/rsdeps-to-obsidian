pub const PROTOCOL_VERSION: u32 = 1;
pub const SPEC_VERSION: &str = "0.4.0";
pub const SCHEMA_VERSION: u32 = 3;
pub const GENERATOR: &str = "cargo-obsidian-graph";

pub mod commit;
pub mod config;
pub mod diag;
pub mod domain;
pub mod graph;
pub mod ids;
pub mod manifest;
pub mod metadata;
pub mod paths;
pub mod render;
pub mod resolver;
