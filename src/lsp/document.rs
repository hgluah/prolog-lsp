use std::{
    borrow::Borrow,
    fmt::{self, Write},
    iter::{Enumerate, Peekable},
    marker::ConstParamTy,
    ops::Deref,
    rc::Rc,
};

use itertools::Itertools;
use lsp_types::{Range, Uri};
use rustc_hash::FxHashMap;
use smallvec::{SmallVec, smallvec};
use tracing::info;
use tree_sitter::{Node, Parser, Point, QueryCursor, StreamingIterator, Tree};

use texter::{change::GridIndex, core::text::Text};

use crate::{
    lsp::queries::{Ancestors, clauses, module},
    util::{
        formatting::NoAlternate,
        sorted_small_set::{SortedSmallSet, SortedSmallVec, sss_handler, ssv_handler},
    },
};

use super::queries::CLAUSES;

pub type Documents = FxHashMap<Uri, Document>;
impl Document {
    pub fn new(tree: Tree, text: Text, parser: &mut Parser) -> anyhow::Result<Self> {
        let mut res = Self {
            tree,
            text: text.into(),
            imports: SmallVec::new(),
            exports: SmallVec::new(),
            functions_and_facts: SortedSmallVec::empty(),
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
                module_name: MiniNode::new(module_name, &self.text.text)?.into(),
                exported: exported
                    .into_iter()
                    .map(|(function, arity)| {
                        Ok((
                            MiniNode::new(function, &self.text.text)?.into(),
                            MiniNode::new(arity, &self.text.text)?.into(),
                        ))
                    })
                    .collect::<Result<_, std::str::Utf8Error>>()?,
            }))
        })
        .collect::<Result<_, std::str::Utf8Error>>()?;

        let mut cursor = QueryCursor::new();
        let mut unprocessed_functions = clauses(
            &mut cursor,
            self.tree.root_node(),
            self.text.text.as_bytes(),
        )
        .map_deref(|&x| x)
        .map(|(kind, node)| {
            'err: {
                let op_check = |node: Node| {
                    node.child_by_field_name("operator")
                        .unwrap()
                        .utf8_text(&self.text.text.as_bytes())
                        != Ok(":-")
                        || node.child_count() != 3
                };
                let function = match kind {
                    CLAUSES::Atom | CLAUSES::Function => {
                        if Ancestors(node).any(|parent| match parent.kind() {
                            "functional_notation" => true,
                            "operator_notation" => op_check(parent),
                            _ => false,
                        }) {
                            return Ok(None);
                        }
                        if let CLAUSES::Atom = kind {
                            let name = MiniNode::new(node, &self.text.text)?;
                            let item = FunctionOrFact {
                                position_including_body: name.position.into(),
                                head: FunctionHeadOrFact {
                                    position_including_params: name.position.into(),
                                    name: name.into(),
                                    parameters: SmallVec::new_const(),
                                },
                                variables: SortedSmallSet::empty(),
                            };
                            unsafe { self.functions_and_facts.push(item) };
                            return Ok(None);
                        }
                        node
                    }
                    CLAUSES::Op => {
                        if op_check(node) {
                            break 'err;
                        }
                        node.child(0).unwrap()
                    }
                };

                let function = FunctionOrFact {
                    head: Self::parse_funtion_head(function, &self.text.text)
                        .unwrap_or_else(|_| todo!() /* TODO */),
                    position_including_body: MiniNode::pos(node).into(),
                    variables: SortedSmallSet::empty(),
                };
                sss_handler!(
                    <()>
                    UnprocessedVariablesGroupper<UnprocessedVariable> Key = str;
                    |x| &x.declaration,
                    |old, mut new| {
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
                );
                fn variables(
                    res: &mut SortedSmallSet<
                        UnprocessedVariable,
                        4,
                        UnprocessedVariablesGroupper,
                    >,
                    arg: &Argument<impl Deref<Target = FunctionHeadOrFact>>,
                    nested_path: &mut SmallVec<[SingleFunctionOrArrayCall; 6]>,
                ) {
                    match arg {
                        Argument::Number(_) | Argument::Atom(_) | Argument::String(_) => (),
                        Argument::Variable(var) => unsafe {
                            res.push(UnprocessedVariable {
                                declaration: var.clone(),
                                domain_all_of: smallvec![VariableUsage::NestedCall(
                                    nested_path.clone()
                                )],
                                defined_starting_from_point: None,
                            });
                        },
                        Argument::List(args, _) => {
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
                                        function: Some(function.name.clone().into()),
                                        param_idx,
                                    });
                                    variables(res, arg, nested_path);
                                    nested_path.pop();
                                },
                            );
                        }
                    }
                }
                let mut unprocessed_params = SortedSmallSet::empty();
                variables(&mut unprocessed_params, &Argument::Function(&function.head), &mut SmallVec::new_const());

                return Ok(
                    if unprocessed_params.is_empty() {
                        unsafe { self.functions_and_facts.push(function) };
                        None
                    } else {
                        Some((function.head.name.text.clone(), (function, unprocessed_params)))
                    }
                );
            }
            todo!() // TODO
        })
        .filter_map_ok(std::convert::identity)
        .map_ok(|(k, v)| (k, smallvec![v]))
        .collect::<anyhow::Result<SortedSmallSet<(_, SmallVec<[_; 4]>), 4, UnprocessedFunctionsHandler>>>()?;
        sss_handler!(
            <(T, const N: usize)>
            UnprocessedFunctionsHandler<(Rc<String>, SmallVec<[T; N]>)> Key = str;
            |x| &x.0,
            |old, mut new| {
                debug_assert_eq!(old.0, new.0);
                old.1.append(&mut new.1);
            }
        );

        while let Some((mut function, mut unprocessed_params)) = 'next_to_be_processed: {
            while let Some((_, v)) = unsafe { unprocessed_functions.as_mut_slice() }.last_mut() {
                if let Some(res) = v.pop() {
                    break 'next_to_be_processed Some(res);
                }
                unsafe { unprocessed_functions.pop().unwrap_unchecked() };
            }
            None
        } {
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
                                    std::iter::chain(
                                        unsafe { self.functions_and_facts.get(first_nested) }.iter().map(|function| (function, const { &SortedSmallSet::empty() })),
                                        // TODO sure, let's add the first one, but we also need to add all the interemediates THAT ARE NOT INCLUDED BEFORE:
                                        //  e.g. example(test(X, Y)) should check other example's, fine, but also the requirements of test THAT ARE NOT INCLUDED IN THE PREVIOUS example'S
                                        (&unsafe { unprocessed_functions.get(first_nested).expect("TODO") }.1).into_iter().map(|(a, b)| (a, b))
                                    ),
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
                                let domain_any_of = domain_any_of
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
                                                        .filter_map(|(_, path)| function.head.get_param_at(path)),
                                                    (&callee_ctx_res.references_variables)
                                                        .into_iter()
                                                        .flat_map(|other_param_of_callee|
                                                            callee.head.get_path_for(other_param_of_callee)
                                                                .filter_map(|(_, path)| function.head.get_param_at(path))
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
                                    });
                                domain_any_of
                                    .collect::<VariableDomainCollector<{ ReductionKind::Union }>>().0
                            })
                            .unwrap_or(VariableDomain::ANY.with(Rc::clone))
                    })
                    .collect::<VariableDomainCollector<{ ReductionKind::Intersection }>>().0;

                unsafe {
                    function.variables.push(Variable {
                        declaration: caller_var.declaration.text,
                        domain,
                        defined_starting_from_point: caller_var.defined_starting_from_point,
                    })
                };
            }
            unsafe { self.functions_and_facts.push(function) };
        }

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
            "list_notation" => List(
                {
                    let mut cursor = node.walk();
                    node.children(&mut cursor)
                        .skip(1)
                        .step_by(2)
                        .map(|child| Self::parse_arg(child, text.as_ref()).map(Into::into))
                        .collect::<Result<_, _>>()?
                },
                MiniNode::pos(node),
            ),
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
                .unwrap_or_else(|_| todo!() /* TODO */)
                .into(),
            position_including_params: MiniNode::pos(function).into(),
            parameters: {
                let mut res = SmallVec::new();
                for arg in args.children(&mut cursor).step_by(2) {
                    res.push(Self::parse_arg(arg, text.as_ref())?.into());
                }
                res
            },
        })
    }
}
#[derive(Debug)]
pub struct Document {
    pub tree: Tree,
    pub text: NoAlternate<Text>,
    pub imports: SmallVec<[MiniNode; 16]>, // TODO Construct
    pub exports: SmallVec<[Result<Exports, MiniNode>; 1]>,
    pub functions_and_facts: SortedSmallVec<FunctionOrFact, 32, FuncToName>,
}
ssv_handler!(<()> pub FuncToName<FunctionOrFact> Key = str; |x| &x.head.name);
#[derive(Debug)]
pub struct Exports {
    pub module_name: NoAlternate<MiniNode>,
    pub exported: SmallVec<[(NoAlternate<MiniNode>, NoAlternate<MiniNode>); 32]>,
}
#[derive(Debug)]
pub struct FunctionHeadOrFact {
    pub name: NoAlternate<MiniNode>,
    pub position_including_params: NoAlternate<Range>,
    pub parameters: SmallVec<[NoAlternate<Argument>; 8]>,
}
impl FunctionHeadOrFact {
    pub fn get_path_for<'self_>(
        &'self_ self,
        var: &'self_ str,
    ) -> impl Iterator<Item = (Range, impl IntoIterator<Item = SingleFunctionOrArrayCall>)> {
        // TODO Make it return -> impl for<'a> SelfAwareIterator<Item<'a> = impl Iterator<Item = SingleFunctionOrArrayCall>>

        type Path<'a> = Peekable<Enumerate<std::slice::Iter<'a, NoAlternate<Argument>>>>;
        struct Iter<'a> {
            nested_path: SmallVec<[Path<'a>; 16]>,
            var: &'a str,
        }
        impl<'a> Iterator for Iter<'a> {
            type Item = (Range, SmallVec<[SingleFunctionOrArrayCall; 16]>);

            fn next(&mut self) -> Option<Self::Item> {
                loop {
                    let path = 'path: loop {
                        if let Some((_, path)) = self.nested_path.last_mut()?.peek() {
                            break 'path path;
                        }
                        self.nested_path.pop();
                        self.nested_path.last_mut()?.next(); // Skip the array/function that pushed the new scope
                    };
                    let (_, path) = match &***path {
                        Argument::Function(new_args) => {
                            self.nested_path
                                .push(new_args.parameters.iter().enumerate().peekable());
                            continue;
                        }
                        Argument::List(new_args, _) => {
                            self.nested_path
                                .push(new_args.iter().enumerate().peekable());
                            continue;
                        }
                        _ => unsafe {
                            self.nested_path
                                .last_mut()
                                .unwrap_unchecked()
                                .next()
                                .unwrap_unchecked()
                        },
                    };
                    if let Argument::Variable(var) = &**path
                        && **var == *self.var
                    {
                        let len = self.nested_path.len();
                        return Some((
                            var.position,
                            (&mut self.nested_path[..len - 1])
                                .iter_mut()
                                .map(|call| {
                                    let (param_idx, arg) =
                                        unsafe { call.peek().unwrap_unchecked() };
                                    SingleFunctionOrArrayCall {
                                        function: match &***arg {
                                            Argument::List(_, _) => None,
                                            Argument::Function(function) => {
                                                Some(function.name.clone().into())
                                            }
                                            _ => unsafe { core::hint::unreachable_unchecked() },
                                        },
                                        param_idx: *param_idx,
                                    }
                                })
                                .collect(),
                        ));
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
        let first = &*self.parameters[first.borrow().param_idx];
        nested_call.try_fold(first, |parent, child| {
            let child = child.borrow();
            match parent {
                Argument::List(args, _) if child.borrow().function.is_none() => {
                    args.get(child.borrow().param_idx).map(Deref::deref)
                }
                Argument::Function(function) => match &child.borrow().function {
                    Some(function_name) if &**function.name == &**function_name => function
                        .parameters
                        .get(child.borrow().param_idx)
                        .map(Deref::deref),
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
    pub position_including_body: NoAlternate<Range>,
    pub variables: SortedSmallSet<Variable, 16, VarToName>,
}
sss_handler!(<()> pub VarToName<Variable> Key = str; |x| &x.declaration);
#[derive(Debug)]
pub enum Argument<Function: Deref<Target = FunctionHeadOrFact> = Box<FunctionHeadOrFact>> {
    Number(MiniNode),
    Atom(MiniNode),
    String(MiniNode),
    Variable(MiniNode),
    List(Vec<NoAlternate<Argument>>, Range), // TODO SmallVec?
    Function(Function),
}
impl<Function: Deref<Target = FunctionHeadOrFact>> fmt::Display for Argument<Function> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(node) | Self::Atom(node) | Self::String(node) | Self::Variable(node) => {
                f.write_str(node)
            }
            Self::List(args, _) => {
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
    pub declaration: Rc<String>,
    pub domain: Rc<VariableDomain>, // TODO Check for cycles (e.g. test(X, Y):- X=Y, Y=X.)
    pub defined_starting_from_point: Option<Point>, // TODO Compute and Use
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
    pub fn is_not_complex_type(&self) -> bool {
        match &self.kind {
            Some(ComplexKind {
                simple_kinds,
                array_kind: None,
            }) => simple_kinds.len() <= 1,
            Some(ComplexKind {
                simple_kinds,
                array_kind: Some(_),
            }) => simple_kinds.is_empty(),
            _ => true,
        }
    }

    fn from_argument(
        arg: &Argument,
        mut parameter_resolver: impl FnMut(&MiniNode) -> Rc<Self>,
    ) -> Rc<Self> {
        fn inner(
            arg: &Argument,
            parameter_resolver: &mut impl FnMut(&MiniNode) -> Rc<VariableDomain>,
        ) -> Rc<VariableDomain> {
            if let Argument::Variable(var) = arg {
                VariableDomain::try_static(parameter_resolver(var))
            } else {
                let res = VariableDomain {
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
                            Argument::List(args, _) => Some((
                                args.into_iter()
                                    .map(|x| inner(x, parameter_resolver))
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
                VariableDomain::try_static(res)
            }
        }
        inner(arg, &mut parameter_resolver)
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
pub struct SingleFunctionOrArrayCall {
    function: Option<MiniNode>, // TODO We should add the possibility of "function" being "=" or "is". They WOULD **NOT** imply anoything other than the output_type=input_type and output_type=num respectively, since at this point they don't act as actual operators, just as structs with fancy names
    param_idx: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MiniNode {
    pub position: Range,
    pub text: Rc<String>,
}
impl Ord for MiniNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.text
            .cmp(&other.text)
            .then(self.position.start.cmp(&other.position.start))
    }
}
impl PartialOrd for MiniNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
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
