use lsp_types::{Location, ReferenceContext, Uri};
use texter::change::GridIndex;
use tree_sitter::{QueryCursor, StreamingIterator};

use crate::lsp::{document::Documents, queries};

pub fn references(
    pos: GridIndex,
    ReferenceContext {
        include_declaration,
    }: ReferenceContext,
    documents: &Documents,
    uri: Uri,
) -> anyhow::Result<Option<Vec<Location>>> {
    let pos = pos.into();
    let mut cursor = QueryCursor::new();

    let document = documents.get(&uri).unwrap();

    let mut node = queries::idents(
        &mut cursor,
        document.tree.root_node(),
        document.text.text.as_bytes(),
    )
    .filter(|node| node.end_position() > pos);
    let node = node.next();

    let name = match node {
        Some(node) => node.utf8_text(&document.text.text.as_bytes())?,
        None => "",
    };

    let _ = name;
    let _ = include_declaration;
    let completions = Vec::new();

    Ok(Some(completions))
}
