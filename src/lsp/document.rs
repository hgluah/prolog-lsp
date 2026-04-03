use std::{
    borrow::Borrow,
    collections::BTreeMap,
    fmt::{self, Write},
    iter::{Enumerate, Map, Peekable},
    marker::{ConstParamTy, PhantomData},
    ops::Deref,
    rc::Rc,
    slice::IterMut,
};

use clap::Arg;
use either::Either;
use lsp_types::{Range, Uri};
use rustc_hash::FxHashMap;
use smallvec::{SmallVec, smallvec};
use smol_str::ToSmolStr;
use tracing::info;
use tree_sitter::{Node, Parser, Point, QueryCursor, StreamingIterator, Tree};

use texter::{change::GridIndex, core::text::Text};

use crate::{
    lsp::queries::{clauses, module},
    util::{
        self_aware_iter::SelfAwareIterator,
        sorted_small_set::{SSSHandler, SortedSmallSet},
    },
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
                        struct UnprocessedVariablesGroupper;
                        impl SSSHandler<UnprocessedVariable> for UnprocessedVariablesGroupper {
                            type Key = str;

                            fn key(x: &UnprocessedVariable) -> &Self::Key {
                                &x.declaration
                            }

                            fn reduce(old: &mut UnprocessedVariable, mut new: UnprocessedVariable) {
                                debug_assert_eq!(old.declaration.text.as_str(), &*new.declaration);
                                old.domain_all_of.append(&mut new.domain_all_of);
                                if let Some(new) = new.defined_starting_from_point {
                                    match &mut old.defined_starting_from_point {
                                        Some(old) if new < *old => *old = new,
                                        Some(_) => (),
                                        old @ None => *old = Some(new),
                                    }
                                }
                            }
                        }
                        let mut unprocessed_params = SortedSmallSet::empty();
                        function.head.parameters.iter().for_each(|arg| {
                            fn variables(
                                res: &mut SortedSmallSet<
                                    UnprocessedVariable,
                                    4,
                                    UnprocessedVariablesGroupper,
                                >,
                                arg: &Argument,
                                nested_path: &mut SmallVec<[SingleFunctionOrArrayCall; 6]>,
                            ) {
                                match arg {
                                    Argument::Number(_)
                                    | Argument::Atom(_)
                                    | Argument::String(_) => (),
                                    Argument::Variable(var) => unsafe {
                                        res.entry(&var).insert(UnprocessedVariable {
                                            declaration: var.clone(),
                                            domain_all_of: smallvec![VariableUsage::NestedCall(
                                                nested_path.clone()
                                            )],
                                            defined_starting_from_point: None,
                                        });
                                    },
                                    Argument::List(args) => {
                                        args.iter().enumerate().for_each(|(param_idx, arg)| {
                                            nested_path.push(SingleFunctionOrArrayCall {
                                                function: None,
                                                param_idx,
                                            });
                                            variables(res, arg, nested_path);
                                            nested_path.pop();
                                        });
                                    }
                                    Argument::Function(function) => {
                                        function.parameters.iter().enumerate().for_each(
                                            |(param_idx, arg)| {
                                                nested_path.push(SingleFunctionOrArrayCall {
                                                    function: Some(function.name.clone()),
                                                    param_idx,
                                                });
                                                variables(res, arg, nested_path);
                                                nested_path.pop();
                                            },
                                        );
                                    }
                                }
                            }
                            variables(&mut unprocessed_params, arg, &mut SmallVec::new_const())
                        });

                        return Some((
                            function.head.name.text.clone(),
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

        for (function, unprocessed_params) in
            // TODO Replace with some kind of pop then push it to another global dict after it's finished
            functions.values_mut().flatten()
        {
            for caller_var in unprocessed_params.drain(..) {
                let domain = caller_var
                    .domain_all_of
                    .into_iter()
                    .map(|usage| match usage {
                        VariableUsage::ArithIs => VariableDomain::NUMBER.with(Rc::clone),
                        // VariableUsage::PatternMatchEq(res) => todo!(),
                        VariableUsage::NestedCall(nested) => nested
                            .iter()
                            .position(|first_nested| first_nested.function.is_some()) // If there is no function call (e.g. [X|Tail]), then it's "Any"
                            .map(|first_nested| {
                                let nested = unsafe { nested.get_unchecked(first_nested..) };
                                let first_nested = &unsafe {
                                    nested
                                        .get_unchecked(0)
                                        .function
                                        .as_ref()
                                        .unwrap_unchecked()
                                }
                                .text;
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
                                            let callee_ctx_res = match unsafe { callee.variables.get(&&**callee_var) } {
                                                Some(var) => var.domain.clone(),
                                                None => {
                                                    // TODO This var is still unprocessed...
                                                    todo!("something with {callees_unprocessed_params:?}")
                                                }
                                            };
                                            let caller_ctx_res = std::iter::chain(
                                                    callee.head.get_path_for(callee_var)
                                                        .filter_map(|path| function.head.get_param_at(path)),
                                                    (&callee_ctx_res.references_variables)
                                                        .into_iter()
                                                        .flat_map(|other_param_of_callee|
                                                            callee.head.get_path_for(other_param_of_callee)
                                                                .filter_map(|path| function.head.get_param_at(path))
                                                        )
                                                )
                                                .map(|caller_param| VariableDomain::from_argument(caller_param, |new_caller_var| {
                                                    // At this point, we already have a caller_var, so we don't need to convert it from a callee_var as before
                                                    if &**new_caller_var == &*caller_var.declaration {
                                                        // TODO does this make sense?
                                                        VariableDomain::ANY.with(Rc::clone)
                                                    } else {
                                                        let new_caller_var_domain = match unsafe { function.variables.get(&&**new_caller_var) } {
                                                            Some(var) => var.domain.clone(),
                                                            None => {
                                                                // TODO This var is still unprocessed...
                                                                todo!("something with {callees_unprocessed_params:?}")
                                                            }
                                                        };
                                                        let res = VariableDomain {
                                                            kind: new_caller_var_domain.kind.clone(),
                                                            references_variables: unsafe { SortedSmallSet::union_iters(
                                                                (&new_caller_var_domain.references_variables).into_iter().cloned(),
                                                                Some(new_caller_var.text.clone())
                                                            ) },
                                                        };
                                                        if res == *new_caller_var_domain {
                                                            new_caller_var_domain
                                                        } else {
                                                            Rc::new(res)
                                                        }
                                                    }
                                                }))
                                                .collect::<VariableDomainCollector<{ ReductionKind::Intersection }>>().0;
                                            VariableDomainCollector::<{ ReductionKind::Intersection }>::reduce(
                                                caller_ctx_res,
                                                VariableDomain {
                                                    kind: callee_ctx_res.kind.clone(),
                                                    references_variables: SortedSmallSet::empty(),
                                                },
                                            )
                                        })
                                    })
                                    .collect::<VariableDomainCollector<{ ReductionKind::Union }>>().0
                            })
                            .unwrap_or(VariableDomain::ANY.with(Rc::clone))
                    })
                    .collect::<VariableDomainCollector<{ ReductionKind::Intersection }>>().0;

                unsafe {
                    function
                        .variables
                        .entry(&caller_var.declaration)
                        .insert(Variable {
                            declaration: caller_var.declaration,
                            domain,
                            defined_starting_from_point: caller_var.defined_starting_from_point,
                        })
                };
            }
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
    fn get_path_for<'self_>(
        &'self_ self,
        var: &'self_ str,
    ) -> impl Iterator<Item = impl IntoIterator<Item = SingleFunctionOrArrayCall>> {
        // TODO Make it return -> impl for<'a> SelfAwareIterator<Item<'a> = impl Iterator<Item = SingleFunctionOrArrayCall>>

        type Path<'a> = Peekable<Enumerate<std::slice::Iter<'a, Argument>>>;
        struct Iter<'a> {
            nested_path: SmallVec<[Path<'a>; 16]>,
            var: &'a str,
        }
        impl<'a> Iterator for Iter<'a> {
            type Item = SmallVec<[SingleFunctionOrArrayCall; 16]>;

            fn next(&mut self) -> Option<Self::Item> {
                loop {
                    let path = 'path: loop {
                        let last = self.nested_path.last_mut()?;
                        if let Some((_, path)) = last.peek() {
                            break 'path path;
                        }
                        self.nested_path.pop();
                    };
                    match path {
                        Argument::Variable(var) if **var == *self.var => {
                            return Some(
                                self.nested_path
                                    .iter_mut()
                                    .map(|call| {
                                        let (param_idx, arg) =
                                            unsafe { call.peek().unwrap_unchecked() };
                                        SingleFunctionOrArrayCall {
                                            function: match arg {
                                                Argument::List(_) => None,
                                                Argument::Function(function) => {
                                                    Some(function.name.clone())
                                                }
                                                _ => unsafe { core::hint::unreachable_unchecked() },
                                            },
                                            param_idx: *param_idx,
                                        }
                                    })
                                    .collect(),
                            );
                        }
                        Argument::Function(new_args) => {
                            self.nested_path
                                .push(new_args.parameters.iter().enumerate().peekable());
                        }
                        Argument::List(new_args) => {
                            self.nested_path
                                .push(new_args.iter().enumerate().peekable());
                        }
                        _ => unsafe {
                            self.nested_path
                                .last_mut()
                                .unwrap_unchecked()
                                .next()
                                .unwrap_unchecked();
                        },
                    }
                }
            }
        }

        let mut nested_path = SmallVec::new_const();
        nested_path.push(self.parameters.iter().enumerate().peekable());
        Iter { nested_path, var }
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
        nested_call.try_fold(first, |parent, child| {
            let child = child.borrow();
            match parent {
                Argument::List(args) if child.borrow().function.is_none() => {
                    args.get(child.borrow().param_idx).map(Into::into)
                }
                Argument::Function(function) => match &child.borrow().function {
                    Some(function_name) if &*function.name == &**function_name => {
                        function.parameters.get(child.borrow().param_idx)
                    }
                    _ => None,
                },
                _ => None,
            }
        })
    }
}
#[derive(Debug)]
pub struct FunctionOrFact {
    pub head: FunctionHeadOrFact,
    pub variables: SortedSmallSet<Variable, 16, VarToName>,
}
struct VarToName;
impl SSSHandler<Variable> for VarToName {
    type Key = str;
    fn key(x: &Variable) -> &str {
        &x.declaration
    }
}
#[derive(Debug)]
pub enum Argument<Function: Deref<Target = FunctionHeadOrFact> = Box<FunctionHeadOrFact>> {
    Number(MiniNode),
    Atom(MiniNode),
    String(MiniNode),
    Variable(MiniNode),
    List(Vec<Argument>), // TODO SmallVec?
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
    domain: Rc<VariableDomain>, // TODO Check for cycles (e.g. test(X, Y):- X=Y, Y=X.)
    defined_starting_from_point: Option<Point>, // TODO Compute and Use
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct ComplexKind {
    /// * If `[]`, then this is not a simple kind (either an array or an ill-formed type, see [`Self::array_kinds`]).
    /// * If `[...]`, then any of those kinds are valid.
    pub simple_kinds: SortedSmallSet<VariableKind, 1>,

    /// * If `None`, then this is not an array.
    /// * If `Some((VariableDomain::~::IllFormed, true))`, then this is the "always empty" valid array kind.
    /// * If `Some((VariableDomain::~::IllFormed, false))`, then this is is an array of ill-formed types.
    /// * If `Some((VariableDomain::~::Valid(...), emptyable))`, then this is an array of those kinds, that may (not must) be empty if***f*** `emptyable`.
    pub array_kind: Option<(Rc<VariableDomain>, bool)>,
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct VariableDomain {
    /// * If `None`, then "Any" kind is matched by this domain.
    /// * If `Some(...)`, then that kind is matched by this domain.
    pub kind: Option<ComplexKind>,

    /// * If `[]`, then it is a free-variable.
    /// * If `[...]`, then it references all of those variables.
    pub references_variables: SortedSmallSet<Rc<String>, 2>,
}
#[derive(PartialEq, Eq, Clone, Copy, ConstParamTy)]
pub enum ReductionKind {
    Union,
    Intersection,
}
pub struct VariableDomainCollector<const RK: ReductionKind>(Rc<VariableDomain>);
impl<const RK: ReductionKind> VariableDomainCollector<RK> {
    pub fn reduce(
        a: impl Borrow<VariableDomain> + Into<Rc<VariableDomain>>,
        b: impl Borrow<VariableDomain> + Into<Rc<VariableDomain>>,
    ) -> Rc<VariableDomain> {
        let res = VariableDomain {
            kind: match RK {
                ReductionKind::Union => match (&a.borrow().kind, &b.borrow().kind) {
                    (Some(a), Some(b)) => Some(ComplexKind {
                        simple_kinds: unsafe {
                            SortedSmallSet::union_iters(
                                (&a.simple_kinds).into_iter().cloned(),
                                (&b.simple_kinds).into_iter().cloned(),
                            )
                        },
                        array_kind: match (&a.array_kind, &b.array_kind) {
                            (None, None) => None,
                            (None, Some((domain, emptyable)))
                            | (Some((domain, emptyable)), None) => {
                                Some((domain.clone(), *emptyable))
                            }
                            (Some((da, ea)), Some((db, eb))) => {
                                Some((Self::reduce(da.clone(), db.clone()), *ea || *eb))
                            }
                        },
                    }),
                    _ => None,
                },
                ReductionKind::Intersection => match (&a.borrow().kind, &b.borrow().kind) {
                    (None, None) => None,
                    (None, Some(x)) | (Some(x), None) => Some(x.clone()),
                    (Some(a), Some(b)) => Some(ComplexKind {
                        simple_kinds: unsafe {
                            SortedSmallSet::intersection_iters(
                                (&a.simple_kinds).into_iter().cloned(),
                                (&b.simple_kinds).into_iter().cloned(),
                            )
                        },
                        array_kind: match (&a.array_kind, &b.array_kind) {
                            (Some((da, ea)), Some((db, eb))) => {
                                Some((Self::reduce(da.clone(), db.clone()), *ea && *eb))
                            }
                            _ => None,
                        },
                    }),
                },
            },
            references_variables: {
                let a = (&a.borrow().references_variables).into_iter().cloned();
                let b = (&b.borrow().references_variables).into_iter().cloned();
                match RK {
                    ReductionKind::Union => unsafe { SortedSmallSet::intersection_iters(a, b) },
                    ReductionKind::Intersection => unsafe { SortedSmallSet::union_iters(a, b) },
                }
            },
        };
        if res == *a.borrow() {
            a.into()
        } else if res == *b.borrow() {
            b.into()
        } else {
            VariableDomain::try_static(res)
        }
    }
}
impl<const RK: ReductionKind> FromIterator<Rc<VariableDomain>> for VariableDomainCollector<RK> {
    fn from_iter<T: IntoIterator<Item = Rc<VariableDomain>>>(iter: T) -> Self {
        // TODO rn, some things don't make sense
        // * e.g.
        //   test(1,1). test(a,a).
        //   if example(test(X,Y)). then both X and Y would be FreeVars
        // * e.g.
        //   test(In,Out,true):-Out=In.
        //   test(Out,In,false):-Out=In.
        //   if example(X,Y,Cond). then both X and Y would be FreeVars
        Self(iter.into_iter().reduce(Self::reduce).unwrap_or_else(|| {
            match RK {
                // TODO Is this fine, or should it be swapped. check users
                ReductionKind::Union => &VariableDomain::INVALID,
                ReductionKind::Intersection => &VariableDomain::ANY,
            }
            .with(Rc::clone)
        }))
    }
}
impl VariableDomain {
    thread_local! {
        static ANY: Rc<VariableDomain> = Rc::new(const {
            VariableDomain {
                kind: None,
                references_variables: SortedSmallSet::empty(),
            }
        });
        static INVALID: Rc<VariableDomain> = Rc::new(const {
            VariableDomain {
                kind: Some(ComplexKind {
                    simple_kinds: SortedSmallSet::empty(),
                    array_kind: None,
                }),
                references_variables: SortedSmallSet::empty(),
            }
        });
        static NUMBER: Rc<VariableDomain> = Rc::new(VariableDomain {
            kind: Some(ComplexKind { simple_kinds: SortedSmallSet::single(VariableKind::Number), array_kind: None }),
            references_variables: SortedSmallSet::empty(),
        });
        static LIST_ANY_EMPTYABLE: Rc<VariableDomain> = Rc::new(VariableDomain {
            kind: Some(ComplexKind { simple_kinds: SortedSmallSet::empty(), array_kind: Some((VariableDomain::ANY.with(Rc::clone), true)) }),
            references_variables: SortedSmallSet::empty(),
        });
        static LIST_ANY_NON_EMPTY: Rc<VariableDomain> = Rc::new(VariableDomain {
            kind: Some(ComplexKind { simple_kinds: SortedSmallSet::empty(), array_kind: Some((VariableDomain::ANY.with(Rc::clone), false)) }),
            references_variables: SortedSmallSet::empty(),
        });
        static LIST_ALWAYS_EMPTY: Rc<VariableDomain> = Rc::new(VariableDomain {
            kind: Some(ComplexKind { simple_kinds: SortedSmallSet::empty(), array_kind: Some((VariableDomain::INVALID.with(Rc::clone), true)) }),
            references_variables: SortedSmallSet::empty(),
        });
        static LIST_INVALID: Rc<VariableDomain> = Rc::new(VariableDomain {
            kind: Some(ComplexKind { simple_kinds: SortedSmallSet::empty(), array_kind: Some((VariableDomain::INVALID.with(Rc::clone), false)) }),
            references_variables: SortedSmallSet::empty(),
        });
    }

    fn try_static(
        item: impl Into<Rc<VariableDomain>> + Borrow<VariableDomain>,
    ) -> Rc<VariableDomain> {
        [
            &VariableDomain::ANY,
            &VariableDomain::INVALID,
            &VariableDomain::NUMBER,
            &VariableDomain::LIST_ANY_EMPTYABLE,
            &VariableDomain::LIST_ANY_NON_EMPTY,
            &VariableDomain::LIST_ALWAYS_EMPTY,
            &VariableDomain::LIST_INVALID,
        ]
        .into_iter()
        .find_map(|x| x.with(|x| (item.borrow() == &**x).then(|| x.clone())))
        .unwrap_or_else(|| item.into())
    }

    pub fn is_ill_formed(&self) -> bool {
        matches!(&self.kind, Some(ComplexKind{ simple_kinds: x, array_kind: None }) if x.is_empty())
    }

    fn from_argument(
        arg: &Argument,
        mut parameter_resolver: impl FnMut(&MiniNode) -> Rc<Self>,
    ) -> Rc<Self> {
        if let Argument::Variable(var) = arg {
            Self::try_static(parameter_resolver(var))
        } else {
            let res = Self {
                kind: Some(ComplexKind {
                    simple_kinds: match arg {
                        Argument::Number(_) => Some(VariableKind::Number),
                        Argument::Atom(atom) => Some(VariableKind::Atom(atom.text.clone())),
                        Argument::String(str) => Some(VariableKind::String(str.text.clone())),
                        Argument::Function(function) => {
                            Some(VariableKind::Function(function.name.text.clone()))
                        }
                        _ => None,
                    }
                    .map(SortedSmallSet::single)
                    .unwrap_or(SortedSmallSet::empty()),
                    array_kind: match arg {
                        Argument::List(args) => Some((
                            args.into_iter()
                                .map(|x| Self::from_argument(x, &mut parameter_resolver))
                                .collect::<VariableDomainCollector<{ ReductionKind::Union }>>()
                                .0,
                            args.is_empty(),
                        )),
                        _ => None,
                    },
                }),
                references_variables: match arg {
                    Argument::Variable(var) => SortedSmallSet::single(var.text.clone()),
                    _ => SortedSmallSet::empty(),
                },
            };
            Self::try_static(res)
        }
    }
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub enum VariableKind {
    Number,
    Atom(Rc<String>),
    String(Rc<String>),
    Function(Rc<String>),
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
        /// Non empty // TODO Is it tho
        SmallVec<[SingleFunctionOrArrayCall; 6]>,
    ),

    // TODO Document that at these Eq and Is MUST **NOT** be any kind of Eq/Is, just those that act as operators, e.g. [X|Tail] = [123], but not when they act as structs, e.g. hello_world(X = 123)
    // PatternMatchEq(XXX), // TODO Construct
    ArithIs, // TODO Construct
}
#[derive(Debug, Clone)]
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
