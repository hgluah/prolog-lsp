use lsp_types::{
    CompletionOptions, InitializeParams, InitializeResult, OneOf, PositionEncodingKind,
    ServerCapabilities, ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind,
};

use texter::core::text::Text;

pub type TextFn = fn(String) -> Text;

pub fn initialize_result(p: &InitializeParams) -> (TextFn, InitializeResult) {
    let pos_encoding = p
        .capabilities
        .general
        .as_ref()
        .and_then(|g| g.position_encodings.as_deref());

    let (t_fn, enc): (TextFn, _) = if pos_encoding
        .unwrap_or(&[])
        .contains(&PositionEncodingKind::UTF8)
    {
        (Text::new, PositionEncodingKind::UTF8)
    } else {
        (Text::new_utf16, PositionEncodingKind::UTF16)
    };

    let res = InitializeResult {
        capabilities: ServerCapabilities {
            position_encoding: Some(enc),
            text_document_sync: Some(TextDocumentSyncCapability::Kind(
                TextDocumentSyncKind::INCREMENTAL,
            )),
            completion_provider: Some(CompletionOptions {
                trigger_characters: Some(["(", "["].map(str::to_owned).into()),
                ..Default::default()
            }),
            references_provider: Some(OneOf::Left(true)),
            ..Default::default()
        },
        server_info: Some(ServerInfo {
            name: String::from("prolog-lsp"),
            version: env!("CARGO_PKG_VERSION").to_string().into(),
        }),
    };
    (t_fn, res)
}
