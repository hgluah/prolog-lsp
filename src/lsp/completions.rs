use std::hash;

use indexmap::IndexSet;
use lsp_types::{CompletionItem, CompletionItemKind, CompletionResponse};
use rustc_hash::FxBuildHasher;
use smallvec::SmallVec;
use streaming_iterator::StreamingIterator;
use tracing::warn;
use tree_sitter::{Node, Point};

use crate::lsp::{document::Document, queries::Ancestors};
use texter::change::GridIndex;

use super::document::Argument;

pub fn completions(pos: GridIndex, document: &Document) -> anyhow::Result<CompletionResponse> {
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

    /// Similar but not quite like [`descendant_for_point_range`]
    fn last_non_close_in_pos(node: Node, min_pos: Point) -> Node {
        let mut cursor = node.walk();
        match node
            .children(&mut cursor)
            .filter(|child| {
                child.start_position() <= min_pos
                    && !matches!(child.kind(), "close" | "close_list")
                    && (child.start_position() < min_pos
                        || (!matches!(child.kind(), "comma" | "open_ct" | "open" | "open_list")
                            && !child.is_error()))
            })
            .last()
        {
            Some(x) => last_non_close_in_pos(x, min_pos),
            None => node,
        }
    }

    let pos = pos.into();
    let node = last_non_close_in_pos(document.tree.root_node(), pos);

    let mut offset = 0;

    warn!(
        "what - {pos} - {node:?} - {}",
        node.utf8_text(document.text.text.as_bytes()).unwrap()
    );
    warn!("{:#}", node.parent().unwrap());
    let node = if let Some(parent) = node.parent()
        && let Some(grandparent) = parent.parent()
        && grandparent.is_error()
        && node.kind() == "comma"
    {
        offset = 1;
        match parent.kind() {
            "list_notation_separator" => grandparent,
            "arg_list_separator" => {
                if let Some(prev) = grandparent.prev_sibling()
                    && prev.kind() == "arg_list"
                    && prev.child_count() != 0
                {
                    offset = 2;
                    prev.child(prev.child_count() - 1).unwrap()
                } else {
                    grandparent
                }
            }
            _ => node,
        }
    } else {
        node
    };

    let name = if offset == 0 {
        let mut range = node.start_byte()..node.end_byte();
        if let Some(row_start) = document.text.br_indexes.row_start(pos.row) {
            range.end = (row_start + pos.column).clamp(range.start, range.end);
        };
        str::from_utf8(&document.text.text.as_bytes()[range])?
    } else {
        ""
    };

    let mut indices = SmallVec::<[usize; 8]>::new();
    let function = {
        let last_arg_list = std::iter::chain(Some(node), Ancestors(node)).fold(
            (0, 0, None),
            |(idx, last_arg_list_idx, last_arg_list), ancestor| {
                if ancestor.kind() == "arg_list" {
                    (idx, idx, Some(ancestor))
                } else {
                    indices.push(index_in_parent(ancestor));
                    (idx + 1, last_arg_list_idx, last_arg_list)
                }
            },
        );
        match last_arg_list {
            (_, last_arg_list_idx, Some(last_arg_list)) => {
                indices.drain(last_arg_list_idx..);
                warn!("{indices:?} - {:#}", last_arg_list);
                last_arg_list.parent().unwrap()
            }
            (_, _, None) => {
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
            }
        }
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
            let mut param = function
                .head
                .parameters
                .get((indices.next().unwrap() + offset) / 2)?;
            for index in indices {
                param = match param {
                    Argument::List(_, args) => args.get(index / 2)?,
                    Argument::Function(node) => node.parameters.get((index + offset) / 2)?,
                    _ => return None,
                };
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
