use std::fmt::Write;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemLabelDetails, CompletionResponse,
    Documentation, InsertTextFormat,
};
use rustc_hash::FxBuildHasher;
use smallvec::SmallVec;
use tree_sitter::{Node, Point};

use crate::lsp::{document::Document, queries::Ancestors};
use texter::change::GridIndex;

use super::document::Argument;

pub fn completions(pos: GridIndex, document: &Document) -> anyhow::Result<CompletionResponse> {
    macro_rules! item {
        ($name:expr, $description:expr, $kind:expr, $snippet:expr) => {{
            let insert_text: String = $name.into();
            let (label, label_details, documentation, insert_text, insert_text_format) = if $snippet
            {
                let description = $description.to_string();
                (
                    insert_text[..insert_text.find(['(', '[']).unwrap()].to_owned(),
                    Some(CompletionItemLabelDetails {
                        detail: None,
                        description: Some(description.clone()),
                    }),
                    Some(Documentation::String(description)),
                    Some(insert_text),
                    Some(InsertTextFormat::SNIPPET),
                )
            } else {
                (insert_text, None, None, None, None)
            };
            Ok(CompletionItem {
                label,
                label_details,
                documentation,
                kind: Some($kind),
                insert_text,
                insert_text_format,
                ..Default::default()
            })
        }};
        (@list $str_init:expr, $begin:literal, $end:literal, $iterable:expr) => {{
            let mut str = $str_init;
            str += $begin;
            let mut params = $iterable.iter().enumerate().map(|(i, _)| i + 1);
            if let Some(i) = params.next() {
                write!(str, "${}", i)?;
                // No whitespace on purpose, since otherwise the partial AST
                // makes ERROR nodes, so completion stops working
                params.try_for_each(|i| write!(str, ",${}", i))?;
            }
            str + $end
        }};
    }

    fn filter_prefixed(
        name: &str,
        iter: impl Iterator<Item = anyhow::Result<CompletionItem>>,
    ) -> anyhow::Result<Vec<CompletionItem>> {
        iter.filter(|completion| match completion {
            Ok(completion) => name != completion.label && completion.label.starts_with(name),
            Err(_) => true,
        })
        .collect()
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
                        || (!matches!(
                            child.kind(),
                            "comma" | "semicolon" | "end" | "open_ct" | "open" | "open_list"
                        ) && !child.is_error()))
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
                last_arg_list.parent().unwrap()
            }
            (_, _, None) => {
                return Ok(CompletionResponse::Array(filter_prefixed(
                    name,
                    // TODO Also add imports
                    (&document.functions_and_facts).into_iter().map(|function| {
                        item!(
                            item!(@list
                                (&*function.head.name).to_owned(),
                                "(",
                                ")",
                                function.head.parameters
                            ),
                            Argument::Function(&function.head),
                            CompletionItemKind::FUNCTION,
                            true
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
    let completions = (&document.functions_and_facts)
        .into_iter()
        .filter_map(|function| {
            if &*function.head.name == function_name {
                let mut indices = indices.iter().copied().rev();
                let mut param = function
                    .head
                    .parameters
                    .get((indices.next().unwrap() + offset) / 2)?;
                for index in indices {
                    param = match param {
                        Argument::List(args) => args.get(index / 2)?,
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
            let (label, snippet) = match arg {
                Argument::Number(node)
                | Argument::Atom(node)
                | Argument::String(node)
                | Argument::Variable(node) => ((&**node).to_owned(), false),
                Argument::List(args) => (item!(@list String::new(), "[", "]", args), true),
                Argument::Function(node) => (
                    item!(@list (&*node.name).to_owned(), "(", ")", node.parameters),
                    true,
                ),
            };

            item!(
                label,
                arg,
                match arg {
                    Argument::Number(_) => CompletionItemKind::VALUE,
                    Argument::Atom(_) => CompletionItemKind::CONSTANT,
                    Argument::String(_) => CompletionItemKind::TEXT,
                    Argument::Variable(_) => CompletionItemKind::VARIABLE,
                    Argument::List(_) => CompletionItemKind::SNIPPET,
                    Argument::Function(_) => CompletionItemKind::FUNCTION,
                },
                snippet
            )
        }),
    )?))
}
