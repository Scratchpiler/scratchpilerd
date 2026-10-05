use serde_json::{Value, json};

use super::{Reply, lexical, lsp_range, plural, position, slot_of, sprite_label};
use crate::analyzer::{Analysis, SymKind};
use crate::ast::Span;
use crate::data::{self, Sig};
use crate::facts::VarKey;
use crate::index::{STAGE, VarKind};
use crate::project::{Linked, ProjectHit, describe_variable, display_message, is_routine_message, message_sites, project_symbol_at};
use crate::workspace::{Slot, Snapshot};

pub struct UserSig {
    pub label: String,
    pub params: Vec<String>,
    pub values: Vec<crate::ast::JsVal>,
}

pub fn user_sig(analysis: &Analysis, name: &str) -> Option<UserSig> {
    let id = analysis.lookup(SymKind::Define, name).or_else(|| analysis.lookup(SymKind::Routine, name))?;
    let sym = analysis.symbol(id);
    let params: Vec<String> = sym.params().iter().map(|p| p.text().into_owned()).collect();
    let kind = if sym.kind == SymKind::Define { "custom block" } else { "scratchroutine" };
    Some(UserSig { label: format!("{name}({})  —  {kind}", params.join(", ")), params, values: sym.params().to_vec() })
}

pub fn project_hit(snapshot: &Snapshot, slot: &Slot, line: u32, col: u32) -> Option<(std::sync::Arc<Linked>, ProjectHit)> {
    let sprite = slot.kind.sprite()?;
    if snapshot.sprite_slot(sprite).is_none_or(|s| s.id != slot.id) {
        return None;
    }
    let linked = snapshot.linked_including(slot)?;
    let analysis = slot.analysis();
    let hit = project_symbol_at(&linked, &analysis, sprite, line, col)?;
    if let ProjectHit::Message { name, .. } = &hit
        && is_routine_message(name)
    {
        return None;
    }
    Some((linked, hit))
}

fn list_sprites<'a>(sprites: impl Iterator<Item = &'a str>) -> String {
    let mut seen: Vec<String> = Vec::new();
    for s in sprites {
        let label = sprite_label(s);
        if !seen.contains(&label) {
            seen.push(label);
        }
    }
    seen.join(", ")
}

pub fn hit_span(hit: &ProjectHit) -> Span {
    match hit {
        ProjectHit::Message { span, .. } | ProjectHit::Variable { span, .. } => *span,
    }
}

pub fn key_name(key: &VarKey) -> &str {
    &key.name
}

fn project_contents(linked: &Linked, hit: &ProjectHit) -> Vec<String> {
    match hit {
        ProjectHit::Message { name, .. } => {
            let (senders, receivers) = message_sites(linked, name);
            let received = if receivers.is_empty() {
                "Nothing receives it".to_string()
            } else {
                format!("Received by {} ({})", list_sprites(receivers.iter().map(|r| &*r.sprite)), plural(receivers.len(), "script"))
            };
            let sent = if senders.is_empty() {
                "Nothing sends it".to_string()
            } else {
                format!("Sent from {} ({})", list_sprites(senders.iter().map(|s| &*s.sprite)), plural(senders.len(), "place"))
            };
            vec![format!("**\"{}\"**", display_message(name)), format!("{received}  \n{sent}")]
        }
        ProjectHit::Variable { key, .. } => {
            let (writes, reads) = describe_variable(linked, key);
            let scope = if &*key.owner == STAGE { "for all sprites".to_string() } else { format!("only {}", sprite_label(&key.owner)) };
            let kind = if key.kind == VarKind::List { "list" } else { "variable" };
            vec![
                format!("**[{}]** · {kind}, {scope}", key.name),
                format!("Written by {}  \nRead by {}", writes.as_deref().unwrap_or("nobody"), reads.as_deref().unwrap_or("nobody")),
            ]
        }
    }
}

fn sig_contents(label: &str, params: &[(String, String)]) -> Vec<String> {
    let mut out = vec![format!("```scratchpiler\n{label}\n```")];
    out.extend(params.iter().filter(|(_, doc)| !doc.is_empty()).map(|(label, doc)| format!("**{label}** — {doc}")));
    out
}

fn static_sig(sig: &Sig) -> (String, Vec<(String, String)>) {
    (sig.label.to_string(), sig.params.iter().map(|(l, d)| (l.to_string(), d.to_string())).collect())
}

pub fn language_hover(slot: &Slot, line_number: u32, col: u32) -> Option<(Value, Vec<String>)> {
    let line = crate::text::line_text(&slot.text, line_number - 1);
    let word = lexical::word_at(line, col)?;
    let prefix = crate::js::utf16_prefix(line, word.start as usize - 1);
    let analysis = slot.analysis();
    let after_pen = prefix.ends_with("pen.") && prefix[..prefix.len() - 4].chars().last().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
    let (label, params) = if after_pen {
        data::pen_sig(&word.text).map(static_sig)?
    } else {
        data::sig(&word.text)
            .map(static_sig)
            .or_else(|| user_sig(&analysis, &word.text).map(|u| (u.label, u.params.into_iter().map(|p| (p, String::new())).collect())))?
    };
    let range = json!({
        "start": { "line": line_number - 1, "character": word.start - 1 },
        "end": { "line": line_number - 1, "character": word.end - 1 },
    });
    Some((range, sig_contents(&label, &params)))
}

pub fn hover(snapshot: &Snapshot, params: &Value) -> Reply {
    let Some(pos) = position(params) else { return Ok(Value::Null) };
    let slot = slot_of(snapshot, pos.uri)?;
    let mut contents = Vec::new();
    let mut range = None;
    if let Some((linked, hit)) = project_hit(snapshot, slot, pos.line, pos.col) {
        range = Some(lsp_range(&hit_span(&hit)));
        contents.extend(project_contents(&linked, &hit));
    }
    if let Some((word_range, docs)) = language_hover(slot, pos.line, pos.col) {
        range.get_or_insert(word_range);
        contents.extend(docs);
    }
    if contents.is_empty() {
        return Ok(Value::Null);
    }
    Ok(json!({ "contents": { "kind": "markdown", "value": contents.join("\n\n") }, "range": range }))
}
