use std::hash;

use indexmap::IndexSet;
use lsp_types::{CompletionItem, CompletionItemKind, CompletionResponse};
use rustc_hash::FxBuildHasher;
use streaming_iterator::StreamingIterator;
use tree_sitter::QueryCursor;

use crate::lsp::{
    document::Document,
    queries::{self, COMPLETE},
};
use texter::change::GridIndex;

use super::document::Argument;

pub fn completions(pos: GridIndex, document: &Document) -> anyhow::Result<CompletionResponse> {
    let pos = pos.into();
    let mut cursor = QueryCursor::new();

    let mut node = queries::completions(
        &mut cursor,
        document.tree.root_node(),
        document.text.text.as_bytes(),
    )
    .filter(|(_, node)| node.end_position() > pos);
    let node = node.next();

    let (kind, name) = match node {
        Some(&(kind, node)) => (kind, {
            let mut range = node.start_byte()..node.end_byte();
            if let Some(row_start) = document.text.br_indexes.row_start(pos.row) {
                range.end = (row_start + pos.column).clamp(range.start, range.end);
            };
            str::from_utf8(&document.text.text.as_bytes()[range])?
        }),
        None => (COMPLETE::Atom, ""),
    };

    macro_rules! item {
        ($name:expr, $kind:ident) => {
            CompletionItem {
                label: $name,
                kind: Some(CompletionItemKind::$kind),
                ..Default::default()
            }
        };
    }

    fn filter_prefixed(
        name: &str,
        iter: impl Iterator<Item = CompletionItem>,
    ) -> anyhow::Result<Vec<CompletionItem>> {
        #[derive(PartialEq)]
        #[repr(transparent)]
        struct Wrapper(CompletionItem);
        impl hash::Hash for Wrapper {
            fn hash<H: hash::Hasher>(&self, state: &mut H) {
                self.0.label.hash(state);
            }
        }
        impl Eq for Wrapper {}
        Ok(iter
            .filter(|completion| name != completion.label && completion.label.starts_with(name))
            .map(Wrapper)
            .fold(
                IndexSet::<_, FxBuildHasher>::with_hasher(FxBuildHasher),
                |mut acc, x| {
                    acc.insert(x);
                    acc
                },
            )
            .into_iter()
            .map(|Wrapper(x)| x)
            .collect())
    }

    // TODO Rework based on parameters
    let completions = match kind {
        COMPLETE::Atom => filter_prefixed(
            name,
            document.functions_and_facts.iter().flat_map(|f| {
                std::iter::chain(
                    Some(item!((&*f.name).to_owned(), FUNCTION)),
                    f.declared_args.iter().filter_map(|arg| match arg {
                        Argument::Atom(name) => Some(item!((&**name).to_owned(), CONSTANT)),
                        _ => None,
                    }),
                )
            }),
        ),
        COMPLETE::Variable => filter_prefixed(
            name,
            document.functions_and_facts.iter().flat_map(|f| {
                f.declared_args.iter().filter_map(|arg| match arg {
                    Argument::Variable(name) => Some(item!((&**name).to_owned(), VARIABLE)),
                    _ => None,
                })
            }),
        ),
    }?;

    Ok(CompletionResponse::Array(completions))
}
