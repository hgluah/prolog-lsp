use std::sync::{LazyLock, Mutex};

use anyhow::bail;
use lsp_types::Uri;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use smol_str::SmolStr;
use tree_sitter::{Node, Parser, QueryCursor, StreamingIterator, Tree};

use texter::{change::GridIndex, core::text::Text};

use crate::lsp::queries::search_functions;

use super::queries::SEARCH_FUNCTIONS;

pub static DOCUMENTS: LazyLock<Mutex<Documents>> = LazyLock::new(Mutex::default);
type Documents = FxHashMap<Uri, Document>;
impl Document {
    pub fn new(tree: Tree, text: Text, parser: &mut Parser) -> anyhow::Result<Self> {
        let mut res = Self {
            tree,
            text,
            imports: SmallVec::new(),
            exports: SmallVec::new(),
            functions: SmallVec::new(),
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
        self.functions.clear();

        let mut cursor = QueryCursor::new();
        let mut functions = search_functions(
            &mut cursor,
            self.tree.root_node(),
            self.text.text.as_bytes(),
        );
        if let Some(name) = functions.next() {
            let function = match name {
                SEARCH_FUNCTIONS::Function(node) => node,
                node => bail!("Unexpected node {:?}", node),
            };

            let get_name = |name: &Node| -> anyhow::Result<_> {
                Ok(name.utf8_text(self.text.text.as_bytes())?.into())
            };
            let new_function = |name: &Node| -> anyhow::Result<_> {
                Ok(Function {
                    name: get_name(name)?,
                    parameters: SmallVec::new(),
                    declared_args: SmallVec::new(),
                    inner_variables: SmallVec::new(),
                })
            };

            let name = new_function(function);
            let last_function = functions.fold(name, |function, x| {
                let mut function = function?;
                function.declared_args.push(match x {
                    SEARCH_FUNCTIONS::Function(name) => {
                        self.functions.push(function);
                        return new_function(name);
                    }
                    SEARCH_FUNCTIONS::Atom(node) => Argument::Atom(get_name(node)?),
                    SEARCH_FUNCTIONS::Variable(node) => Argument::Variable(get_name(node)?),
                });
                Ok(function)
            })?;

            self.functions.push(last_function);
        }

        Ok(())
    }
}
pub struct Document {
    pub tree: Tree,
    pub text: Text,
    pub imports: SmallVec<[SmolStr; 16]>,
    pub exports: SmallVec<[SmolStr; 32]>,
    pub functions: SmallVec<[Function; 32]>,
}
pub struct Function {
    pub name: SmolStr,
    pub parameters: SmallVec<[SmolStr; 8]>,
    pub declared_args: SmallVec<[Argument; 8]>,
    pub inner_variables: SmallVec<[SmolStr; 16]>,
}
pub enum Argument {
    Atom(SmolStr),
    Variable(SmolStr),
}
