mod completions;
mod diagnostics;
mod document;
pub mod queries;
mod references;

use anyhow::Context;
use lsp_server::{Connection, Message, Response};
use lsp_types::{
    Uri,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
        Notification, PublishDiagnostics,
    },
    request::{Completion, References, Request},
};
use tracing::warn;
use tree_sitter::Parser;

use crate::{
    init::TextFn,
    lsp::{
        completions::completions,
        diagnostics::diagnostics,
        document::{Document, Documents},
        references::references,
    },
};
use texter::change::{Change, GridIndex};

pub fn main_loop(text_fn: TextFn, con: Connection) -> anyhow::Result<()> {
    let mut parser = Parser::new();
    parser.set_language(&prolog_grammar::LANGUAGE.into())?;

    let mut docs = Documents::default();

    for msg in con.receiver {
        match msg {
            Message::Notification(noti) => {
                if let Some(iter) = handle_notification(&mut docs, &mut parser, text_fn, noti)? {
                    iter.map(Message::Notification)
                        .try_for_each(|response| con.sender.send(response))?;
                }
            }
            Message::Request(req) => {
                let response = Message::Response(handle_request(&mut docs, &mut parser, req)?);
                con.sender.send(response)?;
            }
            Message::Response(_) => unreachable!(),
        };
    }

    Ok(())
}

fn handle_notification(
    docs: &mut Documents,
    parser: &mut Parser,
    text_fn: TextFn,
    noti: lsp_server::Notification,
) -> anyhow::Result<Option<impl Iterator<Item = lsp_server::Notification>>> {
    let publish_diagnostics = |res: <PublishDiagnostics as Notification>::Params| {
        lsp_server::Notification::new(PublishDiagnostics::METHOD.to_owned(), res)
    };

    Ok(match &*noti.method {
        DidChangeTextDocument::METHOD => {
            let p: <DidChangeTextDocument as Notification>::Params =
                serde_json::from_value(noti.params)?;
            let document = docs
                .get_mut(&p.text_document.uri)
                .context("Changed unknown document.")?;
            for ch in p.content_changes.into_iter() {
                document.text.update(Change::from(ch), &mut document.tree)?;
            }
            None
        }
        DidSaveTextDocument::METHOD => {
            let p: <DidSaveTextDocument as Notification>::Params =
                serde_json::from_value(noti.params)?;
            let document = docs
                .get_mut(&p.text_document.uri)
                .context("Saved unknown document.")?;
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
            let tree = parser
                .parse(p.text_document.text.as_bytes(), None)
                .context("Tree not returned during parsing")?;
            docs.entry(p.text_document.uri.clone())
                .insert_entry(Document::new(tree, text_fn(p.text_document.text), parser)?)
                .get_mut()
                .recompute(parser, None)?;
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

            let document = docs
                .get_mut(&uri)
                .context("Requested completion for unknown document.")?;
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

            let document = docs
                .get_mut(&uri)
                .context("Requested completion for unknown document.")?;
            document.recompute(parser, Some(&mut pos))?;
            res = references(pos, p.context, &docs, uri)?;
            Response::new_ok(req.id, res)
        }
        method => {
            warn!("Unsupported request recieved -> {method} {}", req.params);
            Response::new_ok(req.id, None::<String>)
        }
    })
}
