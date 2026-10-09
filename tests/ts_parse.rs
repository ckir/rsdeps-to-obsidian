use rsdeps2obsidian::ts_parse::{parse, query_nodes};

#[test]
fn python_import_queries_capture_with_rows() {
    let src = "import os\nfrom pkg.mod import Thing as T\n";
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let tree = parse(language.clone(), src).expect("parse should succeed");
    assert!(!tree.root_node().has_error());

    let imps = query_nodes(&tree, &language, src, "(import_statement) @imp").expect("query imp");
    assert_eq!(imps.len(), 1);
    assert_eq!(imps[0].capture, "imp");
    assert_eq!(imps[0].text, "import os");
    assert_eq!(imps[0].start_row, 0);

    let froms =
        query_nodes(&tree, &language, src, "(import_from_statement) @from").expect("query from");
    assert_eq!(froms.len(), 1);
    assert_eq!(froms[0].capture, "from");
    assert_eq!(froms[0].start_row, 1);
}

#[test]
fn bad_query_is_err_and_error_tree_no_panic() {
    let src = "import os\n";
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let tree = parse(language.clone(), src).expect("parse should succeed");
    assert!(query_nodes(&tree, &language, src, "(unclosed").is_err());

    let bad = parse(language, "def f(:\n").expect("parse of invalid src is Ok");
    assert!(bad.root_node().has_error());
}
