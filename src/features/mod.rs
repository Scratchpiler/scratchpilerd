pub mod completion;
pub mod diagnostics;
pub mod hover;
pub mod lexical;
pub mod navigation;
pub mod outline;
pub mod signature;
pub mod tokens;

use std::sync::Arc;

use serde_json::{Value, json};

use crate::ast::Span;
use crate::index::STAGE;
use crate::workspace::{Slot, Snapshot};

pub type Reply = Result<Value, (i64, String)>;

pub enum Body {
    Json(Value),
    Raw(String),
}

pub const TOKEN_TYPES: &[&str] =
    &["parameter", "variable", "function", "enumMember", "type", "property", "invalid", "keyword", "string", "number", "comment", "operator"];

pub fn capabilities() -> Value {
    json!({
        "capabilities": {
            "positionEncoding": "utf-16",
            "textDocumentSync": { "openClose": true, "change": 2, "save": { "includeText": false } },
            "completionProvider": { "triggerCharacters": ["\"", "(", "[", ".", "<"], "resolveProvider": false },
            "hoverProvider": true,
            "signatureHelpProvider": { "triggerCharacters": ["(", " ", "["], "retriggerCharacters": [","] },
            "definitionProvider": true,
            "referencesProvider": true,
            "documentHighlightProvider": true,
            "renameProvider": { "prepareProvider": true },
            "documentSymbolProvider": true,
            "workspaceSymbolProvider": true,
            "documentFormattingProvider": true,
            "foldingRangeProvider": true,
            "inlayHintProvider": true,
            "semanticTokensProvider": { "legend": { "tokenTypes": TOKEN_TYPES, "tokenModifiers": [] }, "full": true, "range": true },
            "executeCommandProvider": { "commands": [] },
        },
        "serverInfo": { "name": "scratchpilerd", "version": env!("CARGO_PKG_VERSION") },
    })
}

pub struct Position<'a> {
    pub uri: &'a str,
    pub line: u32,
    pub col: u32,
}

pub fn position(params: &Value) -> Option<Position<'_>> {
    Some(Position {
        uri: params["textDocument"]["uri"].as_str()?,
        line: params["position"]["line"].as_u64()? as u32 + 1,
        col: params["position"]["character"].as_u64()? as u32 + 1,
    })
}

pub fn lsp_range(span: &Span) -> Value {
    let end_line = if span.end_line != 0 { span.end_line } else { span.line };
    let end_col = if span.end_col != 0 { span.end_col } else { span.col + 1 };
    json!({
        "start": { "line": span.line.saturating_sub(1), "character": span.col.saturating_sub(1) },
        "end": { "line": end_line.saturating_sub(1), "character": end_col.saturating_sub(1) },
    })
}

pub fn location(uri: &str, span: &Span) -> Value {
    json!({ "uri": uri, "range": lsp_range(span) })
}

pub fn sprite_label(name: &str) -> String {
    if name.is_empty() {
        String::new()
    } else if name == STAGE {
        "Stage".into()
    } else if !crate::js::trim(name).is_empty() {
        name.into()
    } else {
        format!("“{name}”")
    }
}

pub fn plural(n: usize, word: &str) -> String {
    let digits = n.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{grouped} {}", if n == 1 { word.to_string() } else { format!("{word}s") })
}

pub fn slot_of<'a>(snapshot: &'a Snapshot, uri: &str) -> Result<&'a Arc<Slot>, (i64, String)> {
    snapshot.slot(uri).ok_or_else(|| (-32602, format!("unknown document {uri}")))
}

pub fn request(snapshot: &Snapshot, method: &str, params: &Value) -> Result<Body, (i64, String)> {
    if method == "textDocument/completion" {
        return completion::completion(snapshot, params).map(Body::Raw);
    }
    json_request(snapshot, method, params).map(Body::Json)
}

pub fn json_request(snapshot: &Snapshot, method: &str, params: &Value) -> Reply {
    match method {
        "textDocument/hover" => hover::hover(snapshot, params),
        "textDocument/signatureHelp" => signature::signature_help(snapshot, params),
        "textDocument/definition" => navigation::definition(snapshot, params),
        "textDocument/references" => navigation::references(snapshot, params),
        "textDocument/documentHighlight" => navigation::highlights(snapshot, params),
        "textDocument/prepareRename" => navigation::prepare_rename(snapshot, params),
        "textDocument/rename" => navigation::rename(snapshot, params),
        "textDocument/semanticTokens/full" => tokens::full(snapshot, params),
        "textDocument/semanticTokens/range" => tokens::range(snapshot, params),
        "textDocument/inlayHint" => navigation::inlay_hints(snapshot, params),
        "textDocument/documentSymbol" => outline::document_symbols(snapshot, params),
        "workspace/symbol" => outline::workspace_symbols(snapshot, params),
        "textDocument/formatting" => outline::formatting(snapshot, params),
        "textDocument/foldingRange" => outline::folding(snapshot, params),
        "workspace/executeCommand" => Ok(Value::Null),
        "scratchpiler/eventGraph" => {
            Ok(snapshot.linked().map(|l| serde_json::to_value(crate::project::event_graph(&l)).unwrap_or(Value::Null)).unwrap_or(Value::Null))
        }
        "scratchpiler/projectProblems" => Ok(outline::project_problems(snapshot)),
        "scratchpiler/status" => Ok(outline::status(snapshot)),
        _ => Err((-32601, format!("unhandled method {method}"))),
    }
}
