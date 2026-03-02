mod completions;
pub mod docs;
mod document;
mod hover;
pub mod queries;

use anyhow::Context;
use completions::completions;
use document::DOCUMENTS;
use hover::hover;
use lsp_server::{Connection, Message, Response};
use lsp_types::{
    TextDocumentPositionParams, Uri,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification,
    },
    request::{Completion, HoverRequest, Request},
};
use tracing::warn;
use tree_sitter::Parser;

use crate::init::TextFn;
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
            let (tree, text) = docs.get_mut(&p.text_document.uri).unwrap();
            for ch in p.content_changes.into_iter() {
                text.update(Change::from(ch), tree)?;
            }
        }
        DidOpenTextDocument::METHOD => {
            let p: <DidOpenTextDocument as Notification>::Params =
                serde_json::from_value(noti.params)?;
            let tree = parser
                .parse(p.text_document.text.as_bytes(), None)
                .context("Tree not returned during parsing")?;
            docs.insert(p.text_document.uri, (tree, text_fn(p.text_document.text)));
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

            let (tree, text) = docs
                .get_mut(&uri)
                .context("Requested completion for unknown document.")?;
            *tree = parser.parse(text.text.as_str(), Some(tree)).unwrap();
            pos.normalize(text)?;
            return Ok(Response::new_ok(
                req.id,
                completions(pos, tree.root_node(), text),
            ));
        }
        HoverRequest::METHOD => {
            let p: <HoverRequest as Request>::Params = serde_json::from_value(req.params)?;
            let TextDocumentPositionParams {
                text_document: id,
                position: pos,
            } = p.text_document_position_params;
            let (tree, text) = docs
                .get_mut(&id.uri)
                .context("Requested hover for unknown document.")?;
            *tree = parser.parse(text.text.as_str(), Some(tree)).unwrap();
            let mut pos = GridIndex::from(pos);
            pos.normalize(text)?;
            return Ok(Response::new_ok(req.id, hover(pos, tree.root_node(), text)));
        }
        method => warn!("Unsupported request recieved -> {method} {}", req.params),
    }

    Ok(Response::new_ok(req.id, None::<String>))
}
