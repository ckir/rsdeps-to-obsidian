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

#[test]
fn ts_and_tsx_import_queries_capture() {
    let ts_src = "import x from './a';\nexport * from './c';\nconst z = require('dep');\n";
    let ts_language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let ts_tree = parse(ts_language.clone(), ts_src).expect("TS parse should succeed");
    assert!(!ts_tree.root_node().has_error());

    let ts_imports = query_nodes(&ts_tree, &ts_language, ts_src, "(import_statement) @imp")
        .expect("TS import query");
    assert_eq!(ts_imports.len(), 1);
    assert_eq!(ts_imports[0].capture, "imp");
    assert_eq!(ts_imports[0].start_row, 0);

    let ts_exports = query_nodes(&ts_tree, &ts_language, ts_src, "(export_statement) @exp")
        .expect("TS export query");
    assert_eq!(ts_exports.len(), 1);
    assert_eq!(ts_exports[0].capture, "exp");
    assert_eq!(ts_exports[0].start_row, 1);

    let ts_requires = query_nodes(
        &ts_tree,
        &ts_language,
        ts_src,
        "(call_expression function: (identifier) @func arguments: (arguments (string) @dep))",
    )
    .expect("TS require query");
    let funcs: Vec<_> = ts_requires.iter().filter(|n| n.capture == "func").collect();
    assert_eq!(funcs.len(), 1);
    assert_eq!(funcs[0].text, "require");
    assert_eq!(funcs[0].start_row, 2);
    let deps: Vec<_> = ts_requires.iter().filter(|n| n.capture == "dep").collect();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].text, "'dep'");

    let tsx_src = "import A from './A';\nexport const B = () => <div/>;\n";
    let tsx_language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let tsx_tree = parse(tsx_language.clone(), tsx_src).expect("TSX parse should succeed");
    assert!(!tsx_tree.root_node().has_error());

    let tsx_imports = query_nodes(&tsx_tree, &tsx_language, tsx_src, "(import_statement) @imp")
        .expect("TSX import query");
    assert_eq!(tsx_imports.len(), 1);
    assert_eq!(tsx_imports[0].capture, "imp");
    assert_eq!(tsx_imports[0].start_row, 0);
}

#[test]
fn csharp_namespace_and_using_queries_capture() {
    let src = "using System;\nusing static System.Math;\nusing IO = System.IO;\nglobal using System.Collections.Generic;\nnamespace MyApp.Services\n{\n    public partial class Handler { }\n}\n";
    let language: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    let tree = parse(language.clone(), src).expect("C# parse should succeed");
    assert!(!tree.root_node().has_error());

    let usings =
        query_nodes(&tree, &language, src, "(using_directive) @use").expect("C# using query");
    assert_eq!(usings.len(), 4);

    let namespaces = query_nodes(&tree, &language, src, "(namespace_declaration) @ns")
        .expect("C# namespace query");
    assert_eq!(namespaces.len(), 1);

    let classes =
        query_nodes(&tree, &language, src, "(class_declaration) @cls").expect("C# class query");
    assert_eq!(classes.len(), 1);

    let file_scoped_src = "namespace MyApp.Other;\npublic class Worker {}\n";
    let file_scoped_tree =
        parse(language.clone(), file_scoped_src).expect("C# file-scoped parse should succeed");
    assert!(!file_scoped_tree.root_node().has_error());

    let file_scoped_ns = query_nodes(
        &file_scoped_tree,
        &language,
        file_scoped_src,
        "(file_scoped_namespace_declaration) @ns",
    )
    .expect("C# file-scoped namespace query");
    assert_eq!(file_scoped_ns.len(), 1);

    let block_ns_in_file_scoped = query_nodes(
        &file_scoped_tree,
        &language,
        file_scoped_src,
        "(namespace_declaration) @ns",
    )
    .expect("C# block namespace query over file-scoped src");
    assert_eq!(block_ns_in_file_scoped.len(), 0);

    let top_level_src = "Console.WriteLine(\"hi\");\nvar x = 1;\n";
    let top_level_tree =
        parse(language.clone(), top_level_src).expect("C# top-level parse should succeed");
    assert!(!top_level_tree.root_node().has_error());
}
