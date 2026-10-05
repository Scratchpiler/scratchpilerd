use serde_json::{Value, json};

use super::lexical::{self, Lex};
use super::{Reply, TOKEN_TYPES, slot_of};
use crate::analyzer::semantic_tokens;
use crate::workspace::{SemanticTokenMode, Snapshot};

fn type_index(name: &str) -> u32 {
    TOKEN_TYPES.iter().position(|t| *t == name).unwrap_or(0) as u32
}

fn lex_type(kind: Lex) -> &'static str {
    match kind {
        Lex::Keyword => "keyword",
        Lex::String => "string",
        Lex::Number => "number",
        Lex::Comment => "comment",
        Lex::Operator => "operator",
        Lex::Variable => "variable",
        Lex::MathFunc => "function",
        Lex::TypeIdentifier => "type",
    }
}

pub fn build(snapshot: &Snapshot, text: &str, analysis: &crate::analyzer::Analysis, lines: Option<(u32, u32)>) -> Vec<u32> {
    let line_count = text.split('\n').count() as u32;
    let semantic: Vec<(u32, u32, u32, u32)> =
        semantic_tokens(analysis).into_iter().filter(|t| t.line <= line_count).map(|t| (t.line, t.col, t.length, type_index(t.token_type))).collect();
    let mut merged: Vec<(u32, u32, u32, u32)> = Vec::new();
    if snapshot.settings.semantic_tokens == SemanticTokenMode::Full {
        let lexical = lexical::tokenize(text);
        let mut si = 0;
        for (line, col, len, kind) in lexical {
            while si < semantic.len() && (semantic[si].0, semantic[si].1) < (line, col) {
                merged.push(semantic[si]);
                si += 1;
            }
            let covered = semantic[si..].iter().take_while(|s| s.0 == line && s.1 < col + len).any(|s| s.1 + s.2 > col)
                || merged.last().is_some_and(|m| m.0 == line && m.1 + m.2 > col);
            if !covered {
                merged.push((line, col, len, type_index(lex_type(kind))));
            }
        }
        merged.extend_from_slice(&semantic[si..]);
    } else {
        merged = semantic;
    }
    let mut data = Vec::with_capacity(merged.len() * 5);
    let (mut prev_line, mut prev_col) = (0u32, 0u32);
    for (line, col, len, kind) in merged {
        if let Some((from, to)) = lines
            && (line < from || line > to)
        {
            continue;
        }
        let (line, col) = (line - 1, col - 1);
        if line < prev_line || len == 0 {
            continue;
        }
        let delta_line = line - prev_line;
        let delta_col = if delta_line == 0 {
            if col < prev_col {
                continue;
            }
            col - prev_col
        } else {
            col
        };
        data.extend_from_slice(&[delta_line, delta_col, len, kind, 0]);
        prev_line = line;
        prev_col = col;
    }
    data
}

pub fn full(snapshot: &Snapshot, params: &Value) -> Reply {
    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
    let slot = slot_of(snapshot, uri)?;
    let analysis = slot.analysis();
    Ok(json!({ "data": build(snapshot, &slot.text, &analysis, None) }))
}

pub fn range(snapshot: &Snapshot, params: &Value) -> Reply {
    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
    let slot = slot_of(snapshot, uri)?;
    let analysis = slot.analysis();
    let from = params["range"]["start"]["line"].as_u64().unwrap_or(0) as u32 + 1;
    let to = params["range"]["end"]["line"].as_u64().unwrap_or(u64::MAX >> 33) as u32 + 1;
    Ok(json!({ "data": build(snapshot, &slot.text, &analysis, Some((from, to))) }))
}
