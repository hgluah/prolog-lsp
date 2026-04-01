use std::{
    borrow::Borrow,
    collections::BTreeMap,
    fmt::{self, Write},
    marker::ConstParamTy,
    ops::Deref,
    rc::Rc,
};

use either::Either;
use lsp_types::{Range, Uri};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use smol_str::ToSmolStr;
use tracing::info;
use tree_sitter::{Node, Parser, Point, QueryCursor, StreamingIterator, Tree};

use texter::{change::GridIndex, core::text::Text};

use crate::{
    lsp::queries::{clauses, module},
    util::sorted_small_set::{BorrowMap, SortedSmallSet},
};

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
        let mut functions = clauses(
            &mut cursor,
            self.tree.root_node(),
            self.text.text.as_bytes(),
        )
        .map_deref(|&x| x)
        // TODO Replace with map
        .filter_map(|(kind, node)| {
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

                        let function = FunctionOrFact {
                            head: Self::parse_funtion_head(function, &self.text.text)
                                .unwrap_or_else(|_| todo!() /* TODO */),
                            variables: SortedSmallSet::empty(),
                        };
                        let mut unprocessed_params = Vec::new();
                        function.head.parameters.iter().for_each(|arg| {
                            fn variables(res: &mut Vec<UnprocessedVariable>, arg: &Argument) {
                                match arg {
                                    Argument::Number(_)
                                    | Argument::Atom(_)
                                    | Argument::String(_) => (),
                                    Argument::Variable(var) => res.push(var.clone()),
                                    Argument::List(args) => {
                                        args.iter().for_each(|arg| {
                                            variables(res, arg);
                                        });
                                    }
                                    Argument::Function(function) => {
                                        function.parameters.iter().for_each(|arg| {
                                            variables(res, arg);
                                        });
                                    }
                                }
                            }
                            variables(&mut unprocessed_params, arg);
                        });

                        return Some((
                            function.head.name.to_smolstr(),
                            (function, unprocessed_params),
                        ));
                    }
                    todo!() // TODO
                }
            }
        })
        .fold(BTreeMap::<_, Vec<_>>::new(), |mut acc, x| {
            acc.entry(x.0).or_default().push(x.1);
            acc
        });

        while let Some((function, unprocessed_params)) =
            // TODO Replace with some kind of pop then push it to another global dict after it's finished
            functions.values_mut().flatten().next()
        {
            for caller_var in unprocessed_params.drain(..) {
                let domain = caller_var
                    .domain_all_of
                    .into_iter()
                    .map(|usage| match usage {
                        VariableUsage::ArithIs => VariableDomain::SimpleKind(VariableKind::NUMBER.with(Rc::clone)),
                        VariableUsage::PatternMatchEq(res) => todo!(),
                        VariableUsage::NestedCall(nested) => nested
                            .iter()
                            .position(|first_nested| first_nested.function.is_some()) // If there is no function call (e.g. [X|Tail]), then it's "Any"
                            .map(|first_nested| {
                                let nested = unsafe { nested.get_unchecked(first_nested..) };
                                let first_nested = &*unsafe {
                                    nested
                                        .get_unchecked(0)
                                        .function
                                        .unwrap_unchecked()
                                };
                                let domain_any_of = std::iter::chain(
                                    // TODO sure, let's add the first one, but we also need to add all the interemediates THAT ARE NOT INCLUDED BEFORE:
                                    //  e.g. example(test(X, Y)) should check other example's, fine, but also the requirements of test THAT ARE NOT INCLUDED IN THE PREVIOUS example'S
                                    functions[first_nested].iter(),
                                    None, // TODO Add imported functions
                                );
                                let domain_any_of = domain_any_of
                                    .filter_map(|(callee, callees_unprocessed_params)|
                                        // TODO If there's test(X,Y):-... and test(X,Y,Z):-..., then example(test(X,Y)):-... would try to get info about both, even though it shouldn't.
                                        //   It isn't really a big problem since that is a warning anyway
                                        //   This applies to all callers of `get_param_at`, since we just filter_map on outputs known to be invalid, but not on those that seem possible bc we didn't check the arity
                                        callee.head.get_param_at(nested)
                                            .map(|callee_param| (callee_param, callee, callees_unprocessed_params))
                                    );
                                domain_any_of
                                    .map(|(callee_param, callee, callees_unprocessed_params)| {
                                        VariableDomain::from_argument(callee_param, |callee_var| {
                                            // At this point, we have a callee_var, so we have to convert it to a caller_var
                                            let callee_ctx_res = match callee.variables.get(&&**callee_var) {
                                                Some(var) => var.domain.clone(),
                                                None => {
                                                    // TODO This param is still unprocessed...
                                                    todo!("something with {callees_unprocessed_params:?}")
                                                }
                                            };
                                            'caller_ctx_res: {
                                                match callee_ctx_res {
                                                        VariableDomain::SimpleKind(_) => break 'caller_ctx_res callee_ctx_res,
                                                        VariableDomain::FreeInputVariable => // e.g. example(test(X, Y)) where test(X, X) // TODO Test all of these things (these e.g.'s and such)
                                                            Either::Left(std::iter::empty()),
                                                        VariableDomain::NonFreeVariable { any_of } => Either::Right(
                                                            any_of
                                                                .iter()
                                                                .flat_map(|other_param_of_callee|
                                                                    callee.head.get_path_for(other_param_of_callee)
                                                                        .filter_map(|path| function.head.get_param_at(path))
                                                                )
                                                        ),
                                                    }
                                                    .chain(
                                                        callee.head.get_path_for(callee_var)
                                                            .filter_map(|path| function.head.get_param_at(path))
                                                    )
                                                    .filter_map(|caller_param| match caller_param {
                                                        Argument::Variable(new_caller_var) => (&**new_caller_var != &*caller_var.declaration).then_some(Either::Left(std::iter::once(Err(new_caller_var.text.clone())))),
                                                        _ => {
                                                            let res = VariableDomain::from_argument(caller_param, |new_caller_var| {
                                                                // At this point, we already have a caller_var, so we don't need to convert it from a callee_var as before
                                                                if &**new_caller_var == &*caller_var.declaration {
                                                                    // TODO does this make sense?
                                                                    VariableDomain::FreeInputVariable
                                                                } else {
                                                                    VariableDomain::NonFreeVariable { any_of: SortedSmallSet::single(new_caller_var.text.clone()) }
                                                                }
                                                            });
                                                            Some(match res {
                                                                VariableDomain::FreeInputVariable =>
                                                                    // VariableDomain::from_argument can only return this variant if the top level argument
                                                                    // was a variable, and the resolver returned it, but that return is guarded by the same
                                                                    // condition that guards the VariableDomain::from_argument call to begin with
                                                                    unreachable!(),
                                                                VariableDomain::SimpleKind(res) => Either::Left(std::iter::once(Ok(res))),
                                                                VariableDomain::NonFreeVariable { any_of } => Either::Right(any_of.iter().cloned().map(Err))
                                                            })
                                                        }
                                                    })
                                                    .flatten()
                                                    .collect::<VariableDomain>().0
                                            }
                                        })
                                    })
                                    .collect::<VariableDomain>().0
                            })
                            .unwrap_or(VariableDomain::FreeInputVariable),
                    })
                    .try_fold(None, |acc, x| match (acc, x) {
                        (None, x) | (x, None) => Ok(x),
                        (Some(a), Some(b)) => VariableKind::intersection(a, b).map(Some),
                    });

                let domain = match domain {
                    Ok(Some(domain)) => Some(domain),                 // Valid domain
                    Ok(None) => None, // "Any" kind, either bc no restrictions or intersection of "Any"s
                    Err(()) => Some(ExtendedVariableDomain::Invalid), // "Never" / "Invalid" kind
                };

                // TODO .push shouldn't work
                function.variables.push(Variable {
                    declaration: caller_var.declaration,
                    domain,
                    defined_starting_from_point: caller_var.defined_starting_from_point,
                });
            }
            function.variables.sort_by_key(|var| &*var.declaration);
        }

        // self.functions_and_facts.extend(functions);

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
    /// Binary searchable by name
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
impl FunctionHeadOrFact {
    fn get_path_for(
        &self,
        param: &str,
    ) -> impl Iterator<Item = impl Iterator<Item = SingleFunctionOrArrayCall>> {
        xxx
    }

    /// Panics if `nested_call` is empty.\
    /// The first element of `nested_call` doesn't need to reference `self`.
    fn get_param_at(
        &self,
        nested_call: impl IntoIterator<Item = impl Borrow<SingleFunctionOrArrayCall>>,
    ) -> Option<&Argument> {
        let mut nested_call = nested_call.into_iter();
        let first = nested_call.next().unwrap();
        let first = &self.parameters[first.borrow().param_idx];
        nested_call.try_fold(first, |parent, child| match parent {
            Argument::List(args) if child.borrow().function.is_none() => {
                args.get(child.borrow().param_idx)
            }
            Argument::Function(function) => match &child.borrow().function {
                Some(function_name) if &*function.name == &**function_name => {
                    function.parameters.get(child.borrow().param_idx)
                }
                _ => None,
            },
            _ => None,
        })
    }
}
#[derive(Debug)]
pub struct FunctionOrFact {
    pub head: FunctionHeadOrFact,
    pub variables: SortedSmallSet<[Variable; 16], str, VarToName>,
}
struct VarToName;
impl BorrowMap<Variable, str> for VarToName {
    fn borrow_map(x: &Variable) -> &str {
        &x.declaration
    }
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
#[derive(Debug)]
pub struct Variable {
    declaration: MiniNode,
    domain: VariableDomain, // TODO Check for cycles (e.g. test(X, Y):- X=Y, Y=X.)
    defined_starting_from_point: Option<Point>, // TODO Use
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub enum VariableDomain {
    SimpleKind(Rc<VariableKind>),
    NonFreeVariable {
        /// If empty, this is an ill-formed type
        any_of: SortedSmallSet<[Rc<String>; 2]>,
    },
    FreeInputVariable,
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ListVariableDomain {
    SimpleKind {
        any_of: SortedSmallSet<[Rc<VariableKind>; 1]>,
    },
    NonFreeVariable {
        /// If empty, this is an ill-formed type
        any_of: SortedSmallSet<[Rc<String>; 2]>,
    },
    FreeInputVariable,
}
#[derive(PartialEq, Eq, Clone, Copy, ConstParamTy)]
pub enum ReductionKind {
    Union,
    Intersection,
}
pub struct VariableDomainCollector<const RK: ReductionKind>(VariableDomain);
impl<const RK: ReductionKind> FromIterator<Result<Rc<VariableKind>, Rc<String>>>
    for VariableDomainCollector<RK>
{
    fn from_iter<T: IntoIterator<Item = Result<Rc<VariableKind>, Rc<String>>>>(iter: T) -> Self {
        let mut iter = iter.into_iter();
        let Some(it) = iter.next() else {
            return Self(match RK {
                ReductionKind::Union => VariableDomain::FreeInputVariable,
                ReductionKind::Intersection => VariableDomain::NonFreeVariable {
                    any_of: SortedSmallSet::empty(),
                },
            });
        };
        let it = match it {
            Err(it) => it,
            Ok(mut it) => loop {
                // Manual `while let` bc we need to keep the item where the condition is first broken
                match iter.next() {
                    None => return Self(VariableDomain::SimpleKind(it)),
                    Some(Err(it)) => break it, // TODO What about intersection
                    Some(Ok(new_it)) => match RK {
                        ReductionKind::Union => match VariableKind::union(it, new_it) {
                            Ok(x) => it = x,
                            // TODO rn, some things don't make sense
                            // * e.g.
                            //   test(1,1). test(a,a).
                            //   if example(test(X,Y)). then both X and Y would be FreeVars
                            // * e.g.
                            //   test(In,Out,true):-Out=In.
                            //   test(Out,In,false):-Out=In.
                            //   if example(X,Y,Cond). then both X and Y would be FreeVars
                            Err(()) => return Self(VariableDomain::FreeInputVariable),
                        },
                        ReductionKind::Intersection => match VariableKind::intersection(it, new_it)
                        {
                            Ok(x) => it = x,
                            Err(()) => {
                                return Self(VariableDomain::NonFreeVariable {
                                    any_of: SortedSmallSet::empty(),
                                });
                            }
                        },
                    },
                }
            },
        };
        Self(VariableDomain::NonFreeVariable {
            any_of: std::iter::chain(Some(it), iter.filter_map(Result::err)).collect(),
        })
    }
}
impl<const RK: ReductionKind> FromIterator<VariableDomain> for VariableDomainCollector<RK> {
    fn from_iter<T: IntoIterator<Item = VariableDomain>>(iter: T) -> Self {
        // Hack while `Try` is not stable
        let mut iter = iter.into_iter();
        let Some(first) = iter.next() else {
            return Self(VariableDomain::FreeInputVariable);
        };
        // TODO Same "e.g."s as in the other FromIterator
        Self(
            iter.try_fold(first, |a, b| match (a, b) {
                (VariableDomain::FreeInputVariable, _) | (_, VariableDomain::FreeInputVariable) => {
                    Err(())
                }
                (
                    VariableDomain::NonFreeVariable { any_of: a },
                    VariableDomain::NonFreeVariable { any_of: b },
                ) => Ok(VariableDomain::NonFreeVariable {
                    any_of: SortedSmallSet::union(a, b),
                }),
                (x @ VariableDomain::NonFreeVariable { .. }, VariableDomain::SimpleKind(_))
                | (VariableDomain::SimpleKind(_), x @ VariableDomain::NonFreeVariable { .. }) => {
                    Ok(x)
                }
                (VariableDomain::SimpleKind(a), VariableDomain::SimpleKind(b)) => {
                    VariableKind::union(a, b).map(VariableDomain::SimpleKind)
                }
            })
            .unwrap_or(VariableDomain::FreeInputVariable),
        )
    }
}
impl VariableDomain {
    fn from_argument(
        arg: &Argument,
        mut parameter_resolver: impl FnMut(&MiniNode) -> VariableDomain,
    ) -> Self {
        let kind = match arg {
            Argument::Number(_) => VariableKind::NUMBER.with(Rc::clone),
            Argument::Atom(atom) => Rc::new(VariableKind::Atom {
                any_of: SortedSmallSet::single(atom.text.clone()),
            }),
            Argument::String(str) => Rc::new(VariableKind::String {
                any_of: SortedSmallSet::single(str.text.clone()),
            }),
            Argument::Function(function) => Rc::new(VariableKind::Function {
                any_of: SortedSmallSet::single(function.name.text.clone()),
            }),
            Argument::List(args) => {
                let any_of = args
                    .iter()
                    .map(|x| Self::from_argument(x, &mut parameter_resolver))
                    .collect::<SortedSmallSet<_>>();
                if any_of.is_empty() {
                    VariableKind::LIST_ALWAYS_EMPTY.with(Rc::clone)
                } else {
                    Rc::new(VariableKind::List {
                        any_of,
                        emptyable: false,
                    })
                }
            }
            Argument::Variable(var) => return parameter_resolver(var),
        };
        Self::SimpleKind(kind)
    }
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum VariableKind {
    Number,
    Atom {
        any_of: SortedSmallSet<[Rc<String>; 8]>,
    },
    String {
        any_of: SortedSmallSet<[Rc<String>; 8]>,
    },
    /// ([], false) for an invalid list
    /// ([], true) for a list that can only be empty
    /// ([domain, ...], false) for a non-empty list
    /// ([domain, ...], true) for a list that may or may not be empty
    List {
        element_domain: ListVariableDomain,
        emptyable: bool,
    },
    Function {
        any_of: SortedSmallSet<[Rc<String>; 8]>,
    },
}
impl VariableKind {
    thread_local! {
        static NUMBER: Rc<VariableKind> = Rc::new(VariableKind::Number);
        static LIST_INVALID: Rc<VariableKind> = Rc::new(VariableKind::List { any_of: SortedSmallSet::empty(), emptyable: false });
        static LIST_ALWAYS_EMPTY: Rc<VariableKind> = Rc::new(VariableKind::List { any_of: SortedSmallSet::empty(), emptyable: true });
    }

    fn union(a: Rc<Self>, b: Rc<Self>) -> Result<Rc<Self>, ()> {
        Ok(match (&*a, &*b) {
            (Self::Number, Self::Number) => a,
            (Self::Atom { any_of: a }, Self::Atom { any_of: b }) => Rc::new(Self::Atom {
                any_of: unsafe {
                    SortedSmallSet::union_iters(a.into_iter().cloned(), b.into_iter().cloned())
                },
            }),
            (Self::String { any_of: a }, Self::String { any_of: b }) => Rc::new(Self::String {
                any_of: unsafe {
                    SortedSmallSet::union_iters(a.into_iter().cloned(), b.into_iter().cloned())
                },
            }),
            (Self::Function { any_of: a }, Self::Function { any_of: b }) => {
                Rc::new(Self::Function {
                    any_of: unsafe {
                        SortedSmallSet::union_iters(a.into_iter().cloned(), b.into_iter().cloned())
                    },
                })
            }
            (
                Self::List {
                    any_of: domain_a,
                    emptyable: a_maybe_empty,
                },
                Self::List {
                    any_of: domain_b,
                    emptyable: b_maybe_empty,
                },
            ) => {
                match (
                    *a_maybe_empty,
                    *b_maybe_empty,
                    domain_b.is_empty(),
                    domain_a.is_empty(),
                ) {
                    (_a_maybe_empty @ false, _, _a_always_empty @ true, _) => {
                        // TODO Should `Invalid U X` really be = to `X`?
                        b
                    }
                    (_, _b_maybe_empty @ false, _, _b_always_empty @ true) => {
                        // TODO Should `Invalid U X` really be = to `X`?
                        a
                    }
                    (_a_maybe_empty @ true, _, _, _b_always_empty @ true) => a,
                    (_a_maybe_empty @ false, _, _, _b_always_empty @ true) => Rc::new(Self::List {
                        any_of: domain_a.clone(),
                        emptyable: true,
                    }),
                    (_, _b_maybe_empty @ true, _a_always_empty @ true, _) => b,
                    (_, _b_maybe_empty @ false, _a_always_empty @ true, _) => Rc::new(Self::List {
                        any_of: domain_b.clone(),
                        emptyable: true,
                    }),
                    (
                        a_maybe_empty,
                        b_maybe_empty,
                        _a_always_empty @ false,
                        _b_always_empty @ false,
                    ) => Rc::new(Self::List {
                        any_of: unsafe {
                            SortedSmallSet::union_iters(
                                domain_a.into_iter().cloned(),
                                domain_b.into_iter().cloned(),
                            )
                        },
                        emptyable: a_maybe_empty || b_maybe_empty,
                    }),
                }
            }
            _ => return Err(()),
        })
    }

    fn intersection(a: Rc<Self>, b: Rc<Self>) -> Result<Rc<Self>, ()> {
        Ok(match (&*a, &*b) {
            (Self::Number, Self::Number) => a,
            (Self::Atom { any_of: a }, Self::Atom { any_of: b }) => Rc::new(Self::Atom {
                any_of: unsafe {
                    SortedSmallSet::intersection_iters(
                        a.into_iter().cloned(),
                        b.into_iter().cloned(),
                    )
                },
            }),
            (Self::String { any_of: a }, Self::String { any_of: b }) => Rc::new(Self::String {
                any_of: unsafe {
                    SortedSmallSet::intersection_iters(
                        a.into_iter().cloned(),
                        b.into_iter().cloned(),
                    )
                },
            }),
            (Self::Function { any_of: a }, Self::Function { any_of: b }) => {
                Rc::new(Self::Function {
                    any_of: unsafe {
                        SortedSmallSet::intersection_iters(
                            a.into_iter().cloned(),
                            b.into_iter().cloned(),
                        )
                    },
                })
            }
            (
                Self::List {
                    any_of: domain_a,
                    emptyable: a_maybe_empty,
                },
                Self::List {
                    any_of: domain_b,
                    emptyable: b_maybe_empty,
                },
            ) => {
                let emptyable = *a_maybe_empty && *b_maybe_empty;
                let any_of = unsafe {
                    SortedSmallSet::intersection_iters(
                        domain_a.into_iter().cloned(),
                        domain_b.into_iter().cloned(),
                    )
                };
                match (any_of.is_empty(), emptyable) {
                    (true, true) => Self::LIST_ALWAYS_EMPTY.with(Rc::clone),
                    (true, false) => Self::LIST_INVALID.with(Rc::clone),
                    (_, _) => Rc::new(Self::List { any_of, emptyable }),
                }
            }
            _ => return Err(()),
        })
    }
}
#[derive(Debug)]
struct UnprocessedVariable {
    declaration: MiniNode,
    domain_all_of: SmallVec<[VariableUsage; 2]>,
    defined_starting_from_point: Option<Point>,
}
#[derive(Debug)]
enum VariableUsage {
    NestedCall(
        /// Non empty
        SmallVec<[SingleFunctionOrArrayCall; 8]>,
    ),

    // TODO Document that at these Eq and Is MUST **NOT** be any kind of Eq/Is, just those that act as operators, e.g. [X|Tail] = [123], but not when they act as structs, e.g. hello_world(X = 123)
    PatternMatchEq(XXX),
    ArithIs,
}
#[derive(Debug)]
struct SingleFunctionOrArrayCall {
    function: Option<MiniNode>, // TODO We should add the possibility of "function" being "=" or "is". They WOULD **NOT** imply anoything other than the output_type=input_type and output_type=num respectively, since at this point they don't act as actual operators, just as structs with fancy names
    param_idx: usize,
}
#[derive(Clone, Debug)]
pub struct MiniNode {
    pub position: Range,
    pub text: Rc<String>,
}
impl MiniNode {
    pub fn new(node: Node, text: impl AsRef<[u8]>) -> Result<Self, std::str::Utf8Error> {
        Ok(Self::at(node, node.utf8_text(text.as_ref())?))
    }
    pub fn at(node: Node, text: impl Into<String>) -> Self {
        Self {
            position: Self::pos(node),
            text: Rc::new(text.into()),
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
