use either::Either;
use lsp_types::{Diagnostic, DiagnosticSeverity, PublishDiagnosticsParams, Range, Uri};
use texter::change::GridIndex;
use tracing::warn;
use tree_sitter_traversal::traverse_tree;

use crate::lsp::document::{Document, Documents};

fn diagnostics_single(documents: &Documents, doc: &Document) -> impl Iterator<Item = Diagnostic> {
    let sintactic = traverse_tree(&doc.tree, tree_sitter_traversal::Order::Post)
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
        });

    let multiple_exports = (doc.exports.len() > 1).then(|| Diagnostic {
        range: match &doc.exports[1] {
            Ok(it) => it.module_name.position,
            Err(err) => err.position,
        },
        severity: Some(DiagnosticSeverity::ERROR),
        message: "Multiple module directives".to_owned(),
        ..Default::default()
    });

    let non_existant_exports = doc
        .exports
        .iter()
        .flat_map(|export| match export {
            Ok(it) => Either::Left(it.exported.iter().map(Ok)),
            Err(err) => Either::Right(std::iter::once(Err(err))),
        })
        .filter(|export| {
            let Ok((exported, arity)) = export else {
                return true;
            };

            // O(n^2) but n is so small that it's prob better than having to alloc a map
            !doc.functions_and_facts.iter().any(|function| {
                (&*function.name) == &**exported && arity.parse() == Ok(function.parameters.len()) // TODO Allow arity to be a hex literal etc
            })
        })
        .map(|export| match export {
            Ok((function, arity)) => Diagnostic {
                range: function.position,
                severity: Some(DiagnosticSeverity::ERROR),
                message: format!("Export {}/{} not defined", &**function, &**arity),
                ..Default::default()
            },
            Err(msg) => Diagnostic {
                range: msg.position,
                severity: Some(DiagnosticSeverity::ERROR),
                message: (&**msg).to_owned(),
                ..Default::default()
            },
        });

    std::iter::chain(
        sintactic,
        std::iter::chain(multiple_exports, non_existant_exports),
    )
}

pub fn diagnostics(
    documents: &Documents,
    uri: Uri,
) -> impl IntoIterator<Item = PublishDiagnosticsParams> {
    let diagnostics = std::iter::chain(
        Some((
            uri.clone(),
            diagnostics_single(documents, &documents.get(&uri).unwrap()),
        )),
        documents
            .iter()
            .filter(move |(_, document)| {
                document.imports.iter().any(|import| {
                    let uri = uri.as_str().as_bytes();
                    let import = import.as_bytes();
                    uri.ends_with(import) && uri.get(uri.len() - import.len()) == Some(&b'/')
                })
            })
            .map(|(uri, document)| (uri.clone(), diagnostics_single(documents, document))),
    );

    diagnostics
        .map(|(uri, diagnostics)| PublishDiagnosticsParams::new(uri, diagnostics.collect(), None))
}
