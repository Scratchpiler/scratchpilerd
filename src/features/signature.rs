use serde_json::{Value, json};

use super::hover::user_sig;
use super::{Reply, position, slot_of};
use crate::data;
use crate::workspace::Snapshot;

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn open_call_tail(chars: &[char], open: usize) -> bool {
    chars.get(open) == Some(&'(') && !chars[open + 1..].contains(&')')
}

fn skip_spaces(chars: &[char], mut i: usize) -> usize {
    while chars.get(i).is_some_and(|c| crate::js::is_space(*c)) {
        i += 1;
    }
    i
}

fn word_end(chars: &[char], i: usize) -> usize {
    let mut j = i;
    while chars.get(j).is_some_and(|c| is_word(*c)) {
        j += 1;
    }
    j
}

fn call_match(chars: &[char]) -> Option<String> {
    (0..chars.len()).filter(|i| is_word(chars[*i])).find_map(|i| {
        let end = word_end(chars, i);
        open_call_tail(chars, skip_spaces(chars, end)).then(|| chars[i..end].iter().collect())
    })
}

fn dot_match(chars: &[char]) -> Option<String> {
    (0..chars.len()).filter(|i| chars[*i] == '[').find_map(|i| {
        let close = (i + 1..chars.len()).find(|k| chars[*k] == ']')?;
        if close == i + 1 || chars.get(close + 1) != Some(&'.') {
            return None;
        }
        let start = close + 2;
        let end = word_end(chars, start);
        (end > start && open_call_tail(chars, skip_spaces(chars, end))).then(|| chars[start..end].iter().collect())
    })
}

fn boundary(chars: &[char], i: usize) -> bool {
    i == 0 || !is_word(chars[i - 1])
}

fn pen_match(chars: &[char]) -> Option<String> {
    (0..chars.len()).find_map(|i| {
        if !boundary(chars, i) || chars.len() < i + 4 || chars[i..i + 4] != ['p', 'e', 'n', '.'] {
            return None;
        }
        let start = i + 4;
        let end = word_end(chars, start);
        (end > start && open_call_tail(chars, skip_spaces(chars, end))).then(|| chars[start..end].iter().collect())
    })
}

fn keyword_match(chars: &[char]) -> Option<&'static str> {
    const WORDS: &[&str] = &["wait until", "repeat until", "if", "while", "repeat", "for"];
    (0..chars.len()).find_map(|i| {
        if !boundary(chars, i) {
            return None;
        }
        WORDS.iter().copied().find(|w| {
            let w: Vec<char> = w.chars().collect();
            chars.len() >= i + w.len() && chars[i..i + w.len()] == w[..] && skip_spaces(chars, i + w.len()) == chars.len()
        })
    })
}

fn make(label: &str, params: Vec<(Value, String)>, active: usize) -> Value {
    let parameters: Vec<Value> =
        params.iter().map(|(label, doc)| if doc.is_empty() { json!({ "label": label }) } else { json!({ "label": label, "documentation": doc }) }).collect();
    let active = active.min(params.len().saturating_sub(1));
    json!({ "signatures": [{ "label": label, "parameters": parameters }], "activeSignature": 0, "activeParameter": active })
}

fn from_sig(sig: &data::Sig, active: usize) -> Value {
    make(sig.label, sig.params.iter().map(|(l, d)| (json!(l), d.to_string())).collect(), active)
}

pub fn signature_help(snapshot: &Snapshot, params: &Value) -> Reply {
    let Some(pos) = position(params) else { return Ok(Value::Null) };
    let slot = slot_of(snapshot, pos.uri)?;
    let line = crate::text::line_text(&slot.text, pos.line - 1);
    let before = crate::js::utf16_prefix(line, pos.col as usize - 1);
    let chars: Vec<char> = before.chars().collect();
    let commas = || before[before.rfind('(').map_or(0, |i| i + 1)..].matches(',').count();
    if let Some(sig) = dot_match(&chars).and_then(|m| data::sig(&m)) {
        return Ok(from_sig(sig, commas()));
    }
    if let Some(sig) = pen_match(&chars).and_then(|m| data::pen_sig(&m)) {
        return Ok(from_sig(sig, commas()));
    }
    if let Some(name) = call_match(&chars) {
        if let Some(sig) = data::sig(&name) {
            return Ok(from_sig(sig, commas()));
        }
        if let Some(schema) = data::asm_opcode(&name) {
            let names: Vec<&str> = schema.iter().map(|p| p.name).collect();
            return Ok(make(&format!("{name}({})", names.join(", ")), names.iter().map(|n| (json!(n), String::new())).collect(), commas()));
        }
        if let Some(user) = user_sig(&slot.analysis(), &name) {
            let labels = user.values.iter().map(|v| match v {
                crate::ast::JsVal::Num(n) => json!(crate::js::number_to_string(*n)),
                crate::ast::JsVal::Str(s) => json!(&**s),
            });
            return Ok(make(&user.label, labels.map(|l| (l, String::new())).collect(), commas()));
        }
    }
    if let Some(sig) = keyword_match(&chars).and_then(data::sig) {
        return Ok(from_sig(sig, 0));
    }
    Ok(Value::Null)
}
