use lsp_types::{Diagnostic, DiagnosticSeverity, PublishDiagnosticsParams, Range, Uri};
use texter::change::GridIndex;
use tree_sitter_traversal::traverse_tree;

use crate::lsp::document::Documents;

pub fn diagnostics<'a, 'b>(
    documents: &'a Documents,
    uri: Uri,
) -> impl IntoIterator<Item = PublishDiagnosticsParams> + use<'b> {
    let diagnostics = traverse_tree(
        &documents.get(&uri).unwrap().tree,
        tree_sitter_traversal::Order::Post,
    )
    .filter(|node| node.is_error() || node.is_missing())
    .map(|node| Diagnostic {
        range: Range::new(
            GridIndex::from(node.start_position()).into(),
            GridIndex::from(node.end_position()).into(),
        ),
        severity: Some(DiagnosticSeverity::ERROR),
        message: {
            let is_missing = node.is_missing();
            let mut msg = String::new();
            if let Some(mut iter) = node.language().lookahead_iterator(node.parse_state()) {
                let mut iter = iter.iter_names().peekable();
                if let Some(first) = iter.next() {
                    if iter.next().is_none() {
                        msg += "Expected ";
                        msg += first;
                    } else {
                        msg += "Expected one of {";
                        msg += first;
                        iter.for_each(|name| {
                            msg += ", ";
                            msg += name;
                        });
                        msg += "}";
                    }
                }
            } else {
                msg += if is_missing {
                    "Missing token"
                } else {
                    "Unexpected syntax"
                };
            }
            msg
        },
        ..Default::default()
    })
    .collect();

    Some(PublishDiagnosticsParams::new(uri, diagnostics, None))
}
