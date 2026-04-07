mod completions;
mod diagnostics;
mod document;
mod inlay_hints;
pub mod queries;
mod references;
mod symbols;

use std::collections::hash_map::Entry;

use either::Either;
pub use symbols::SemanticTokenHandler;

use anyhow::Context;
use lsp_server::{Connection, ErrorCode, Message, RequestId, Response};
use lsp_types::{
    MessageType, ShowMessageParams, TextDocumentIdentifier, Uri,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
        Notification, PublishDiagnostics, ShowMessage,
    },
    request::{
        Completion, DocumentSymbolRequest, InlayHintRequest, References, Request,
        SemanticTokensRangeRequest,
    },
};
use tracing::{error, warn};
use tree_sitter::Parser;

use crate::{
    init::TextFn,
    lsp::{
        completions::completions,
        diagnostics::diagnostics,
        document::{Document, Documents},
        inlay_hints::inlay_hints,
        references::references,
        symbols::{document_symbols, semantic_tokens},
    },
    util::partition_iter::PartitionIter,
};
use texter::change::{Change, GridIndex};

pub fn main_loop(text_fn: TextFn, con: Connection) -> anyhow::Result<()> {
    let mut parser = Parser::new();
    parser.set_language(&prolog_grammar::LANGUAGE.into())?;

    let mut docs = Documents::default();

    let mut receiver = PartitionIter::new(con.receiver, |x| match x {
        x @ Message::Request(_) | x @ Message::Notification(_) => Either::Left(x),
        Message::Response(response) => Either::Right(response),
    });
    while let Some(msg) = receiver.next_a() {
        let get_doc: impl for<'a> FnOnce(
            &'a mut Documents,
            Uri,
            &mut Parser,
            Option<String>,
        ) -> anyhow::Result<(bool, &'a mut Document)> = |docs, uri, parser, text| {
            let entry = docs.entry(uri.clone());
            let res = match entry {
                Entry::Occupied(entry) => (false, entry.into_mut()),
                Entry::Vacant(entry) => {
                    let text = match text {
                        Some(text) => text,
                        None => {
                            con.sender.send(Message::Request(lsp_server::Request::new(
                                RequestId::from(69),
                                "custom/getContentsOfDoc".to_owned(),
                                TextDocumentIdentifier::new(uri),
                            )))?;
                            receiver
                                .next_b()
                                .and_then(|res| res.result)
                                .and_then(|x| match x {
                                    serde_json::Value::String(s) => Some(s),
                                    _ => None,
                                })
                                .context("Expected the document contents to be returned")?
                        }
                    };
                    let tree = parser
                        .parse(text.as_bytes(), None)
                        .context("Tree not returned during parsing")?;
                    (
                        true,
                        entry.insert(Document::new(tree, text_fn(text), parser)?),
                    )
                }
            };
            Ok(res)
        };
        let req_id = match &msg {
            Message::Request(lsp_server::Request { id, .. }) => Some(id.clone()),
            _ => None,
        };
        let handle_msg = || {
            anyhow::Result::<()>::Ok(match msg {
                Message::Notification(noti) => {
                    if let Some(iter) = handle_notification(&mut docs, &mut parser, get_doc, noti)?
                    {
                        iter.map(Message::Notification)
                            .try_for_each(|response| con.sender.send(response))?;
                    }
                }
                Message::Request(req) => {
                    let response =
                        Message::Response(handle_request(&mut docs, &mut parser, get_doc, req)?);
                    con.sender.send(response)?;
                }
                Message::Response(_) => unreachable!(),
            })
        };
        if let Err(err) = handle_msg() {
            if let Some(req_id) = req_id {
                con.sender.send(Message::Response(Response::new_err(
                    req_id,
                    ErrorCode::InternalError as _,
                    err.to_string(),
                )))?;
            } else {
                let err = err.into_boxed_dyn_error();
                error!(err);
                con.sender
                    .send(Message::Notification(lsp_server::Notification::new(
                        <ShowMessage as Notification>::METHOD.to_owned(),
                        ShowMessageParams {
                            typ: MessageType::ERROR,
                            message: err.to_string(),
                        },
                    )))?;
            }
        }
    }

    Ok(())
}

fn handle_notification(
    docs: &mut Documents,
    parser: &mut Parser,
    get_doc: impl for<'a> FnOnce(
        &'a mut Documents,
        Uri,
        &mut Parser,
        Option<String>,
    ) -> anyhow::Result<(bool, &'a mut Document)>,
    noti: lsp_server::Notification,
) -> anyhow::Result<Option<impl Iterator<Item = lsp_server::Notification>>> {
    let publish_diagnostics = |res: <PublishDiagnostics as Notification>::Params| {
        lsp_server::Notification::new(PublishDiagnostics::METHOD.to_owned(), res)
    };

    Ok(match &*noti.method {
        DidChangeTextDocument::METHOD => {
            let p: <DidChangeTextDocument as Notification>::Params =
                serde_json::from_value(noti.params)?;
            let (newly_inserted, document) = get_doc(docs, p.text_document.uri, parser, None)?;
            if newly_inserted {
                warn!("Changed unknown document.");
            } else {
                for ch in p.content_changes.into_iter() {
                    document.text.update(Change::from(ch), &mut document.tree)?;
                }
            }
            None
        }
        DidSaveTextDocument::METHOD => {
            let p: <DidSaveTextDocument as Notification>::Params =
                serde_json::from_value(noti.params)?;
            let (newly_inserted, document) =
                get_doc(docs, p.text_document.uri.clone(), parser, None)?;
            if newly_inserted {
                warn!("Saved unknown document.");
            }
            document.recompute(parser, None)?;
            Some(
                diagnostics(docs, p.text_document.uri)
                    .into_iter()
                    .map(publish_diagnostics),
            )
        }
        DidOpenTextDocument::METHOD => {
            let p: <DidOpenTextDocument as Notification>::Params =
                serde_json::from_value(noti.params)?;
            let (_, document) = get_doc(
                docs,
                p.text_document.uri.clone(),
                parser,
                Some(p.text_document.text),
            )?;
            document.recompute(parser, None)?;
            Some(
                diagnostics(docs, p.text_document.uri)
                    .into_iter()
                    .map(publish_diagnostics),
            )
        }
        DidCloseTextDocument::METHOD => {
            let p: <DidCloseTextDocument as Notification>::Params =
                serde_json::from_value(noti.params)?;
            if docs.remove(&p.text_document.uri).is_none() {
                warn!("Closed non registered document.")
            }
            None
        }
        method => {
            warn!(
                "Unsupported notification recieved -> {method} {}",
                noti.params
            );
            None
        }
    })
}

fn handle_request(
    docs: &mut Documents,
    parser: &mut Parser,
    get_doc: impl for<'a> FnOnce(
        &'a mut Documents,
        Uri,
        &mut Parser,
        Option<String>,
    ) -> anyhow::Result<(bool, &'a mut Document)>,
    req: lsp_server::Request,
) -> anyhow::Result<Response> {
    Ok(match &*req.method {
        Completion::METHOD => {
            let p: <Completion as Request>::Params = serde_json::from_value(req.params)?;
            let res: <Completion as Request>::Result;
            let (mut pos, uri): (GridIndex, Uri) = {
                let text_document_position = p.text_document_position;

                (
                    text_document_position.position.into(),
                    text_document_position.text_document.uri,
                )
            };

            let (newly_inserted, document) = get_doc(docs, uri, parser, None)?;
            if newly_inserted {
                warn!("Requested completion for unknown document.");
            }
            document.recompute(parser, Some(&mut pos))?;
            res = Some(completions(pos, document)?);
            Response::new_ok(req.id, res)
        }
        References::METHOD => {
            let p: <References as Request>::Params = serde_json::from_value(req.params)?;
            let res: <References as Request>::Result;

            let (mut pos, uri): (GridIndex, Uri) = {
                let text_document_position = p.text_document_position;

                (
                    text_document_position.position.into(),
                    text_document_position.text_document.uri,
                )
            };

            let (newly_inserted, document) = get_doc(docs, uri.clone(), parser, None)?;
            if newly_inserted {
                warn!("Requested references for unknown document.");
            }
            document.recompute(parser, Some(&mut pos))?;
            res = references(pos, p.context, &docs, uri)?;
            Response::new_ok(req.id, res)
        }
        InlayHintRequest::METHOD => {
            let p: <InlayHintRequest as Request>::Params = serde_json::from_value(req.params)?;
            let res: <InlayHintRequest as Request>::Result;

            let uri = p.text_document.uri;

            let (newly_inserted, document) = get_doc(docs, uri, parser, None)?;
            if newly_inserted {
                warn!("Requested inlay hints for unknown document.");
            }
            document.recompute(parser, None)?;
            res = inlay_hints(p.range, document)?;
            Response::new_ok(req.id, res)
        }
        DocumentSymbolRequest::METHOD => {
            let p: <DocumentSymbolRequest as Request>::Params = serde_json::from_value(req.params)?;
            let res: <DocumentSymbolRequest as Request>::Result;

            let uri = p.text_document.uri;

            let (newly_inserted, document) = get_doc(docs, uri, parser, None)?;
            if newly_inserted {
                warn!("Requested document symbols for unknown document.");
            }
            document.recompute(parser, None)?;
            res = document_symbols(document)?;
            Response::new_ok(req.id, res)
        }
        SemanticTokensRangeRequest::METHOD => {
            let p: <SemanticTokensRangeRequest as Request>::Params =
                serde_json::from_value(req.params)?;
            let res: <SemanticTokensRangeRequest as Request>::Result;

            let uri = p.text_document.uri;

            let (newly_inserted, document) = get_doc(docs, uri, parser, None)?;
            if newly_inserted {
                warn!("Requested semantic symbols for unknown document.");
            }
            document.recompute(parser, None)?;
            res = semantic_tokens(p.range, document)?;
            Response::new_ok(req.id, res)
        }
        method => {
            warn!("Unsupported request recieved -> {method} {}", req.params);
            Response::new_ok(req.id, None::<String>)
        }
    })
}
