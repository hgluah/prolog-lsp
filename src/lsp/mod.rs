mod completions;
mod document;
pub mod queries;

use anyhow::Context;
use document::DOCUMENTS;
use lsp_server::{Connection, Message, Response};
use lsp_types::{
    Uri,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification,
    },
    request::{Completion, Request},
};
use tracing::warn;
use tree_sitter::Parser;

use crate::{
    init::TextFn,
    lsp::{completions::completions, document::Document},
};
use texter::change::{Change, GridIndex};

pub fn main_loop(text_fn: TextFn, con: Connection) -> anyhow::Result<()> {
    let mut parser = Parser::new();
    parser.set_language(&prolog_grammar::LANGUAGE.into())?;
    for msg in con.receiver {
        match msg {
            Message::Notification(noti) => handle_notification(&mut parser, text_fn, noti)?,
            Message::Request(req) => con
                .sender
                .send(Message::Response(handle_request(&mut parser, req)?))?,
            _ => continue,
        };
    }

    Ok(())
}

fn handle_notification(
    parser: &mut Parser,
    text_fn: TextFn,
    noti: lsp_server::Notification,
) -> anyhow::Result<()> {
    let mut docs = DOCUMENTS.lock().unwrap();
    match &*noti.method {
        DidChangeTextDocument::METHOD => {
            let p: <DidChangeTextDocument as Notification>::Params =
                serde_json::from_value(noti.params)?;
            let document = docs.get_mut(&p.text_document.uri).unwrap();
            for ch in p.content_changes.into_iter() {
                document.text.update(Change::from(ch), &mut document.tree)?;
            }
        }
        DidOpenTextDocument::METHOD => {
            let p: <DidOpenTextDocument as Notification>::Params =
                serde_json::from_value(noti.params)?;
            let tree = parser
                .parse(p.text_document.text.as_bytes(), None)
                .context("Tree not returned during parsing")?;
            docs.insert(
                p.text_document.uri,
                Document::new(tree, text_fn(p.text_document.text), parser)?,
            );
        }
        DidCloseTextDocument::METHOD => {
            let p: <DidCloseTextDocument as Notification>::Params =
                serde_json::from_value(noti.params)?;
            if docs.remove(&p.text_document.uri).is_none() {
                warn!("Closed non registered document.")
            }
        }
        method => warn!(
            "Unsupported notification recieved -> {method} {}",
            noti.params,
        ),
    };

    Ok(())
}

fn handle_request(parser: &mut Parser, req: lsp_server::Request) -> anyhow::Result<Response> {
    let mut docs = DOCUMENTS.lock().unwrap();
    match &*req.method {
        Completion::METHOD => {
            let p: <Completion as Request>::Params = serde_json::from_value(req.params)?;
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
            return Ok(Response::new_ok(req.id, completions(pos, document)?));
        }
        method => warn!("Unsupported request recieved -> {method} {}", req.params),
    }

    Ok(Response::new_ok(req.id, None::<String>))
}
