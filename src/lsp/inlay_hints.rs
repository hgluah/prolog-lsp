use std::borrow::Cow;

use either::Either;
use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, InlayHintLabelPart, Range};
use smallvec::{SmallVec, smallvec};

use crate::lsp::document::{Document, VariableDomain, VariableKind};

pub fn inlay_hints(range: Range, document: &Document) -> anyhow::Result<Option<Vec<InlayHint>>> {
    let inlay_hints = (&document.functions_and_facts)
        .into_iter()
        .filter(|function| {
            range.end >= function.position_including_body.start
                && range.start <= function.position_including_body.end
        })
        .flat_map(|function| {
            (&function.variables)
                .into_iter()
                .flat_map(|var| {
                    function
                        .head
                        .get_path_for(&var.declaration)
                        .map(|(range, _)| (var.domain.clone(), range))
                })
                .map(|(domain, range)| InlayHint {
                    position: range.end,
                    label: match domain_to_label(&domain) {
                        Either::Left(str) => InlayHintLabel::String(format!(": {str}")),
                        Either::Right(mut parts) => InlayHintLabel::LabelParts(
                            {
                                parts.insert(
                                    0,
                                    InlayHintLabelPart {
                                        value: ": ".to_owned(),
                                        tooltip: None,
                                        location: None,
                                        command: None,
                                    },
                                );
                                parts
                            }
                            .into_vec(),
                        ),
                    },
                    kind: Some(InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: Some(false),
                    padding_right: Some(false),
                    data: None,
                })
        })
        .collect::<Vec<_>>();

    Ok((!inlay_hints.is_empty()).then_some(inlay_hints))
}

fn domain_to_label(
    domain: &VariableDomain,
) -> Either<Cow<'_, str>, SmallVec<[InlayHintLabelPart; 4]>> {
    fn kind_to_label(
        kind: &VariableKind,
    ) -> Either<Cow<'_, str>, SmallVec<[InlayHintLabelPart; 4]>> {
        match kind {
            VariableKind::Number => Either::Left(Cow::Borrowed("number")),
            VariableKind::Atom(name) => Either::Left(Cow::Borrowed(name)),
            VariableKind::String(str) => Either::Left(Cow::Owned(format!(r#""{str}""#))),
            VariableKind::Function(function) => Either::Right(smallvec![InlayHintLabelPart {
                value: format!("{function}(..)"),
                tooltip: None,
                location: {
                    // all_functions.find_location_of(function) // TODO
                    None
                },
                command: None,
            }]),
        }
    }
    match &domain.kind {
        None => Either::Left(Cow::Borrowed("any")),
        Some(kind) if kind.simple_kinds.is_empty() && kind.array_kind.is_none() => {
            Either::Left(Cow::Borrowed("!"))
        }
        Some(kind) => std::iter::chain(
            (&kind.simple_kinds)
                .into_iter()
                .map(kind_to_label)
                .map(std::iter::once)
                .map(Either::Left),
            kind.array_kind.as_ref().map(|(domain, emptyable)| {
                if domain.is_ill_formed() {
                    Either::Left(std::iter::once(Either::Left(Cow::Borrowed(
                        if *emptyable { "[]" } else { "![]" },
                    ))))
                } else {
                    Either::Right(
                        [
                            Either::Left(Cow::Borrowed(if domain.is_not_complex_type() {
                                ""
                            } else {
                                "("
                            })),
                            domain_to_label(&domain),
                            Either::Left(Cow::Borrowed(
                                match (domain.is_not_complex_type(), *emptyable) {
                                    (false, true) => ")[0..]",
                                    (true, true) => "[0..]",
                                    (false, false) => ")[1..]",
                                    (true, false) => "[1..]",
                                },
                            )),
                        ]
                        .into_iter(),
                    )
                }
            }),
        )
        .intersperse(Either::Left(std::iter::once(Either::Left(Cow::Borrowed(
            " | ",
        )))))
        .flatten()
        .fold(Either::Left(Cow::Borrowed("")), |a, b| match (a, b) {
            (Either::Left(a), Either::Left(b)) => Either::Left(Cow::Owned(a.into_owned() + &b)),
            (Either::Left(a), Either::Right(mut b)) => {
                b.insert(
                    0,
                    InlayHintLabelPart {
                        value: a.into_owned(),
                        tooltip: None,
                        location: None,
                        command: None,
                    },
                );
                Either::Right(b)
            }
            (Either::Right(mut a), Either::Left(b)) => {
                a.push(InlayHintLabelPart {
                    value: b.into_owned(),
                    tooltip: None,
                    location: None,
                    command: None,
                });
                Either::Right(a)
            }
            (Either::Right(mut a), Either::Right(mut b)) => {
                a.append(&mut b);
                Either::Right(a)
            }
        }),
    }
}
