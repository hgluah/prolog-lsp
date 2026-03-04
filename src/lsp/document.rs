use std::sync::{LazyLock, Mutex};

use anyhow::bail;
use lsp_types::Uri;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use tree_sitter::{Node, QueryCursor, StreamingIterator, Tree};

use texter::core::text::Text;

use crate::lsp::queries::search_functions;

use super::queries::SEARCH_FUNCTIONS;

pub static DOCUMENTS: LazyLock<Mutex<Documents>> = LazyLock::new(Mutex::default);
type Documents<'tree> = FxHashMap<Uri, Document<'tree>>;
impl<'tree> TryFrom<(Tree, Text)> for Document<'tree> {
    type Error = anyhow::Error;

    fn try_from((tree, text): (Tree, Text)) -> Result<Self, Self::Error> {
        let mut res = Self {
            tree,
            text,
            cursor: QueryCursor::new(),
            imports: SmallVec::new(),
            exports: SmallVec::new(),
            functions: SmallVec::new(),
        };
        res.recompute()?;
        Ok(res)
    }
}
impl<'tree> Document<'tree> {
    pub fn recompute(&'tree mut self) -> anyhow::Result<()> {
        self.imports.clear();
        self.exports.clear();
        self.functions.clear();

        self.cursor = QueryCursor::new();
        let mut functions = search_functions(
            &mut self.cursor,
            self.tree.root_node(),
            self.text.text.as_bytes(),
        );
        if let Some(name) = functions.next() {
            let &name = match name {
                SEARCH_FUNCTIONS::Function(node) => node,
                node => bail!("Unexpected node {:?}", node),
            };

            let last_function = functions.fold(
                Function {
                    name,
                    parameters: SmallVec::new(),
                    declared_args: SmallVec::new(),
                    inner_variables: SmallVec::new(),
                },
                |mut function, x| match x {
                    SEARCH_FUNCTIONS::Function(name) => {
                        self.functions.push(function);
                        Function {
                            name: *name,
                            parameters: SmallVec::new(),
                            declared_args: SmallVec::new(),
                            inner_variables: SmallVec::new(),
                        }
                    }
                    SEARCH_FUNCTIONS::Atom(node) => {
                        function.declared_args.push(Argument::Atom(*node));
                        function
                    }
                    SEARCH_FUNCTIONS::Variable(node) => {
                        function.declared_args.push(Argument::Variable(*node));
                        function
                    }
                },
            );

            self.functions.push(last_function);
        }

        Ok(())
    }
}
pub struct Document<'tree> {
    pub tree: Tree,
    pub text: Text,
    cursor: QueryCursor,
    pub imports: SmallVec<[Node<'tree>; 16]>,
    pub exports: SmallVec<[Node<'tree>; 32]>,
    pub functions: SmallVec<[Function<'tree>; 32]>,
}
pub struct Function<'tree> {
    pub name: Node<'tree>,
    pub parameters: SmallVec<[Node<'tree>; 8]>,
    pub declared_args: SmallVec<[Argument<'tree>; 8]>,
    pub inner_variables: SmallVec<[Node<'tree>; 16]>,
}
pub enum Argument<'tree> {
    Atom(Node<'tree>),
    Variable(Node<'tree>),
}
