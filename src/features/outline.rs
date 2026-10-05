use serde_json::{Value, json};

use super::{Reply, lsp_range, slot_of, sprite_label};
use crate::ast::{Span, StmtKind};
use crate::project::project_symbols;
use crate::walk::{Node, script_nodes};
use crate::workspace::Snapshot;

pub fn format_source(src: &str) -> String {
    let mut out = Vec::new();
    let mut indent: usize = 0;
    for raw in src.split('\n') {
        let trimmed = crate::js::trim(raw);
        if trimmed.is_empty() {
            out.push(String::new());
            continue;
        }
        if trimmed.starts_with('}') {
            indent = indent.saturating_sub(1);
        }
        out.push(format!("{}{trimmed}", "    ".repeat(indent)));
        if trimmed.ends_with('{') {
            indent += 1;
        }
    }
    out.join("\n")
}

fn full_range(text: &str) -> Value {
    let lines: Vec<&str> = text.split('\n').collect();
    let last = lines.len() - 1;
    json!({ "start": { "line": 0, "character": 0 }, "end": { "line": last, "character": crate::js::utf16_len(lines[last]) } })
}

pub fn formatting(snapshot: &Snapshot, params: &Value) -> Reply {
    let slot = slot_of(snapshot, params["textDocument"]["uri"].as_str().unwrap_or_default())?;
    let formatted = format_source(&slot.text);
    if formatted.is_empty() || formatted == *slot.text {
        return Ok(json!([]));
    }
    Ok(json!([{ "range": full_range(&slot.text), "newText": formatted }]))
}

pub fn folding(snapshot: &Snapshot, params: &Value) -> Reply {
    let slot = slot_of(snapshot, params["textDocument"]["uri"].as_str().unwrap_or_default())?;
    let mut ranges = Vec::new();
    let mut stack = Vec::new();
    for (i, line) in slot.text.split('\n').enumerate() {
        let t = crate::js::trim(line);
        if t.ends_with('{') {
            stack.push(i + 1);
        } else if t.starts_with('}')
            && let Some(start) = stack.pop()
            && i + 1 > start
        {
            ranges.push(json!({ "startLine": start - 1, "endLine": i, "kind": "region" }));
        }
    }
    Ok(Value::Array(ranges))
}

fn symbol(name: String, kind: u32, range: &Span, selection: &Span, children: Vec<Value>) -> Value {
    json!({ "name": name, "kind": kind, "range": lsp_range(range), "selectionRange": lsp_range(selection), "children": children })
}

pub fn document_symbols(snapshot: &Snapshot, params: &Value) -> Reply {
    let slot = slot_of(snapshot, params["textDocument"]["uri"].as_str().unwrap_or_default())?;
    let analysis = slot.analysis();
    let user_lines = analysis.user_line_count().unwrap_or(u32::MAX);
    let mut out = Vec::new();
    for node in script_nodes(&analysis.ast) {
        let Node::Stmt(block, Some(span)) = node else { continue };
        if block.line > user_lines {
            continue;
        }
        let head = Span::new(block.line, block.col, block.line, block.col + 1);
        match &block.kind {
            StmtKind::On { hat, .. } => {
                let label = crate::facts::hat_label(hat);
                out.push(symbol(label, 24, span, &head, Vec::new()));
            }
            StmtKind::Define(d) => {
                let params: Vec<String> = d.params.iter().map(|p| p.text().into_owned()).collect();
                let name_span = Span::new(
                    block.line,
                    block.col,
                    if d.name_end_line != 0 { d.name_end_line } else { block.line },
                    if d.name_end_col != 0 { d.name_end_col } else { block.col + 1 },
                );
                out.push(symbol(format!("define {}({})", d.name.text(), params.join(", ")), 12, span, &name_span, Vec::new()));
            }
            StmtKind::Routine(r) => {
                let params: Vec<String> = r.params.iter().map(|p| p.text().into_owned()).collect();
                out.push(symbol(format!("scratchroutine {}({})", r.name, params.join(", ")), 12, span, &r.name_span.unwrap_or(head), Vec::new()));
            }
            StmtKind::Enum { entries } => {
                let children = entries.iter().map(|e| symbol(e.name.to_string(), 22, &e.span, &e.span, Vec::new())).collect();
                out.push(symbol("enum".into(), 10, span, &head, children));
            }
            StmtKind::Struct { name, name_span, field_spans, .. } => {
                let children = field_spans.iter().map(|f| symbol(f.name.text().into_owned(), 8, &f.span, &f.span, Vec::new())).collect();
                out.push(symbol(format!("struct {name}"), 23, span, &name_span.unwrap_or(head), children));
            }
            _ => {}
        }
    }
    Ok(Value::Array(out))
}

fn matches_query(query: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    let mut chars = name.chars();
    query.to_lowercase().chars().all(|q| chars.any(|c| c == q))
}

pub fn workspace_symbols(snapshot: &Snapshot, params: &Value) -> Reply {
    let query = params["query"].as_str().unwrap_or_default();
    let Some(linked) = snapshot.linked() else { return Ok(json!([])) };
    let out: Vec<Value> = project_symbols(&linked)
        .into_iter()
        .filter(|s| matches_query(query, &s.label) || matches_query(query, &s.name))
        .filter_map(|s| {
            let uri = snapshot.sprite_uri(&s.sprite)?;
            let kind = match s.kind {
                "define" | "routine" => 12,
                "script" => 24,
                "enumMember" => 22,
                "struct" => 23,
                _ => 13,
            };
            let span = Span::new(s.line, s.col, s.line, s.col + 1);
            Some(json!({ "name": s.label, "kind": kind, "containerName": sprite_label(&s.sprite), "location": { "uri": &*uri, "range": lsp_range(&span) } }))
        })
        .collect();
    Ok(Value::Array(out))
}

pub fn project_problems(snapshot: &Snapshot) -> Value {
    let Some(linked) = snapshot.linked() else { return json!([]) };
    Value::Array(
        linked
            .project_items
            .iter()
            .map(|p| json!({ "message": p.message, "owner": &*p.key.owner, "kind": p.key.kind.js(), "name": &*p.key.name, "uri": snapshot.sprite_uri(&p.key.owner).as_deref() }))
            .collect(),
    )
}

pub fn status(snapshot: &Snapshot) -> Value {
    let total = snapshot.slots.len();
    let analyzed = snapshot.slots.values().filter(|s| s.has_derived()).count();
    json!({
        "root": snapshot.root.to_string_lossy(),
        "generation": snapshot.generation,
        "indexLoaded": snapshot.index_loaded,
        "indexFromProject": snapshot.index.from_project,
        "sprites": snapshot.index.sprite_names().collect::<Vec<_>>(),
        "files": total,
        "analyzed": analyzed,
        "ready": snapshot.project_ready(),
    })
}
