use rsdeps2obsidian::domain::{Module, Target, TargetKind};
use rsdeps2obsidian::ids::{PackageId, TargetId};
use rsdeps2obsidian::resolver::{discover_modules_for_target, extract_uses, parse_source_file};
use std::path::PathBuf;

fn scenario_dir(scenario: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/resolve-fixture")
        .join(scenario)
}

fn lib_target(pkg: &str, name: &str, root: PathBuf) -> Target {
    Target {
        id: TargetId(format!("{pkg}::lib::{name}")),
        package_id: PackageId(pkg.to_string()),
        name: name.to_string(),
        kind: TargetKind::Lib,
        root_source: root,
    }
}

fn find<'a>(modules: &'a [Module], id: &str) -> &'a Module {
    modules
        .iter()
        .find(|m| m.id.0 == id)
        .unwrap_or_else(|| panic!("missing module {id}; have {:?}", ids_of(modules)))
}

fn ids_of(modules: &[Module]) -> Vec<&str> {
    modules.iter().map(|m| m.id.0.as_str()).collect()
}

#[test]
fn file_and_inline_modules_discovered() {
    let dir = scenario_dir("basic");
    let target = lib_target("fix", "basic", dir.join("lib.rs"));
    let (modules, diags) = discover_modules_for_target(&target, &dir);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    assert_eq!(
        ids_of(&modules),
        vec![
            "fix::basic",
            "fix::basic::foo",
            "fix::basic::api",
            "fix::basic::api::handler",
        ]
    );

    let root = find(&modules, "fix::basic");
    assert_eq!(root.qualified_name, "crate");
    assert_eq!(root.source_span, None);

    let foo = find(&modules, "fix::basic::foo");
    assert_eq!(foo.qualified_name, "crate::foo");
    assert!(foo.source_path.as_ref().unwrap().ends_with("foo.rs"));
    assert_eq!(foo.source_span, None);

    // Inline modules carry no own file; their span points at the containing file.
    let api = find(&modules, "fix::basic::api");
    assert_eq!(api.source_path, None);
    let api_span = api.source_span.as_ref().expect("inline mod needs a span");
    assert_eq!(api_span.file, dir.join("lib.rs"));
    assert_eq!((api_span.line_start, api_span.line_end), (3, 5));

    let handler = find(&modules, "fix::basic::api::handler");
    assert_eq!(handler.qualified_name, "crate::api::handler");
    assert_eq!(handler.source_path, None);
    let handler_span = handler
        .source_span
        .as_ref()
        .expect("nested inline mod needs a span");
    assert_eq!(handler_span.file, dir.join("lib.rs"));
    assert_eq!((handler_span.line_start, handler_span.line_end), (4, 4));
}

#[test]
fn path_attribute_respected_and_escaped_rejected() {
    let dir = scenario_dir("path-attr");
    let target = lib_target("fix", "pathattr", dir.join("lib.rs"));
    let (modules, diags) = discover_modules_for_target(&target, &dir);

    assert_eq!(
        ids_of(&modules),
        vec!["fix::pathattr", "fix::pathattr::custom"]
    );
    let custom = find(&modules, "fix::pathattr::custom");
    assert!(custom
        .source_path
        .as_ref()
        .unwrap()
        .ends_with("special/foo.rs"));

    // The ../../evil.rs escape must not become a module; exactly one PATH diagnostic.
    assert!(
        modules.iter().all(|m| !m.id.0.ends_with("::evil")),
        "escaped module registered: {:?}",
        ids_of(&modules)
    );
    let path_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.code.starts_with("PATH"))
        .collect();
    assert_eq!(
        path_diags.len(),
        1,
        "expected one PATH diagnostic: {diags:?}"
    );
    assert_eq!(path_diags[0].code, "PATH001");
    assert_eq!(diags.len(), 1, "no other diagnostics expected: {diags:?}");
}

#[test]
fn ambiguous_layout_prefers_file_rs_with_mod002() {
    let dir = scenario_dir("ambiguous");
    let target = lib_target("fix", "ambig", dir.join("lib.rs"));
    let (modules, diags) = discover_modules_for_target(&target, &dir);

    assert_eq!(ids_of(&modules), vec!["fix::ambig", "fix::ambig::qux"]);
    let qux = find(&modules, "fix::ambig::qux");
    let path = qux.source_path.as_ref().expect("file module needs a path");
    assert!(path.ends_with("qux.rs"), "must prefer qux.rs, got {path:?}");
    assert!(
        !path.ends_with("qux/mod.rs"),
        "must not pick qux/mod.rs, got {path:?}"
    );

    let mod002: Vec<_> = diags.iter().filter(|d| d.code == "MOD002").collect();
    assert_eq!(mod002.len(), 1, "exactly one MOD002 expected: {diags:?}");
    assert!(
        mod002[0].message.contains("ambiguous module layout"),
        "message: {}",
        mod002[0].message
    );
    assert_eq!(diags.len(), 1, "no other diagnostics expected: {diags:?}");
}

#[test]
fn use_trees_aliases_and_pub_use_extracted() {
    let dir = scenario_dir("uses");
    let target = lib_target("fix", "uses", dir.join("lib.rs"));
    let (modules, diags) = discover_modules_for_target(&target, &dir);

    // `use` items never create modules: only root + the inline child.
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    assert_eq!(ids_of(&modules), vec!["fix::uses", "fix::uses::inner"]);

    let root = find(&modules, "fix::uses");
    let file = parse_source_file(&dir.join("lib.rs")).expect("fixture must parse");
    let uses = extract_uses(root, &file);

    let inner_id = "fix::uses::inner";
    let expected: Vec<(&str, &str, bool, Option<&str>)> = vec![
        ("fix::uses", "crate::alpha", false, None),
        ("fix::uses", "crate::beta", false, None),
        ("fix::uses", "crate::foo::gamma", false, None),
        ("fix::uses", "crate::foo::delta", false, Some("delta_alias")),
        ("fix::uses", "crate::bar", false, Some("bar_alias")),
        ("fix::uses", "crate::shared", true, None),
        ("fix::uses", "self::sibling", false, None),
        ("fix::uses", "super::parent_mod", false, None),
        ("fix::uses", "serde::de::Deserializer", false, None),
        (inner_id, "crate::deep::thing", false, None),
        (inner_id, "super::up_thing", true, None),
    ];
    assert_eq!(
        uses.len(),
        expected.len(),
        "unexpected RawUse set: {uses:?}"
    );
    for (from, path, is_pub, alias) in expected {
        let hit = uses.iter().find(|u| u.from.0 == from && u.path == path);
        let hit = hit.unwrap_or_else(|| panic!("missing use {path} from {from}: {uses:?}"));
        assert_eq!(hit.is_pub, is_pub, "is_pub for {path}");
        assert_eq!(hit.alias.as_deref(), alias, "alias for {path}");
        let span = hit
            .span
            .as_ref()
            .unwrap_or_else(|| panic!("use {path} needs a span"));
        assert_eq!(span.file, dir.join("lib.rs"));
        assert!(span.line_start >= 1 && span.line_end >= span.line_start);
    }
    // Spot-check an exact 1-based line: `use crate::bar as bar_alias;` is line 3.
    let bar = uses.iter().find(|u| u.path == "crate::bar").unwrap();
    assert_eq!(bar.span.as_ref().unwrap().line_start, 3);
}
