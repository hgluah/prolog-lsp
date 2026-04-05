use std::borrow::{Borrow, Cow};

use lsp_types::{
    DocumentSymbol, DocumentSymbolResponse, Range, SemanticToken, SemanticTokenModifier,
    SemanticTokenType, SemanticTokens, SemanticTokensLegend, SemanticTokensRangeResult, SymbolKind,
};

use crate::lsp::document::{Argument, Document};

/// Cow without ToOwned
enum Ref<'a, T> {
    Ref(&'a T),
    Own(T),
}
impl<'a, T> Borrow<T> for Ref<'a, T> {
    fn borrow(&self) -> &T {
        match self {
            Ref::Ref(x) => x,
            Ref::Own(x) => x,
        }
    }
}

pub fn document_symbols(document: &Document) -> anyhow::Result<Option<DocumentSymbolResponse>> {
    let functions = &document.functions_and_facts;
    if functions.is_empty() {
        return Ok(None);
    }

    let document_symbols = functions
        .into_iter()
        .map(|function| DocumentSymbol {
            name: (*function.head.name.text).clone(),
            kind: SymbolKind::FUNCTION,
            range: function.position_including_body.into(),
            selection_range: function.head.name.position,
            children: document_symbol_children(std::iter::chain(
                function
                    .head
                    .parameters
                    .iter()
                    .map(Borrow::borrow)
                    .map(Ref::Ref),
                (&function.body_variables)
                    .into_iter()
                    .cloned()
                    .map(Argument::Variable)
                    .map(Ref::Own),
            )),
            detail: None,
            tags: None,
            #[allow(deprecated)]
            deprecated: None,
        })
        .collect();

    Ok(Some(DocumentSymbolResponse::Nested(document_symbols)))
}

fn document_symbol_children(
    args: impl IntoIterator<Item = impl Borrow<Argument>>,
) -> Option<Vec<DocumentSymbol>> {
    fn name(arg: &impl Borrow<Argument>) -> Cow<'_, str> {
        match arg.borrow() {
            Argument::Number(name)
            | Argument::Atom(name)
            | Argument::String(name)
            | Argument::Variable(name) => Cow::Borrowed(&name),
            Argument::Function(function) => Cow::Borrowed(&function.name),
            Argument::List(args, _) => Cow::Owned(
                args.iter()
                    .map(name)
                    .fold("[".to_owned(), |a, b| a + ", " + &b)
                    + "]",
            ),
        }
    }
    let res = args
        .into_iter()
        .map(|arg| {
            let arg = arg.borrow();
            DocumentSymbol {
                name: name(arg).into_owned(),
                kind: match arg {
                    Argument::Number(_) => SymbolKind::NUMBER,
                    Argument::Atom(_) => SymbolKind::CONSTANT,
                    Argument::String(_) => SymbolKind::STRING,
                    Argument::Variable(_) => SymbolKind::VARIABLE,
                    Argument::List(_, _) => SymbolKind::ARRAY,
                    Argument::Function(_) => SymbolKind::FUNCTION,
                },
                range: match arg {
                    Argument::Number(name)
                    | Argument::Atom(name)
                    | Argument::String(name)
                    | Argument::Variable(name) => name.position,
                    Argument::List(_, position) => *position,
                    Argument::Function(function) => function.position_including_params.into(),
                },
                selection_range: match arg {
                    Argument::Number(name)
                    | Argument::Atom(name)
                    | Argument::String(name)
                    | Argument::Variable(name) => name.position,
                    Argument::List(_, position) => *position,
                    Argument::Function(function) => function.name.position,
                },
                children: match arg {
                    Argument::Number(_)
                    | Argument::Atom(_)
                    | Argument::String(_)
                    | Argument::Variable(_) => None,
                    Argument::List(args, _) => {
                        document_symbol_children(args.iter().map(Borrow::borrow))
                    }
                    Argument::Function(function) => {
                        document_symbol_children(function.parameters.iter().map(Borrow::borrow))
                    }
                },
                detail: None,
                tags: None,
                #[allow(deprecated)]
                deprecated: None,
            }
        })
        .collect::<Vec<_>>();
    (!res.is_empty()).then_some(res)
}

macro_rules! sth {
    (bits = [$($bit_ty:ident),* $(,)?] types = [$($fun:ident $(($($bit_id:ident = $bit:literal),+))? = $ty:ident),+ $(,)?]) => {
        pub struct SemanticTokenHandler;
        impl SemanticTokenHandler {
            pub fn legend() -> SemanticTokensLegend {
                let token_types = const {
                    [
                        $(SemanticTokenType::$ty),+
                    ]
                };
                let token_modifiers = const {
                    let token_modifiers = [
                        $(SemanticTokenModifier::$bit_ty),*
                    ];
                    $($($(
                        assert!($bit >= 0 && $bit < token_modifiers.len(), stringify!(Wrong <<$bit_id>>));
                    )+)?)+
                    token_modifiers
                };
                SemanticTokensLegend {
                    token_types: token_types.into(),
                    token_modifiers: token_modifiers.into(),
                }
            }

            sth!(0, $($fun $(($($bit_id = $bit),+))?,)+);
        }
    };
    ($num:expr, ) => {};
    ($num:expr, $fun:ident $(($($bit_id:ident = $bit:literal),+))?, $($rest:tt)*) => {
        paste::paste!{
            pub const fn [<st_ $fun>](delta_line: u32, delta_start: u32, length: u32 $(, $($bit_id: bool),+)?) -> SemanticToken {
                SemanticToken {
                    delta_line,
                    delta_start,
                    length,
                    token_type: $num,
                    token_modifiers_bitset: 0 $($(| if $bit_id { 1 << $bit } else { 0 })+)?,
                }
            }
        }
        sth!($num + 1, $($rest)*);
    };
}
sth! {
    bits = [
        READONLY,        // 0
        DEFAULT_LIBRARY, // 1 // TODO Use
    ]
    types = [
        // TODO Use all of these
        param (is_read_only = 0) = PARAMETER,
        var     = VARIABLE,

        func (is_from_std = 1) = FUNCTION,
        struct  = STRUCT, // e.g. rng(**rng_data**(...))

        str     = STRING,
        num     = NUMBER,

        comment = COMMENT,
        op      = OPERATOR,
        keyword = KEYWORD,

        namespace  = NAMESPACE, // TODO Maybe imports/exports?
    ]
}

pub fn semantic_tokens(
    range: Range,
    document: &Document,
) -> anyhow::Result<Option<SemanticTokensRangeResult>> {
    let semantic_tokens = Vec::new();

    if semantic_tokens.is_empty() {
        return Ok(None);
    }

    Ok(Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
        result_id: None,
        data: semantic_tokens,
    })))
}
