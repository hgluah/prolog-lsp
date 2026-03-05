use std::hash;

use indexmap::IndexSet;
use lsp_types::{CompletionItem, CompletionItemKind, CompletionResponse};
use rustc_hash::FxBuildHasher;
use smallvec::SmallVec;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, QueryCursor};

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

    macro_rules! item {
        ($name:expr, $kind:expr) => {
            CompletionItem {
                label: $name.into(),
                kind: Some($kind),
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

    fn index_in_parent(mut node: Node) -> usize {
        let mut index = 0;
        while let Some(prev) = node.prev_sibling() {
            index += 1;
            node = prev;
        }
        index
    }

    let (_kind, node, name) = match node {
        Some(&(kind, node)) => (kind, Some(node), {
            let mut range = node.start_byte()..node.end_byte();
            if let Some(row_start) = document.text.br_indexes.row_start(pos.row) {
                range.end = (row_start + pos.column).clamp(range.start, range.end);
            };
            str::from_utf8(&document.text.text.as_bytes()[range])?
        }),
        None => (COMPLETE::Atom, None, ""),
    };

    let mut indices = SmallVec::<[usize; 8]>::new();
    let function = 'ok: {
        'err: {
            if let Some(node) = node {
                let mut tmp = node;
                while {
                    indices.push(index_in_parent(tmp));
                    tmp = match tmp.parent() {
                        Some(tmp) => tmp,
                        None => break 'err,
                    };
                    tmp.kind() != "functional_notation"
                } {}
                break 'ok tmp;
            }
        }
        return Ok(CompletionResponse::Array(filter_prefixed(
            name,
            // TODO Also add imports
            document.functions_and_facts.iter().map(|function| {
                item!(
                    (&*function.head.name).to_owned(),
                    CompletionItemKind::FUNCTION
                )
            }),
        )?));
    };

    let function_name = function.child_by_field_name("function").unwrap();
    if function_name.kind() != "atom" || function_name.child_count() != 0 {
        todo!() // TODO
    }
    let function_name = function_name.utf8_text(&document.text.text.as_bytes())?;

    // TODO Also add imports
    let completions = document.functions_and_facts.iter().filter_map(|function| {
        if &*function.head.name == function_name {
            let mut indices = indices.iter().copied().rev();
            let mut param = function.head.parameters.get(indices.next().unwrap())?;
            for index in indices {
                let Argument::List(_, args) = param else {
                    return None;
                };
                param = args.get(index)?;
            }
            Some(param)
        } else {
            None
        }
    });

    Ok(CompletionResponse::Array(filter_prefixed(
        name,
        completions.map(|arg| {
            item!(
                match arg {
                    Argument::Number(node) => &**node,
                    Argument::Atom(node) => &**node,
                    Argument::String(node) => &**node,
                    Argument::Variable(node) => &**node,
                    Argument::List(_, _) => "[",
                    Argument::Function(node) => &*node.name,
                },
                match arg {
                    Argument::Number(_) => CompletionItemKind::VALUE,
                    Argument::Atom(_) => CompletionItemKind::CONSTANT,
                    Argument::String(_) => CompletionItemKind::TEXT,
                    Argument::Variable(_) => CompletionItemKind::VARIABLE,
                    Argument::List(_, _) => CompletionItemKind::SNIPPET,
                    Argument::Function(_) => CompletionItemKind::FUNCTION,
                }
            )
        }),
    )?))
}
