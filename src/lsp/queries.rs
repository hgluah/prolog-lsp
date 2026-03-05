use std::sync::LazyLock;

use lsp_types::Range;
use smallvec::SmallVec;
use smol_str::SmolStr;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, TextProvider};

use crate::lsp::document::MiniNode;

macro_rules! query {
    ($QUERY:ident, $query_lit:literal, $($capture:ident: $variant:ident),* $(,)?) => {
        query!(
            @node-filter m => m.node.child_count() == 0, $QUERY, $query_lit, $($capture: $variant),*
        );
    };
    (@node-filter $node_filter_id:ident => $node_filter:expr, $QUERY:ident, $query_lit:literal, $($capture:ident: $variant:ident),* $(,)?) => {
        paste::paste! {
            static [<$QUERY _QUERY>]: LazyLock<Query> = LazyLock::new(||
                Query::new(&prolog_grammar::LANGUAGE.into(), $query_lit).unwrap()
            );
            #[allow(non_camel_case_types)]
            struct [<$QUERY _Variables>] {
                $($capture: u32),*
            }
            static [<$QUERY _VARS>]: LazyLock<[<$QUERY _Variables>]> = LazyLock::new(|| {
                $(let mut $capture = None;)*

                [<$QUERY _QUERY>]
                    .capture_names()
                    .iter()
                    .enumerate()
                    .for_each(|(id, &x)| {
                        let prev = match x {
                            $(stringify!($capture) => $capture.replace(id as u32),)*
                            _ => unreachable!(),
                        };
                        assert!(prev.is_none());
                    });

                [<$QUERY _Variables>] {
                    $($capture: $capture.unwrap(),)*
                }
            });
            #[derive(Clone, Copy, Debug)]
            #[allow(non_camel_case_types)]
            pub enum $QUERY {
                $($variant,)*
            }
            #[allow(non_snake_case)]
            fn [<$QUERY _RAW>]<'tree, I: AsRef<[u8]>>(
                cursor: &'tree mut QueryCursor,
                tree: Node<'tree>,
                text: impl TextProvider<I>,
            ) -> impl streaming_iterator::StreamingIterator<Item = ($QUERY, Node<'tree>)> {
                cursor
                    .matches(&[<$QUERY _QUERY>], tree, text)
                    .map(|qm| match qm.captures {
                        [m] => m,
                        _ => unreachable!(),
                    })
                    .filter_map(|$node_filter_id| match $node_filter_id.index {
                        $(
                            id if id == [<$QUERY _VARS>].$capture => {
                                ($node_filter).then_some(($QUERY::$variant, $node_filter_id.node))
                            }
                        )*
                        _ => unreachable!(),
                    })
            }
        }
    };
}

query!(
    IDENT,
    r#"
        [
            (atom) @name
        ]
    "#,
    name: Ident,
);

query!(
    @node-filter m => true,
    DIRECTIVE,
    r#"(directive_term (functional_notation) @directive)"#,
    directive: Directive,
);

query!(
    SEARCH_FUNCTIONS,
    r#"
        [
            (functional_notation function: (atom) @function)
            (atom) @name
            (variable_term) @var
        ]
    "#,
    function: Function,
    name: Atom,
    var: Variable,
);

query!(
    COMPLETE,
    r#"
        [
            (atom) @name
            (variable_term) @var
        ]
    "#,
    name: Atom,
    var: Variable,
);

struct Ascendants<'tree>(Node<'tree>);
impl<'tree> Iterator for Ascendants<'tree> {
    type Item = Node<'tree>;

    fn next(&mut self) -> Option<Self::Item> {
        let res = self.0.parent();
        if let Some(res) = res {
            self.0 = res;
        }
        res
    }
}

pub fn idents<'tree, I: AsRef<[u8]>>(
    cursor: &'tree mut QueryCursor,
    tree: Node<'tree>,
    text: impl TextProvider<I>,
) -> impl StreamingIterator<Item = Node<'tree>> {
    IDENT_RAW(cursor, tree, text).map(|&(IDENT::Ident, x)| x)
}

pub fn module<'tree>(
    cursor: &'tree mut QueryCursor,
    tree: Node<'tree>,
    text: &'tree [u8],
) -> impl StreamingIterator<
    Item = Result<(Node<'tree>, SmallVec<[(Node<'tree>, Node<'tree>); 32]>), MiniNode>,
> {
    DIRECTIVE_RAW(cursor, tree, text)
        .map(|&(DIRECTIVE::Directive, x)| x)
        .filter(|module| {
            module.child_by_field_name("function").is_some_and(|atom| {
                atom.kind() == "atom"
                    && atom.child_count() == 0
                    && atom.utf8_text(text) == Ok("module")
            })
        })
        .map(|&module| {
            'err: {
                if let Some(args) = module.child(2 /* arg_list */)
                    && args.kind() == "arg_list" && args.child_count() == 3 {
                    let module_name = args.child(0).unwrap();
                    let exported = args.child(2).unwrap();

                    if module_name.kind() != "atom"
                        || module_name.child_count() != 0
                        || exported.kind() != "list_notation" {
                        break 'err;
                    }
                    let mut res = SmallVec::new();
                    for arg in exported.children(&mut exported.walk()).skip(1).step_by(2) {
                        if arg.kind() != "operator_notation"
                            || arg.child_count() != 3
                            || arg.child_by_field_name("operator").unwrap().utf8_text(text) != Ok("/") {
                            break 'err;
                        }
                        let function_name = arg.child(0).unwrap();
                        let arity = arg.child(2).unwrap();
                        if function_name.kind() != "atom"
                            || module_name.child_count() != 0
                            || arity.kind() != "integer" {
                            break 'err;
                        }
                        res.push((function_name, arity));
                    }
                    return Ok((module_name, res))
                }
            }
            Err(MiniNode::at(
                module,
                SmolStr::new_static("Arguments of `module` must be the module name and a list of 'function-name/arity' nodes"),
            ))
        })
}

pub fn search_functions<'tree>(
    cursor: &'tree mut QueryCursor,
    tree: Node<'tree>,
    text: &'tree [u8],
) -> impl StreamingIterator<Item = (SEARCH_FUNCTIONS, Node<'tree>)> {
    SEARCH_FUNCTIONS_RAW(cursor, tree, text)
        .filter(|(kind, node)| {
            !matches!(kind, SEARCH_FUNCTIONS::Atom if
                matches!(
                    node.parent(),
                    Some(p) if p.kind() == "functional_notation" && p.child(0).unwrap() == *node,
                )
            )
        })
        .filter(|(kind, node)| {
            let function = match kind {
                SEARCH_FUNCTIONS::Atom | SEARCH_FUNCTIONS::Variable => {
                    Ascendants(*node).find(|p| p.kind() == "functional_notation")
                }
                SEARCH_FUNCTIONS::Function => Some(node.parent().unwrap()),
            };
            match function.and_then(|function| function.parent()) {
                Some(op) => match op.kind() {
                    "clause_term" => true,
                    "operator_notation" => op.child_by_field_name("operator").is_some_and(|op| {
                        op.kind() == "binary_operator" && op.utf8_text(text) == Ok(":-")
                    }),
                    _ => false,
                },
                _ => false,
            }
        })
}

pub fn completions<'tree, I: AsRef<[u8]>>(
    cursor: &'tree mut QueryCursor,
    tree: Node<'tree>,
    text: impl TextProvider<I>,
) -> impl StreamingIterator<Item = (COMPLETE, Node<'tree>)> {
    COMPLETE_RAW(cursor, tree, text)
}

#[cfg(test)]
mod tests {
    use tree_sitter::{Parser, QueryCursor, StreamingIterator};

    use super::*;

    #[test]
    fn test_module() {
        let mut parser = Parser::new();
        parser
            .set_language(&prolog_grammar::LANGUAGE.into())
            .unwrap();

        let text = r#"
                :- module(doggos, [
                    is_dog/1,
                    my_dogs/2
                ]).

                is_dog(dog).
                my_dogs(dog1, dog2).
            "#;
        let tree = parser.parse(text, None).unwrap();

        let mut cursor = QueryCursor::new();
        let matches = module(&mut cursor, tree.root_node(), text.as_bytes());
        let res = matches.fold(String::new(), |mut acc, node| {
            let (module_name, exported) = node.as_ref().unwrap();
            let module_name = module_name.utf8_text(text.as_bytes()).unwrap();
            let exported = exported
                .iter()
                .map(|(function, arity)| {
                    format!(
                        "{}/{}, ",
                        function.utf8_text(text.as_bytes()).unwrap(),
                        arity.utf8_text(text.as_bytes()).unwrap(),
                    )
                })
                .fold(String::new(), |acc, x| acc + &x);
            acc += &format!("\n{}({})", module_name, exported);
            acc
        });
        assert_eq!(
            res,
            r#"
doggos(is_dog/1, my_dogs/2, )"#
        );
    }

    #[test]
    fn test_search() {
        let mut parser = Parser::new();
        parser
            .set_language(&prolog_grammar::LANGUAGE.into())
            .unwrap();

        let text = r#"
                is_dog(dog).

                are_dogs([]).
                are_dogs([Head | Tail]) :-
                    is_dog(Head),
                    are_dogs(Tail).
            "#;
        let tree = parser.parse(text, None).unwrap();

        let mut cursor = QueryCursor::new();
        let matches = search_functions(&mut cursor, tree.root_node(), text.as_bytes());
        let res = matches.fold(String::new(), |mut acc, (kind, node)| {
            acc += &format!("\n{kind:?}({})", node.utf8_text(text.as_bytes()).unwrap());
            acc
        });
        assert_eq!(
            res,
            r#"
Function(is_dog)
Atom(dog)
Function(are_dogs)
Function(are_dogs)
Variable(Head)
Variable(Tail)"#
        );
    }

    #[test]
    fn test_completions() {
        let mut parser = Parser::new();
        parser
            .set_language(&prolog_grammar::LANGUAGE.into())
            .unwrap();

        let text = r#"
                is_dog(dog).

                are_dogs([]).
                are_dogs([Head | Tail]) :-
                    is_dog(Head),
                    are_dogs(Tail).
            "#;
        let tree = parser.parse(text, None).unwrap();

        let mut cursor = QueryCursor::new();
        let matches = completions(&mut cursor, tree.root_node(), text.as_bytes());
        let res = matches.fold(String::new(), |mut acc, (kind, node)| {
            acc += &format!("\n{kind:?}({})", node.utf8_text(text.as_bytes()).unwrap());
            acc
        });
        assert_eq!(
            res,
            r#"
Atom(is_dog)
Atom(dog)
Atom(are_dogs)
Atom(are_dogs)
Variable(Head)
Variable(Tail)
Atom(is_dog)
Variable(Head)
Atom(are_dogs)
Variable(Tail)"#
        );
    }
}
