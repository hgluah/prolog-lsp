use std::{
    fmt::{self, Write},
    ops::Deref,
};

use lsp_types::{Range, Uri};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use smol_str::SmolStr;
use tracing::info;
use tree_sitter::{Node, Parser, QueryCursor, StreamingIterator, Tree};

use texter::{change::GridIndex, core::text::Text};

use crate::lsp::queries::{clauses, module};

use super::queries::CLAUSES;

pub type Documents = FxHashMap<Uri, Document>;
impl Document {
    pub fn new(tree: Tree, text: Text, parser: &mut Parser) -> anyhow::Result<Self> {
        let mut res = Self {
            tree,
            text,
            imports: SmallVec::new(),
            exports: SmallVec::new(),
            functions_and_facts: SmallVec::new(),
        };
        res.recompute(parser, None)?;
        Ok(res)
    }
    pub fn recompute(
        &mut self,
        parser: &mut Parser,
        pos: Option<&mut GridIndex>,
    ) -> anyhow::Result<()> {
        self.tree = parser
            .parse(self.text.text.as_str(), Some(&self.tree))
            .unwrap();
        if let Some(pos) = pos {
            pos.normalize(&mut self.text)?;
        }

        self.imports.clear();
        self.exports.clear();
        self.functions_and_facts.clear();

        let mut cursor = QueryCursor::new();
        self.exports = module(
            &mut cursor,
            self.tree.root_node(),
            self.text.text.as_bytes(),
        )
        .cloned()
        .map(|x| {
            let (module_name, exported) = match x {
                Ok(it) => it,
                Err(err) => return Ok(Err(err)),
            };
            Ok(Ok(Exports {
                module_name: MiniNode::new(module_name, &self.text.text)?,
                exported: exported
                    .into_iter()
                    .map(|(function, arity)| {
                        Ok((
                            MiniNode::new(function, &self.text.text)?,
                            MiniNode::new(arity, &self.text.text)?,
                        ))
                    })
                    .collect::<Result<_, std::str::Utf8Error>>()?,
            }))
        })
        .collect::<Result<_, std::str::Utf8Error>>()?;

        let mut cursor = QueryCursor::new();
        self.functions_and_facts.extend(
            clauses(
                &mut cursor,
                self.tree.root_node(),
                self.text.text.as_bytes(),
            )
            .map_deref(|&x| x)
            .flat_map(|(kind, node)| {
                match kind {
                    CLAUSES::Atom => None,     // TODO
                    CLAUSES::Function => None, // TODO
                    CLAUSES::Op => {
                        'err: {
                            if node
                                .child_by_field_name("operator")
                                .unwrap()
                                .utf8_text(&self.text.text.as_bytes())
                                != Ok(":-")
                                || node.child_count() != 3
                            {
                                break 'err;
                            }
                            let function = node.child(0).unwrap();

                            return Some(FunctionOrFact {
                                head: Self::parse_funtion_head(function, &self.text.text)
                                    .unwrap_or_else(|_| todo!() /* TODO */),
                                inner_variables: SmallVec::new(),
                            });
                        }
                        todo!() // TODO
                    }
                }
            }),
        );

        info!("{self:#?}");

        Ok(())
    }

    fn parse_arg(node: Node, text: impl AsRef<[u8]>) -> anyhow::Result<Argument> {
        use Argument::*;
        Ok(match node.kind() {
            "integer" => Number(MiniNode::new(node, text)?),
            "float_number" => Number(MiniNode::new(node, text)?),
            "atom" => Atom(MiniNode::new(node, text)?),
            "double_quoted_list_notation" => String(MiniNode::new(node, text)?),
            "variable_term" => Variable(MiniNode::new(node, text)?),
            "list_notation" => List({
                let mut cursor = node.walk();
                node.children(&mut cursor)
                    .skip(1)
                    .step_by(2)
                    .map(|child| Self::parse_arg(child, text.as_ref()))
                    .collect::<Result<_, _>>()?
            }),
            "functional_notation" => Function(Box::new(Self::parse_funtion_head(node, text)?)),
            _ => return Err(todo!()), // TODO
        })
    }

    fn parse_funtion_head(
        function: Node,
        text: impl AsRef<[u8]>,
    ) -> anyhow::Result<FunctionHeadOrFact> {
        if function.kind() != "functional_notation" || function.child_count() != 4 {
            // TODO
            /*
            test1(X):-
                write(X) % <--- THERE IS NO FINAL .

            :-
                test1(hello).
             */
            return Err(todo!(
                "{:#}",
                function.parent().unwrap().utf8_text(text.as_ref()).unwrap()
            ));
        }
        let function_name = function.child_by_field_name("function").unwrap();
        let args = function.child(2).unwrap();
        if function_name.kind() != "atom"
            || function_name.child_count() != 0
            || args.kind() != "arg_list"
        {
            return Err(todo!()); // TODO
        }

        let mut cursor = args.walk();

        Ok(FunctionHeadOrFact {
            name: MiniNode::new(function_name, text.as_ref())
                .unwrap_or_else(|_| todo!() /* TODO */),
            parameters: {
                let mut res = SmallVec::new();
                for arg in args.children(&mut cursor).step_by(2) {
                    res.push(Self::parse_arg(arg, text.as_ref())?);
                }
                res
            },
        })
    }
}
#[derive(Debug)]
pub struct Document {
    pub tree: Tree,
    pub text: Text,
    pub imports: SmallVec<[MiniNode; 16]>, // TODO Construct
    pub exports: SmallVec<[Result<Exports, MiniNode>; 1]>,
    pub functions_and_facts: SmallVec<[FunctionOrFact; 32]>,
}
#[derive(Debug)]
pub struct Exports {
    pub module_name: MiniNode,
    pub exported: SmallVec<[(MiniNode, MiniNode); 32]>,
}
#[derive(Debug)]
pub struct FunctionHeadOrFact {
    pub name: MiniNode,
    pub parameters: SmallVec<[Argument; 8]>,
}
#[derive(Debug)]
pub struct FunctionOrFact {
    pub head: FunctionHeadOrFact,
    pub inner_variables: SmallVec<[MiniNode; 16]>, // TODO
}
#[derive(Debug)]
pub enum Argument<Function: Deref<Target = FunctionHeadOrFact> = Box<FunctionHeadOrFact>> {
    Number(MiniNode),
    Atom(MiniNode),
    String(MiniNode),
    Variable(MiniNode),
    List(Vec<Argument>),
    Function(Function),
}
impl<Function: Deref<Target = FunctionHeadOrFact>> fmt::Display for Argument<Function> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(node) | Self::Atom(node) | Self::String(node) | Self::Variable(node) => {
                f.write_str(node)
            }
            Self::List(args) => {
                f.write_char('[')?;
                let mut iter = args.iter();
                if let Some(x) = iter.next() {
                    x.fmt(f)?;
                    iter.try_for_each(|x| write!(f, ", {x}"))?;
                }
                f.write_char(']')
            }
            Self::Function(node) => {
                f.write_str(&node.name)?;
                f.write_char('(')?;
                let mut iter = node.parameters.iter();
                if let Some(x) = iter.next() {
                    x.fmt(f)?;
                    iter.try_for_each(|x| write!(f, ", {x}"))?;
                }
                f.write_char(')')
            }
        }
    }
}
#[derive(Clone, Debug)]
pub struct MiniNode {
    pub position: Range,
    pub text: SmolStr,
}
impl MiniNode {
    pub fn new(node: Node, text: impl AsRef<[u8]>) -> Result<Self, std::str::Utf8Error> {
        Ok(Self::at(node, node.utf8_text(text.as_ref())?))
    }
    pub fn at(node: Node, text: impl Into<SmolStr>) -> Self {
        Self {
            position: Self::pos(node),
            text: text.into(),
        }
    }
    pub fn pos(node: Node) -> Range {
        Range {
            start: GridIndex::from(node.start_position()).into(),
            end: GridIndex::from(node.end_position()).into(),
        }
    }
}
impl Deref for MiniNode {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}
