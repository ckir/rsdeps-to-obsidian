/// Shared tree-sitter parse/query helpers for the Python (and later
/// TypeScript) backends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchedNode {
    pub capture: String,
    pub text: String,
    pub start_row: usize,
    pub end_row: usize,
}

/// Parse `src` with `language`. `set_language`/`parse` failures become
/// `Err`; never panics.
pub fn parse(language: tree_sitter::Language, src: &str) -> Result<tree_sitter::Tree, String> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .map_err(|e| format!("set_language failed: {e}"))?;
    parser
        .parse(src, None)
        .ok_or_else(|| "parse failed: tree-sitter returned no tree".to_string())
}

/// Run the tree-sitter query `query_src` over `tree` and collect every
/// capture as a [`MatchedNode`] (rows 0-based as reported; consumers add 1
/// for `SourceSpan`). A bad query becomes `Err` with the query error.
pub fn query_nodes(
    tree: &tree_sitter::Tree,
    language: &tree_sitter::Language,
    src: &str,
    query_src: &str,
) -> Result<Vec<MatchedNode>, String> {
    let query =
        tree_sitter::Query::new(language, query_src).map_err(|e| format!("invalid query: {e}"))?;
    let mut cursor = tree_sitter::QueryCursor::new();
    let src_bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut matches = cursor.matches(&query, tree.root_node(), src_bytes);
    use tree_sitter::StreamingIterator as _;
    while let Some(m) = {
        matches.advance();
        matches.get()
    } {
        for capture in m.captures() {
            let name = query.capture_names()[capture.index as usize].to_string();
            let node = capture.node;
            let text = node
                .utf8_text(src_bytes)
                .map_err(|e| format!("node text is not valid utf8: {e}"))?
                .to_string();
            out.push(MatchedNode {
                capture: name,
                text,
                start_row: node.start_position().row,
                end_row: node.end_position().row,
            });
        }
    }
    Ok(out)
}
