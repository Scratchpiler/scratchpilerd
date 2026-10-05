use serde_json::{Map, Value, json};

use super::hover::{hit_span, project_hit};
use super::{Reply, location, lsp_range, plural, position, slot_of, sprite_label};
use crate::analyzer::{Access, Analysis, Occurrence, SymKind, SymMeta, symbol_at};
use crate::ast::Span;
use crate::lexer::{Tk, Token};
use crate::project::{Linked, ProjectHit, display_message, message_key, message_sites, rename_edits, variable_sites};
use crate::workspace::Snapshot;

fn occ_range(span: &Span) -> Value {
    lsp_range(span)
}

fn site_location(snapshot: &Snapshot, sprite: &str, span: &Span) -> Option<Value> {
    snapshot.sprite_uri(sprite).map(|uri| location(&uri, span))
}

fn occurrences_of(analysis: &Analysis, symbol: u32) -> impl Iterator<Item = &Occurrence> {
    analysis.occurrences.iter().filter(move |o| o.symbol == symbol)
}

fn message_locations(snapshot: &Snapshot, linked: &Linked, name: &str, senders_first: bool, only: Option<bool>) -> Vec<Value> {
    let (senders, receivers) = message_sites(linked, name);
    let senders: Vec<Value> = senders.iter().filter_map(|s| site_location(snapshot, &s.sprite, &s.span)).collect();
    let receivers: Vec<Value> = receivers.iter().filter_map(|r| site_location(snapshot, &r.sprite, &r.span)).collect();
    match only {
        Some(true) => senders,
        Some(false) => receivers,
        None if senders_first => senders.into_iter().chain(receivers).collect(),
        None => receivers.into_iter().chain(senders).collect(),
    }
}

pub fn definition(snapshot: &Snapshot, params: &Value) -> Reply {
    let Some(pos) = position(params) else { return Ok(Value::Null) };
    let slot = slot_of(snapshot, pos.uri)?;
    if let Some((linked, hit)) = project_hit(snapshot, slot, pos.line, pos.col) {
        let sprite = slot.kind.sprite().unwrap_or_default();
        return Ok(match &hit {
            ProjectHit::Message { name, span } => {
                let (_, receivers) = message_sites(&linked, name);
                let is_receiver = receivers.iter().any(|r| &*r.sprite == sprite && r.span.line == span.line && r.span.col == span.col);
                Value::Array(message_locations(snapshot, &linked, name, false, Some(is_receiver)))
            }
            ProjectHit::Variable { key, .. } => Value::Array(
                variable_sites(&linked, key).iter().filter(|s| s.access != Access::Read).filter_map(|s| site_location(snapshot, &s.sprite, &s.span)).collect(),
            ),
        });
    }
    let analysis = slot.analysis();
    let Some(occ) = symbol_at(&analysis, pos.line, pos.col) else { return Ok(Value::Null) };
    let Some(def) = analysis.symbol(occ.symbol).def_range else { return Ok(Value::Null) };
    Ok(location(pos.uri, &def))
}

pub fn references(snapshot: &Snapshot, params: &Value) -> Reply {
    let Some(pos) = position(params) else { return Ok(Value::Null) };
    let slot = slot_of(snapshot, pos.uri)?;
    if let Some((linked, hit)) = project_hit(snapshot, slot, pos.line, pos.col) {
        return Ok(Value::Array(match &hit {
            ProjectHit::Message { name, .. } => message_locations(snapshot, &linked, name, false, None),
            ProjectHit::Variable { key, .. } => variable_sites(&linked, key).iter().filter_map(|s| site_location(snapshot, &s.sprite, &s.span)).collect(),
        }));
    }
    let analysis = slot.analysis();
    let Some(occ) = symbol_at(&analysis, pos.line, pos.col) else { return Ok(Value::Null) };
    Ok(Value::Array(occurrences_of(&analysis, occ.symbol).map(|o| location(pos.uri, &o.span)).collect()))
}

pub fn highlights(snapshot: &Snapshot, params: &Value) -> Reply {
    let Some(pos) = position(params) else { return Ok(Value::Null) };
    let slot = slot_of(snapshot, pos.uri)?;
    let highlight = |span: &Span, write: bool| json!({ "range": occ_range(span), "kind": if write { 3 } else { 2 } });
    if let Some((linked, hit)) = project_hit(snapshot, slot, pos.line, pos.col) {
        let sprite = slot.kind.sprite().unwrap_or_default();
        let sites: Vec<(Span, bool, &str)> = match &hit {
            ProjectHit::Message { name, .. } => {
                let (senders, receivers) = message_sites(&linked, name);
                senders.iter().map(|s| (s.span, true, &*s.sprite)).chain(receivers.iter().map(|r| (r.span, false, &*r.sprite))).collect()
            }
            ProjectHit::Variable { key, .. } => variable_sites(&linked, key).iter().map(|s| (s.span, s.access != Access::Read, &*s.sprite)).collect(),
        };
        return Ok(Value::Array(sites.iter().filter(|(_, _, s)| *s == sprite).map(|(span, write, _)| highlight(span, *write)).collect()));
    }
    let analysis = slot.analysis();
    let Some(occ) = symbol_at(&analysis, pos.line, pos.col) else { return Ok(Value::Null) };
    Ok(Value::Array(occurrences_of(&analysis, occ.symbol).map(|o| highlight(&o.span, o.is_def)).collect()))
}

const RENAMEABLE: &[SymKind] =
    &[SymKind::Define, SymKind::Routine, SymKind::Param, SymKind::LoopVar, SymKind::EnumMember, SymKind::Struct, SymKind::StructField];

fn reject(message: String) -> Reply {
    Err((-32803, message))
}

pub fn prepare_rename(snapshot: &Snapshot, params: &Value) -> Reply {
    let Some(pos) = position(params) else { return Ok(Value::Null) };
    let slot = slot_of(snapshot, pos.uri)?;
    if let Some((_, hit)) = project_hit(snapshot, slot, pos.line, pos.col) {
        let text = match &hit {
            ProjectHit::Message { name, .. } => display_message(name).to_string(),
            ProjectHit::Variable { key, .. } => key.name.to_string(),
        };
        return Ok(json!({ "range": occ_range(&hit_span(&hit)), "placeholder": text }));
    }
    let analysis = slot.analysis();
    let Some(occ) = symbol_at(&analysis, pos.line, pos.col) else { return reject("Nothing to rename here.".into()) };
    let sym = analysis.symbol(occ.symbol);
    if !RENAMEABLE.contains(&sym.kind) || sym.def_range.is_none() {
        return reject(format!("`{}` cannot be renamed here.", sym.name.text()));
    }
    let text = match &sym.meta {
        SymMeta::Field { field, .. } => field.to_string(),
        _ => sym.name.text().into_owned(),
    };
    Ok(json!({ "range": occ_range(&occ.span), "placeholder": text }))
}

fn is_bare_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn text_edit(span: &Span, text: String) -> Value {
    json!({ "range": occ_range(span), "newText": text })
}

pub fn rename(snapshot: &Snapshot, params: &Value) -> Reply {
    let Some(pos) = position(params) else { return Ok(Value::Null) };
    let new_name = params["newName"].as_str().unwrap_or_default();
    let slot = slot_of(snapshot, pos.uri)?;
    if let Some((linked, hit)) = project_hit(snapshot, slot, pos.line, pos.col) {
        let trimmed = new_name.strip_prefix('[').unwrap_or(new_name);
        let trimmed = trimmed.strip_suffix(']').unwrap_or(trimmed);
        let trimmed = trimmed.strip_prefix('"').unwrap_or(trimmed);
        let trimmed = trimmed.strip_suffix('"').unwrap_or(trimmed);
        if crate::js::trim(trimmed).is_empty() {
            return reject("The new name is empty.".into());
        }
        let mut changes = Map::new();
        for (sprite, edits) in rename_edits(&linked, &hit, trimmed) {
            let Some(uri) = snapshot.sprite_uri(&sprite) else { continue };
            changes.insert(uri.to_string(), Value::Array(edits.into_iter().map(|(span, text)| text_edit(&span, text)).collect()));
        }
        return Ok(json!({ "changes": changes }));
    }
    let analysis = slot.analysis();
    let Some(occ) = symbol_at(&analysis, pos.line, pos.col) else { return Ok(Value::Null) };
    let sym = analysis.symbol(occ.symbol);
    if !RENAMEABLE.contains(&sym.kind) {
        return Ok(Value::Null);
    }
    if !is_bare_name(new_name) {
        return reject(format!("`{new_name}` is not a valid name — use letters, digits, and underscores, starting with a letter or underscore."));
    }
    let collides = match &sym.meta {
        SymMeta::Field { struct_name, .. } => analysis.lookup(SymKind::StructField, &format!("{struct_name}.{new_name}")).is_some(),
        _ => analysis.lookup(sym.kind, new_name).is_some(),
    };
    if collides {
        return reject(format!("A {} named `{new_name}` already exists.", sym.kind.js()));
    }
    let edits: Vec<Value> = occurrences_of(&analysis, occ.symbol)
        .map(|o| {
            let text = match &sym.meta {
                SymMeta::Field { struct_name, .. } if !o.is_def => format!("[{struct_name}.{new_name}]"),
                SymMeta::Field { .. } => new_name.to_string(),
                _ if o.is_bracketed => format!("[{new_name}]"),
                _ => new_name.to_string(),
            };
            text_edit(&o.span, text)
        })
        .collect();
    let mut changes = Map::new();
    changes.insert(slot.uri.to_string(), Value::Array(edits));
    Ok(json!({ "changes": changes }))
}

pub struct HintSite {
    pub send: bool,
    pub msg: String,
    pub line: u32,
    pub column: u32,
}

pub fn broadcast_hint_sites(src: &str, tokens: &[Token], line_count: u32, start_line: u32, end_line: u32) -> Vec<HintSite> {
    let is_word = |t: &Token, word: &str| t.kind != Tk::Str && t.kind != Tk::Num && t.text(src) == word;
    let mut sites = Vec::new();
    for i in 0..tokens.len().saturating_sub(2) {
        let (word, next, s) = (&tokens[i], &tokens[i + 1], &tokens[i + 2]);
        if s.kind != Tk::Str || s.end_line != s.line || s.line < start_line || s.line > end_line.min(line_count) {
            continue;
        }
        let send_word = word.kind != Tk::Str && word.kind != Tk::Num && matches!(word.text(src), "broadcast" | "broadcastAndWait" | "send" | "sendAndWait");
        if send_word && next.kind == Tk::LParen {
            let close = tokens.get(i + 3).filter(|t| t.kind == Tk::RParen && t.line == s.line).unwrap_or(s);
            sites.push(HintSite { send: true, msg: s.text(src).to_string(), line: s.line, column: close.end_col });
        } else if is_word(word, "on") && is_word(next, "receive") {
            sites.push(HintSite { send: false, msg: s.text(src).to_string(), line: s.line, column: s.end_col });
        }
    }
    sites
}

fn list_names(sprites: &[std::sync::Arc<str>]) -> String {
    let mut seen: Vec<String> = Vec::new();
    for s in sprites {
        let label = sprite_label(s);
        if !seen.contains(&label) {
            seen.push(label);
        }
    }
    seen.join(", ")
}

pub fn inlay_hints(snapshot: &Snapshot, params: &Value) -> Reply {
    if !snapshot.settings.inlay_hints {
        return Ok(Value::Array(Vec::new()));
    }
    let Some(uri) = params["textDocument"]["uri"].as_str() else { return Ok(Value::Null) };
    let slot = slot_of(snapshot, uri)?;
    if slot.kind.sprite().is_none() {
        return Ok(Value::Array(Vec::new()));
    }
    slot.derived();
    let Some(counts) = snapshot.stable_counts() else { return Ok(Value::Array(Vec::new())) };
    let dynamic_send = snapshot.linked().is_some_and(|l| l.dynamic_send);
    let analysis = slot.analysis();
    let line_count = slot.text.split('\n').count() as u32;
    let start = params["range"]["start"]["line"].as_u64().map_or(1, |l| l as u32 + 1);
    let end = params["range"]["end"]["line"].as_u64().map_or(line_count, |l| l as u32 + 1);
    let empty = (Vec::new(), Vec::new());
    let hints: Vec<Value> = broadcast_hint_sites(&analysis.src, &analysis.tokens, line_count, start, end)
        .into_iter()
        .map(|site| {
            let (senders, receivers) = counts.get(&message_key(&site.msg)).unwrap_or(&empty);
            let (label, tooltip) = if site.send {
                if receivers.is_empty() {
                    ("no listeners".to_string(), "Nothing has an `on receive` for this message".to_string())
                } else {
                    (plural(receivers.len(), "listener"), format!("Received by {}", list_names(receivers)))
                }
            } else if senders.is_empty() {
                let tooltip =
                    if dynamic_send { "No literal broadcast sends it, but some broadcasts use computed names" } else { "Nothing broadcasts this message" };
                ("never sent".to_string(), tooltip.to_string())
            } else {
                (format!("sent from {}", plural(senders.len(), "place")), format!("Sent from {}", list_names(senders)))
            };
            json!({ "position": { "line": site.line - 1, "character": site.column - 1 }, "label": label, "paddingLeft": true, "tooltip": tooltip })
        })
        .collect();
    Ok(Value::Array(hints))
}
