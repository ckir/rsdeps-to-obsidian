use rsdeps2obsidian::ids::ModuleId;
use rsdeps2obsidian::paths::{
    module_filename, resolve_output_root, validate_generated_path, windows_safe_name,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[test]
fn underscore_encoding_is_reversible_and_collision_free() {
    let plain = module_filename(&ModuleId("api::api::commands::build".to_string()));
    assert_eq!(plain, "api__api__commands__build.md");
    let tricky = module_filename(&ModuleId("api::my__module::handler".to_string()));
    assert_eq!(tricky, "api__my______module__handler.md");
    assert_ne!(plain, tricky);
    // A single "_" segment encodes to "___" (never collides with the "__" separator).
    assert_eq!(module_filename(&ModuleId("_".to_string())), "___.md");
    // "__" inside one segment must not decode as a separator.
    let double = module_filename(&ModuleId("a::b__c::d".to_string()));
    assert_eq!(double, "a__b______c__d.md");
}

#[test]
fn windows_reserved_names_escaped_with_collision_rule() {
    let mut taken = HashSet::new();
    assert_eq!(windows_safe_name("CON", &mut taken), "_CON");
    assert!(taken.contains("_CON"));
    // Collision with the escaped name re-applies underscore-doubling.
    assert_eq!(windows_safe_name("CON", &mut taken), "__CON");
    assert!(taken.contains("__CON"));

    let mut fresh = HashSet::new();
    assert_eq!(windows_safe_name("_con", &mut fresh), "__con");
    let mut prn_taken = HashSet::new();
    assert_eq!(windows_safe_name("PRN", &mut prn_taken), "_PRN");
    // Case preserved otherwise.
    let mut misc = HashSet::new();
    assert_eq!(windows_safe_name("MyModule", &mut misc), "MyModule");
    assert_eq!(windows_safe_name("Con", &mut misc), "_Con");
}

#[test]
fn output_root_rejects_symlink_component() {
    let base: PathBuf =
        std::env::temp_dir().join(format!("rsdeps_paths_symlink_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let real = base.join("real");
    std::fs::create_dir_all(&real).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, base.join("vault")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&real, base.join("vault")).unwrap();

    let raw = base.join("vault").join("graph");
    let err = resolve_output_root(&raw, &base).unwrap_err();
    assert_eq!(err, "unsafe output path");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn generated_paths_cannot_escape_root() {
    let root = Path::new("/vault/graph");
    assert!(validate_generated_path(root, "../evil.md").is_err());
    assert!(validate_generated_path(root, "/abs.md").is_err());
    // String-prefix trap: "/vault/graph" is a string prefix of
    // "/vault/graph2/evil.md" but not a path-component parent.
    assert!(validate_generated_path(root, "/vault/graph2/evil.md").is_err());
    assert!(validate_generated_path(root, "../graph2/evil.md").is_err());

    // Sanity: a plain relative path inside an existing root is accepted.
    let base: PathBuf =
        std::env::temp_dir().join(format!("rsdeps_paths_ok_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let joined = validate_generated_path(&base, "mod/sub.md").unwrap();
    assert_eq!(joined, base.join("mod/sub.md"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn nonexistent_root_uses_nearest_existing_parent() {
    let base: PathBuf =
        std::env::temp_dir().join(format!("rsdeps_paths_missing_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let canon = base.canonicalize().unwrap();

    let raw = base.join("new").join("sub").join("dir");
    let resolved = resolve_output_root(&raw, Path::new("/")).unwrap();
    assert_eq!(resolved, canon.join("new").join("sub").join("dir"));
    let _ = std::fs::remove_dir_all(&base);
}
