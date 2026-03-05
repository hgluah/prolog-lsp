use std::{
    ops::Deref,
    sync::{LazyLock, Mutex},
};

use anyhow::bail;
use lsp_types::{Range, Uri};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use smol_str::SmolStr;
use tree_sitter::{Node, Parser, QueryCursor, StreamingIterator, Tree};

use texter::{change::GridIndex, core::text::Text};

use crate::lsp::queries::{module, search_functions};

use super::queries::SEARCH_FUNCTIONS;

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
        let mut functions = search_functions(
            &mut cursor,
            self.tree.root_node(),
            self.text.text.as_bytes(),
        );
        if let Some(&(kind, node)) = functions.next() {
            let function = match kind {
                SEARCH_FUNCTIONS::Function => node,
                node => bail!("Unexpected node {kind:?} {:?}", node),
            };

            let get_name = |name: Node| -> anyhow::Result<_> {
                Ok(MiniNode::at(
                    node,
                    name.utf8_text(self.text.text.as_bytes())?,
                ))
            };
            let new_function = |name| -> anyhow::Result<_> {
                Ok(FunctionOrFact {
                    name: get_name(name)?,
                    parameters: SmallVec::new(),
                    declared_args: SmallVec::new(),
                    inner_variables: SmallVec::new(),
                })
            };

            let name = new_function(function);
            let last_function = functions.fold(name, |function, &(kind, node)| {
                let mut function = function?;
                function.declared_args.push(match kind {
                    SEARCH_FUNCTIONS::Function => {
                        self.functions_and_facts.push(function);
                        return new_function(node);
                    }
                    SEARCH_FUNCTIONS::Atom => Argument::Atom(get_name(node)?),
                    SEARCH_FUNCTIONS::Variable => Argument::Variable(get_name(node)?),
                });
                Ok(function)
            })?;

            self.functions_and_facts.push(last_function);
        }

        Ok(())
    }
}
pub struct Document {
    pub tree: Tree,
    pub text: Text,
    pub imports: SmallVec<[MiniNode; 16]>,
    pub exports: SmallVec<[Result<Exports, MiniNode>; 1]>,
    pub functions_and_facts: SmallVec<[FunctionOrFact; 32]>,
}
pub struct Exports {
    pub module_name: MiniNode,
    pub exported: SmallVec<[(MiniNode, MiniNode); 32]>,
}
pub struct FunctionOrFact {
    pub name: MiniNode,
    pub parameters: SmallVec<[MiniNode; 8]>,
    pub declared_args: SmallVec<[Argument; 8]>,
    pub inner_variables: SmallVec<[MiniNode; 16]>,
}
pub enum Argument {
    Atom(MiniNode),
    Variable(MiniNode),
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
            position: Range {
                start: GridIndex::from(node.start_position()).into(),
                end: GridIndex::from(node.end_position()).into(),
            },
            text: text.into(),
        }
    }
}
impl Deref for MiniNode {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}
