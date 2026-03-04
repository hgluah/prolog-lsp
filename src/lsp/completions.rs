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
    .filter(|node| node.end_position() > pos);
    let node = node.next();

    let default = COMPLETE::Atom(document.tree.root_node()); // The inner node is not going to be used anymore
    let (node, name) = match node {
        Some(node) => (node, node.utf8_text(document.text.text.as_bytes())?),
        None => (&default, ""),
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
                IndexSet::<_, FxBuildHasher>::with_hasher(FxBuildHasher::default()),
                |mut acc, x| {
                    acc.insert(x);
                    acc
                },
            )
            .into_iter()
            .map(|Wrapper(x)| x)
            .collect())
    }

    let completions = match node {
        COMPLETE::Atom(_) => filter_prefixed(
            name,
            document.functions.iter().flat_map(|f| {
                std::iter::chain(
                    Some(item!(f.name.as_str().to_owned(), FUNCTION)),
                    f.declared_args.iter().filter_map(|arg| match arg {
                        Argument::Atom(name) => Some(item!(name.as_str().to_owned(), CONSTANT)),
                        Argument::Variable(_) => None,
                    }),
                )
            }),
        ),
        COMPLETE::Variable(_) => filter_prefixed(
            name,
            document.functions.iter().flat_map(|f| {
                f.declared_args.iter().filter_map(|arg| match arg {
                    Argument::Atom(_) => None,
                    Argument::Variable(name) => Some(item!(name.as_str().to_owned(), VARIABLE)),
                })
            }),
        ),
    }?;

    Ok(CompletionResponse::Array(completions))
}
