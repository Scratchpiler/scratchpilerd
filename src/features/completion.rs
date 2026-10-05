use serde_json::{Value, json};

use super::lexical::word_until;
use super::slot_of;
use crate::analyzer::{SymKind, SymMeta, visible_symbols};
use crate::ast::EnumValue;
use crate::data;
use crate::index::{STAGE, VarKind};
use crate::js;
use crate::lexer::{Tk, Token, tokenize};
use crate::project::is_compiler_variable;
use crate::workspace::{FileKind, Slot, Snapshot};

const MAX_LOOKBACK_LINES: u32 = 8;
const DEFINE_MODIFIERS: &[&str] = &["returns", "warp", "noinline"];
const LOOP_MODIFIERS: &[&str] = &["nounroll"];
const OPERATOR_WORDS: &[&str] = &["and", "or", "not", "mod", "from", "to", "by", "in"];

#[derive(Clone, Debug, PartialEq)]
pub enum Ctx {
    Comment,
    String { quote_col: u32, slot: Option<&'static str> },
    StructField { name: String },
    VarName,
    Include,
    Routine { with_call: bool },
    ListMethod,
    PenMethod,
    Member,
    Modifier(Vec<&'static str>),
    General { after_lt: bool, on_col: Option<u32> },
}

struct Toks<'a> {
    src: &'a str,
    toks: Vec<Token>,
}

impl Toks<'_> {
    fn value<'s>(&'s self, t: &'s Token) -> Option<&'s str> {
        (t.kind != Tk::Num).then(|| t.text(self.src))
    }

    fn is(&self, t: &Token, word: &str) -> bool {
        self.value(t) == Some(word)
    }

    fn is_word(&self, t: &Token) -> bool {
        if matches!(t.kind, Tk::Str | Tk::IStr | Tk::Var | Tk::Num | Tk::Comment | Tk::Hex) {
            return false;
        }
        let v = t.text(self.src);
        let mut chars = v.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    fn ends_operand(&self, t: &Token) -> bool {
        matches!(t.kind, Tk::Num | Tk::Var | Tk::RParen | Tk::Str | Tk::IStr) || (self.is_word(t) && !OPERATOR_WORDS.contains(&t.text(self.src)))
    }
}

fn lex(src: &str, comments: bool) -> Toks<'_> {
    let mut toks = tokenize(src, comments);
    toks.pop();
    Toks { src, toks }
}

fn call_slot(callee: &str, arg: usize) -> Option<&'static str> {
    let slots: &[Option<&'static str>] = match callee {
        "switchCostume" => &[Some("costumes")],
        "switchBackdrop" | "switchBackdropAndWait" => &[Some("backdrops")],
        "play" | "playUntilDone" => &[Some("sounds")],
        "broadcast" | "broadcastAndWait" | "send" | "sendAndWait" => &[Some("broadcasts")],
        "goTo" | "pointTowards" => &[Some("targets")],
        "glide" => &[None, Some("targets")],
        "distanceTo" => &[Some("mouseTargets")],
        "xOf" | "yOf" | "directionOf" | "sizeOf" | "costumeNumOf" | "costumeNameOf" | "volumeOf" => &[Some("ofTargets")],
        "touching" => &[Some("touchTargets")],
        "key" => &[Some("keys")],
        "setEffect" | "changeEffect" => &[Some("effects")],
        "setSoundEffect" | "changeSoundEffect" => &[Some("soundEffects")],
        "setRotationStyle" => &[Some("rotationStyles")],
        "setDragMode" => &[Some("dragModes")],
        "setPenColorParam" | "changePenColorParam" | "setColorParam" | "changeColorParam" => &[Some("penParams")],
        "currentTime" => &[Some("timeUnits")],
        "createClone" => &[Some("clones")],
        "sort" => &[Some("sortDirections")],
        _ => &[],
    };
    slots.get(arg).copied().flatten()
}

enum Scan {
    Decided(Option<&'static str>),
    Carry { depth: u32, arg: usize },
}

fn scan_slot(t: &Toks<'_>, from: Option<usize>, depth: u32, arg: usize) -> Scan {
    let (mut depth, mut arg) = (depth, arg);
    let Some(from) = from else { return Scan::Carry { depth, arg } };
    for i in (0..=from).rev() {
        let tok = &t.toks[i];
        match tok.kind {
            Tk::LBrace | Tk::RBrace => return Scan::Decided(None),
            Tk::RParen => depth += 1,
            Tk::Comma if depth == 0 => arg += 1,
            Tk::LParen => {
                if depth > 0 {
                    depth -= 1;
                    continue;
                }
                let callee = i.checked_sub(1).map(|j| &t.toks[j]);
                return Scan::Decided(callee.filter(|c| t.is_word(c)).and_then(|c| call_slot(c.text(t.src), arg)));
            }
            _ => {}
        }
    }
    Scan::Carry { depth, arg }
}

fn string_slot(t: &Toks<'_>, k: usize, line_above: &dyn Fn(u32) -> Option<String>) -> Option<&'static str> {
    if k >= 2 {
        let (hat, kind) = (&t.toks[k - 2], &t.toks[k - 1]);
        if t.is(hat, "on") {
            match t.value(kind) {
                Some("receive") => return Some("broadcasts"),
                Some("backdrop") => return Some("backdrops"),
                Some("key") => return Some("keys"),
                _ => {}
            }
        }
    }
    let mut state = scan_slot(t, k.checked_sub(1), 0, 0);
    for n in 1..=MAX_LOOKBACK_LINES {
        let Scan::Carry { depth, arg } = state else { break };
        let Some(line) = line_above(n) else { break };
        let above = lex(&line, false);
        if above.toks.iter().any(|t| t.unterminated) {
            break;
        }
        state = scan_slot(&above, above.toks.len().checked_sub(1), depth, arg);
        let _ = (depth, arg);
    }
    match state {
        Scan::Decided(slot) => slot,
        Scan::Carry { .. } => None,
    }
}

fn open_interpolation(prefix: &[u16], quote_col: usize) -> Option<(String, usize)> {
    let body = &prefix[quote_col.min(prefix.len())..];
    let (mut i, mut start, mut depth): (usize, Option<usize>, u32) = (0, None, 0);
    let (lb, rb, quote) = (b'{' as u16, b'}' as u16, b'"' as u16);
    while i < body.len() {
        let c = body[i];
        match start {
            None => {
                if (c == lb || c == rb) && body.get(i + 1) == Some(&c) {
                    i += 2;
                } else if c == lb {
                    i += 1;
                    start = Some(i);
                    depth = 1;
                } else {
                    i += 1;
                }
            }
            Some(s) => {
                if c == quote {
                    match body[i + 1..].iter().position(|u| *u == quote) {
                        None => return Some((String::from_utf16_lossy(&body[s..]), quote_col + s)),
                        Some(close) => i = i + 1 + close + 1,
                    }
                } else {
                    if c == lb {
                        depth += 1;
                    } else if c == rb {
                        depth -= 1;
                        if depth == 0 {
                            start = None;
                        }
                    }
                    i += 1;
                }
            }
        }
    }
    start.map(|s| (String::from_utf16_lossy(&body[s..]), quote_col + s))
}

fn modifier_candidates(t: &Toks<'_>, head: &[Token]) -> Option<Vec<&'static str>> {
    let break_at = head.iter().rposition(|x| matches!(x.kind, Tk::LBrace | Tk::RBrace | Tk::Semi));
    let statement = &head[break_at.map_or(0, |b| b + 1)..];
    let first = statement.first()?;
    if !t.is_word(first) {
        return None;
    }
    let unused = |all: &[&'static str], taken: &[&str]| all.iter().copied().filter(|n| !taken.contains(n)).collect::<Vec<_>>();
    if t.is(first, "define") {
        let close = statement.iter().position(|x| x.kind == Tk::RParen)?;
        let taken = &statement[close + 1..];
        if !taken.iter().all(|x| t.value(x).is_some_and(|v| DEFINE_MODIFIERS.contains(&v)) && t.is_word(x)) {
            return None;
        }
        let names: Vec<&str> = taken.iter().filter_map(|x| t.value(x)).collect();
        return Some(unused(DEFINE_MODIFIERS, &names));
    }
    let tail = statement.last()?;
    let has_modifier = statement.iter().any(|x| t.value(x).is_some_and(|v| LOOP_MODIFIERS.contains(&v)));
    let loop_mods = || if has_modifier { Vec::new() } else { LOOP_MODIFIERS.to_vec() };
    if t.is(first, "repeat") && statement.get(1).is_some_and(|s| !t.is(s, "until")) && t.ends_operand(tail) {
        return Some(loop_mods());
    }
    if t.is(first, "for") {
        let to = statement.iter().enumerate().position(|(i, x)| i > 0 && t.is_word(x) && t.is(x, "to"));
        if to.is_some_and(|to| to > 0 && statement.len() > to + 1) && t.ends_operand(tail) {
            return Some(loop_mods());
        }
    }
    None
}

pub fn completion_context(prefix: &str, line_above: &dyn Fn(u32) -> Option<String>) -> Ctx {
    let t = lex(prefix, true);
    let Some(last) = t.toks.last() else { return Ctx::General { after_lt: false, on_col: None } };
    if last.kind == Tk::Comment {
        return Ctx::Comment;
    }
    if last.unterminated {
        if last.kind == Tk::Var {
            let value = last.text(prefix);
            if let Some((name, rest)) = value.split_once('.')
                && !name.is_empty()
                && !name.contains(']')
                && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Ctx::StructField { name: name.to_string() };
            }
            return Ctx::VarName;
        }
        let units: Vec<u16> = prefix.encode_utf16().collect();
        if let Some((text, offset)) = open_interpolation(&units, last.col as usize) {
            return match completion_context(&text, &|_| None) {
                Ctx::String { quote_col, slot } => Ctx::String { quote_col: quote_col + offset as u32, slot },
                Ctx::General { after_lt, .. } => Ctx::General { after_lt, on_col: None },
                other => other,
            };
        }
        return Ctx::String { quote_col: last.col, slot: string_slot(&t, t.toks.len() - 1, line_above) };
    }

    let partial = t.is_word(last) && last.end_col == js::utf16_len(prefix) + 1;
    let head = if partial { &t.toks[..t.toks.len() - 1] } else { &t.toks[..] };
    let Some(tail) = head.last() else { return Ctx::General { after_lt: false, on_col: None } };
    let before = head.len().checked_sub(2).map(|i| &head[i]);

    if head.len() >= 3 && head[0].kind == Tk::Hash && t.is(&head[1], "include") && head[2].kind == Tk::Lt && !head.iter().any(|x| x.kind == Tk::Gt) {
        return Ctx::Include;
    }
    if tail.kind == Tk::Dot {
        if before.is_some_and(|b| b.kind == Tk::Var) {
            return Ctx::ListMethod;
        }
        if before.is_some_and(|b| t.is(b, "pen") && t.is_word(b)) {
            return Ctx::PenMethod;
        }
        return Ctx::Member;
    }
    if t.is_word(tail) && matches!(t.value(tail), Some("launch" | "await" | "cancel")) {
        return Ctx::Routine { with_call: !t.is(tail, "cancel") };
    }
    if tail.kind == Tk::LParen && before.is_some_and(|b| t.is(b, "isRunning")) {
        return Ctx::Routine { with_call: false };
    }
    let needs_space_first = !partial && !prefix.chars().last().is_some_and(js::is_space);
    if !needs_space_first && let Some(candidates) = modifier_candidates(&t, head) {
        return Ctx::Modifier(candidates);
    }
    Ctx::General { after_lt: tail.kind == Tk::Lt, on_col: (t.is_word(tail) && t.is(tail, "on")).then_some(tail.col) }
}

fn js_truthy(value: &crate::ast::JsVal) -> bool {
    match value {
        crate::ast::JsVal::Str(s) => !s.is_empty(),
        crate::ast::JsVal::Num(n) => *n != 0.0 && !n.is_nan(),
    }
}

fn kind_number(kind: &str) -> u32 {
    match kind {
        "Method" => 2,
        "Function" => 3,
        "Constructor" => 4,
        "Field" => 5,
        "Variable" => 6,
        "Class" => 7,
        "Interface" => 8,
        "Module" => 9,
        "Property" => 10,
        "Unit" => 11,
        "Value" => 12,
        "Enum" => 13,
        "Keyword" => 14,
        "Snippet" => 15,
        "Color" => 16,
        "File" => 17,
        "Reference" => 18,
        "Folder" => 19,
        "EnumMember" => 20,
        "Constant" => 21,
        "Struct" => 22,
        "Event" => 23,
        "Operator" => 24,
        "TypeParameter" => 25,
        _ => 1,
    }
}

#[derive(Default)]
struct Item {
    label: String,
    kind: &'static str,
    detail: Option<String>,
    documentation: Option<String>,
    insert_text: String,
    snippet: bool,
    sort_text: Option<String>,
    filter_text: Option<String>,
    command: Option<&'static str>,
    range: (u32, u32),
    statik: Option<&'static data::StaticItem>,
}

impl Item {
    fn label(&self) -> &str {
        self.statik.map_or(&self.label, |s| s.label)
    }

    fn insert_text(&self) -> &str {
        self.statik.map_or(&self.insert_text, |s| s.insert_text)
    }

    fn detail(&self) -> Option<&str> {
        self.statik.map(|s| s.detail).or(self.detail.as_deref())
    }

    fn documentation(&self) -> Option<&str> {
        self.statik.map_or(self.documentation.as_deref(), |s| s.documentation)
    }

    fn filter_text(&self) -> Option<&str> {
        self.statik.map_or(self.filter_text.as_deref(), |s| s.filter_text)
    }

    fn command(&self) -> Option<&'static str> {
        self.statik.map_or(self.command, |s| Some(s.command)).filter(|c| !c.is_empty())
    }
}

fn push_json_str(out: &mut String, s: &str) {
    out.push('"');
    let mut start = 0;
    for (i, b) in s.bytes().enumerate() {
        let escape = match b {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            0x08 => "\\b",
            0x0c => "\\f",
            0..=0x1f => "",
            _ => continue,
        };
        out.push_str(&s[start..i]);
        if escape.is_empty() {
            out.push_str(&format!("\\u{b:04x}"));
        } else {
            out.push_str(escape);
        }
        start = i + 1;
    }
    out.push_str(&s[start..]);
    out.push('"');
}

fn follow_up_command(text: &str) -> Option<&'static str> {
    let bytes = text.as_bytes();
    let idx = (0..bytes.len()).find(|&i| {
        bytes[i] == b'$'
            && (bytes.get(i + 1).is_some_and(u8::is_ascii_digit) || (bytes.get(i + 1) == Some(&b'{') && bytes.get(i + 2).is_some_and(u8::is_ascii_digit)))
    })?;
    if idx == 0 {
        return None;
    }
    let rest = &text[idx..];
    if let Some(after) = rest.strip_prefix("${") {
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after.as_bytes().get(digits) == Some(&b'|') {
            return None;
        }
    }
    for word in ["launch $", "await $", "cancel $"] {
        if text.starts_with(word) {
            return Some("editor.action.triggerSuggest");
        }
    }
    match bytes[idx - 1] {
        b'"' | b'[' | b'<' => Some("editor.action.triggerSuggest"),
        b'(' => Some("editor.action.triggerParameterHints"),
        _ => None,
    }
}

fn stem_end(label: &str) -> usize {
    let line_break = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    for (i, _) in label.char_indices() {
        let rest = &label[i..];
        let open = rest.trim_start_matches(js::is_space);
        if open.starts_with(['(', '{']) && !open[1..].contains(line_break) {
            return i;
        }
    }
    label.len()
}

fn with_behavior(items: &mut [Item]) {
    for item in items.iter_mut() {
        if item.filter_text.is_none() {
            let stem = &item.label[..stem_end(&item.label)];
            if !stem.is_empty() && stem != item.label {
                item.filter_text = Some(stem.to_string());
            }
        }
        if item.command.is_none() {
            item.command = follow_up_command(&item.insert_text);
        }
    }
}

fn snippet_call(name: &str, params: &[String]) -> String {
    format!("{name}({})", params.iter().enumerate().map(|(i, p)| format!("${{{}:{p}}}", i + 1)).collect::<Vec<_>>().join(", "))
}

const KEYS_STATIC: &[(&str, &str, &str, &str)] = &[
    ("length()", "length()", "String / list length", ""),
    ("len()", "len()", "String / list length (alias)", ""),
    ("contains(item)", "contains(${1:item})", "List · data_listcontainsitem", ""),
    ("item(index)", "item(${1:index})", "List · data_itemoflist", ""),
    ("indexOf(item)", "indexOf(${1:item})", "List · data_itemnumoflist", ""),
    ("sort()", "sort()", "List · sort ascending (Shell sort)", ""),
    ("sort(\"desc\")", "sort(\"desc\")", "List · sort descending (Shell sort)", ""),
    ("sum()", "sum()", "List · sum of numeric items", ""),
    ("min()", "min()", "List · smallest numeric item", ""),
    ("max()", "max()", "List · largest numeric item", ""),
    ("count(value)", "count(${1:value})", "List · items equal to value", ""),
];

const PEN_METHODS: &[(&str, &str, &str)] = &[
    ("down()", "down()", "Pen · penDown()"),
    ("up()", "up()", "Pen · penUp()"),
    ("clear()", "clear()", "Pen · penClear() — erase all pen marks and stamps"),
    ("stamp()", "stamp()", "Pen · stamp()"),
    ("setColor(color)", "setColor(#$0)", "Pen · setPenColor(color)"),
    ("setSize(size)", "setSize($0)", "Pen · setPenSize(size)"),
    ("changeSize(amount)", "changeSize($0)", "Pen · changePenSize(amount)"),
    ("setColorParam(\"param\", value)", "setColorParam(\"${1|color,saturation,brightness,transparency|}\", $0)", "Pen · setPenColorParam(param, value)"),
    (
        "changeColorParam(\"param\", amount)",
        "changeColorParam(\"${1|color,saturation,brightness,transparency|}\", $0)",
        "Pen · changePenColorParam(param, amount)",
    ),
];

pub fn struct_fields(src: &str) -> Vec<(String, Vec<String>)> {
    let t = lex(src, false);
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut i = 0;
    while i + 2 < t.toks.len() {
        if !(t.is(&t.toks[i], "struct") && t.is_word(&t.toks[i]) && t.is_word(&t.toks[i + 1]) && t.toks[i + 2].kind == Tk::LBrace) {
            i += 1;
            continue;
        }
        let mut fields = Vec::new();
        let mut j = i + 3;
        while j < t.toks.len() && t.toks[j].kind != Tk::RBrace {
            if t.is_word(&t.toks[j]) {
                fields.push(t.toks[j].text(src).to_string());
            }
            j += 1;
        }
        let name = t.toks[i + 1].text(src).to_string();
        match out.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = fields,
            None => out.push((name, fields)),
        }
        i = j + 1;
    }
    out
}

fn project_variables<'a>(snapshot: &'a Snapshot, slot: &Slot) -> Vec<&'a crate::index::VarInfo> {
    let sprite = slot.kind.sprite().unwrap_or("");
    let locals: &[crate::index::VarInfo] = if !sprite.is_empty() && sprite != STAGE { snapshot.index.sprite_vars(sprite) } else { &[] };
    snapshot.index.data.global_variables.iter().chain(locals.iter()).filter(|v| !is_compiler_variable(&v.name)).collect()
}

fn utf16_offset(text: &str, line: u32, col: u32) -> usize {
    let mut offset = 0;
    for (i, l) in text.split('\n').enumerate() {
        if i as u32 + 1 == line {
            return offset + col as usize - 1;
        }
        offset += js::utf16_len(l) as usize + 1;
    }
    offset
}

enum AsmCtx {
    Opcode,
    Register,
}

fn asm_context(text: &str, line: u32, col: u32) -> Option<AsmCtx> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let offset = utf16_offset(text, line, col);
    let needle: Vec<u16> = "__asm__".encode_utf16().collect();
    let limit = (offset.saturating_sub(1) + needle.len()).min(units.len());
    if offset == 0 || !units[..limit].windows(needle.len()).any(|w| w == needle.as_slice()) {
        return None;
    }
    let toks = lex(text, false);
    let pos_of = |t: &Token| utf16_offset(text, t.line, t.col);
    let mut last_asm = None;
    for (i, t) in toks.toks.iter().enumerate() {
        if pos_of(t) >= offset {
            break;
        }
        if toks.is(t, "__asm__") {
            last_asm = Some(i);
        }
    }
    let last_asm = last_asm?;
    let (mut depth, mut saw_open) = (0i32, false);
    for t in &toks.toks[last_asm + 1..] {
        if pos_of(t) >= offset {
            break;
        }
        match t.kind {
            Tk::LParen => {
                depth += 1;
                saw_open = true;
            }
            Tk::RParen => {
                depth -= 1;
                if saw_open && depth <= 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    if !saw_open || depth <= 0 {
        return None;
    }
    Some(if depth == 1 { AsmCtx::Opcode } else { AsmCtx::Register })
}

fn word_boundary_match<'a>(head: &'a str, words: &[&str]) -> Option<(usize, &'a str)> {
    let bytes = head.as_bytes();
    for i in 0..bytes.len() {
        if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
            continue;
        }
        for w in words {
            if head[i..].starts_with(w) {
                return Some((i, &head[i + w.len()..]));
            }
        }
    }
    None
}

fn scope_names(head: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut search = head;
    while let Some((at, rest)) = word_boundary_match(search, &["pyfor", "for"]) {
        let after = rest.trim_start_matches(js::is_space);
        if let Some(inner) = after.strip_prefix('[')
            && let Some(close) = inner.find(']')
            && close > 0
        {
            names.push(js::trim(&inner[..close]).to_string());
            break;
        }
        search = &search[at + 1..];
    }
    let mut search = head;
    while let Some((at, rest)) = word_boundary_match(search, &["scratchroutine"]) {
        let after = rest.trim_start_matches(js::is_space);
        let skipped = rest.len() - after.len();
        let word_len = after.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_').count();
        let rest2 = after[word_len..].trim_start_matches(js::is_space);
        if skipped > 0
            && word_len > 0
            && let Some(inner) = rest2.strip_prefix('(')
            && let Some(close) = inner.find(')')
        {
            names.extend(inner[..close].split(',').map(|p| js::trim(p).to_string()).filter(|p| !p.is_empty()));
            break;
        }
        search = &search[at + 1..];
    }
    names
}

fn registers_in_scope(snapshot: &Snapshot, slot: &Slot, line: u32, col: u32) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |name: &str, source: String| {
        if !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), source));
        }
    };
    for v in &snapshot.index.data.global_variables {
        if !is_compiler_variable(&v.name) {
            add(&v.name, format!("Global {}", v.kind.js()));
        }
    }
    if let Some(sprite) = slot.kind.sprite() {
        for v in snapshot.index.sprite_vars(sprite) {
            if !is_compiler_variable(&v.name) {
                add(&v.name, format!("{sprite} {}", v.kind.js()));
            }
        }
    }
    let units: Vec<u16> = slot.text.encode_utf16().collect();
    let offset = utf16_offset(&slot.text, line, col).min(units.len());
    let before = String::from_utf16_lossy(&units[..offset]);
    let (mut depth, mut seg_start) = (0i32, 0usize);
    let mut stack: Vec<(i32, Vec<String>)> = Vec::new();
    for (i, c) in before.char_indices() {
        if c == '{' {
            let names = scope_names(&before[seg_start..i]);
            seg_start = i + 1;
            stack.push((depth, names));
            depth += 1;
        } else if c == '}' {
            seg_start = i + 1;
            depth -= 1;
            while stack.last().is_some_and(|(d, _)| *d >= depth) {
                stack.pop();
            }
        }
    }
    for (_, names) in stack {
        for name in names {
            add(&name, "Loop / routine variable".into());
        }
    }
    out
}

fn string_items(snapshot: &Snapshot, slot: &Slot, name: &str) -> Vec<(String, &'static str, &'static str)> {
    let sprites: Vec<String> = snapshot.index.data.sprites.iter().map(|s| s.name.clone()).collect();
    let with = |first: &[&str]| first.iter().map(|s| s.to_string()).chain(sprites.iter().cloned()).collect::<Vec<_>>();
    let sprite = slot.kind.sprite().unwrap_or("");
    let target = || -> (Vec<String>, Vec<String>) {
        if sprite == STAGE {
            (snapshot.index.data.stage.backdrops.clone(), snapshot.index.data.stage.sounds.clone())
        } else {
            snapshot.index.sprite(sprite).map(|s| (s.costumes.clone(), s.sounds.clone())).unwrap_or_default()
        }
    };
    let enumerate = |values: &[&str]| values.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (values, kind, detail): (Vec<String>, &str, &str) = match name {
        "broadcasts" => (snapshot.index.data.broadcasts.clone(), "Event", "Broadcast message"),
        "backdrops" => (snapshot.index.data.stage.backdrops.clone(), "File", "Backdrop"),
        "costumes" => (target().0, "Color", "Costume"),
        "sounds" => (target().1, "Event", "Sound"),
        "keys" => (data::KEY_NAMES.iter().map(|s| s.to_string()).collect(), "Enum", "Key name"),
        "targets" => (with(&["_mouse_", "_random_"]), "Class", "Sprite / target"),
        "mouseTargets" => (with(&["_mouse_"]), "Class", "Sprite / mouse"),
        "ofTargets" => (with(&["_stage_"]), "Class", "Sprite / stage"),
        "touchTargets" => (with(&["_edge_", "_mouse_"]), "Class", "Sprite / edge / mouse"),
        "clones" => (with(&["_myself_"]), "Class", "Sprite / self"),
        "effects" => (enumerate(&["color", "fisheye", "whirl", "pixelate", "mosaic", "brightness", "ghost"]), "Enum", "Visual effect"),
        "soundEffects" => (enumerate(&["PITCH", "PAN LEFT/RIGHT"]), "Enum", "Sound effect"),
        "rotationStyles" => (enumerate(&["all around", "left-right", "don't rotate"]), "Enum", "Rotation style"),
        "dragModes" => (enumerate(&["draggable", "not draggable"]), "Enum", "Drag mode"),
        "penParams" => (enumerate(&["color", "saturation", "brightness", "transparency"]), "Enum", "Pen color parameter"),
        "timeUnits" => (enumerate(&["year", "month", "date", "day", "hour", "minute", "second"]), "Enum", "Time unit"),
        "sortDirections" => (enumerate(&["desc"]), "Enum", "Sort direction"),
        _ => (Vec::new(), "Text", ""),
    };
    values.into_iter().map(|v| (v, kind, detail)).collect()
}

fn header_names(snapshot: &Snapshot) -> Vec<String> {
    let mut names: Vec<String> = snapshot.headers.headers.keys().cloned().collect();
    names.sort();
    names
}

fn project_block_call(proccode: &str) -> (String, String, String) {
    let name = js::trim(proccode.split(" %").next().unwrap_or_default()).to_string();
    let args = proccode.as_bytes().windows(2).filter(|w| w[0] == b'%' && matches!(w[1], b's' | b'n' | b'b')).count();
    let params: Vec<String> = (1..=args).map(|i| format!("arg{i}")).collect();
    (name.clone(), format!("{name}({})", params.join(", ")), snippet_call(&name, &params))
}

fn items(snapshot: &Snapshot, slot: &Slot, line: u32, col: u32, trigger: Option<&str>) -> Vec<Item> {
    let line_text = crate::text::line_text(&slot.text, line - 1);
    let word = word_until(line_text, col);
    let range = (word.start, word.end);
    let prefix = js::utf16_prefix(line_text, col as usize - 1);
    let line_above = |n: u32| (line > n).then(|| crate::text::line_text(&slot.text, line - n - 1).to_string());
    let ctx = completion_context(prefix, &line_above);
    let item = |label: String, kind: &'static str| Item { label, kind, range, ..Default::default() };

    match &ctx {
        Ctx::Comment | Ctx::Member => return Vec::new(),
        Ctx::Modifier(candidates) => {
            return candidates
                .iter()
                .map(|name| {
                    let (detail, doc) = data::modifier_doc(name).unwrap_or_default();
                    Item {
                        detail: Some(detail.into()),
                        documentation: Some(doc.into()),
                        insert_text: name.to_string(),
                        command: Some(""),
                        ..item(name.to_string(), "Keyword")
                    }
                })
                .collect();
        }
        Ctx::Include => {
            let after: Vec<u16> = line_text.encode_utf16().skip(word.end as usize - 1).take(1).collect();
            let closed = after.first() == Some(&(b'>' as u16));
            return header_names(snapshot)
                .into_iter()
                .map(|name| Item {
                    detail: Some("Header".into()),
                    insert_text: if closed { name.clone() } else { format!("{name}>") },
                    command: Some(""),
                    ..item(name, "File")
                })
                .collect();
        }
        Ctx::General { after_lt: true, .. } if word.text.is_empty() && trigger == Some("<") => return Vec::new(),
        _ => {}
    }

    let analysis = slot.analysis();
    let visible = visible_symbols(&analysis, line);

    if let Ctx::Routine { with_call } = ctx {
        let mut out: Vec<Item> = visible
            .iter()
            .map(|id| analysis.symbol(*id))
            .filter(|s| s.kind == SymKind::Routine)
            .map(|s| {
                let name = s.name.text().into_owned();
                let params: Vec<String> = s.params().iter().map(|p| p.text().into_owned()).collect();
                let insert = if with_call { snippet_call(&name, &params) } else { name.clone() };
                Item {
                    detail: Some("scratchroutine".into()),
                    filter_text: Some(name.clone()),
                    insert_text: insert,
                    snippet: true,
                    ..item(format!("{name}({})", params.join(", ")), "Function")
                }
            })
            .collect();
        with_behavior(&mut out);
        return out;
    }

    if let Ctx::StructField { name } = &ctx {
        let fields = struct_fields(&slot.text).into_iter().find(|(n, _)| n == name).map(|(_, f)| f).unwrap_or_else(|| {
            let prefix = format!("{name}.");
            project_variables(snapshot, slot).iter().filter_map(|v| v.name.strip_prefix(&prefix).map(str::to_string)).collect()
        });
        return fields
            .into_iter()
            .map(|f| Item {
                detail: Some(format!("Struct field · {name}")),
                insert_text: format!("{f}]"),
                command: Some(""),
                ..item(format!("{name}.{f}]"), "Field")
            })
            .collect();
    }

    if ctx == Ctx::VarName {
        let mut out = Vec::new();
        for id in &visible {
            let s = analysis.symbol(*id);
            if !matches!(s.kind, SymKind::Param | SymKind::LoopVar) {
                continue;
            }
            let name = s.name.text().into_owned();
            let detail = match &s.meta {
                SymMeta::Param { owner } => format!("Parameter · {}", if js_truthy(owner) { owner.text().into_owned() } else { "custom block".into() }),
                _ if s.kind == SymKind::Param => "Parameter · custom block".into(),
                _ => "Loop variable".into(),
            };
            out.push(Item {
                detail: Some(detail),
                insert_text: format!("{name}]"),
                sort_text: Some(format!("0_{name}")),
                command: Some(""),
                ..item(format!("{name}]"), "Variable")
            });
        }
        for (s, fields) in struct_fields(&slot.text) {
            for f in fields {
                out.push(Item {
                    detail: Some(format!("Struct field · {s}")),
                    insert_text: format!("{s}.{f}]"),
                    sort_text: Some(format!("1_{s}_{f}")),
                    command: Some(""),
                    ..item(format!("{s}.{f}]"), "Field")
                });
            }
        }
        for v in project_variables(snapshot, slot) {
            let is_list = v.kind == VarKind::List;
            out.push(Item {
                detail: Some(if is_list { "List" } else { "Variable" }.into()),
                insert_text: format!("{}]", v.name),
                sort_text: Some(format!("2_{}", v.name)),
                command: Some(""),
                ..item(format!("{}]", v.name), if is_list { "Enum" } else { "Variable" })
            });
        }
        if !out.is_empty() {
            return out;
        }
    }

    if ctx == Ctx::ListMethod {
        return KEYS_STATIC
            .iter()
            .map(|(label, insert, detail, _)| Item {
                detail: Some(detail.to_string()),
                insert_text: insert.to_string(),
                snippet: true,
                command: Some(""),
                ..item(label.to_string(), "Method")
            })
            .collect();
    }
    if ctx == Ctx::PenMethod {
        return PEN_METHODS
            .iter()
            .map(|(label, insert, detail)| Item {
                detail: Some(detail.to_string()),
                insert_text: insert.to_string(),
                snippet: true,
                command: Some(""),
                ..item(label.to_string(), "Method")
            })
            .collect();
    }
    if let Ctx::String { quote_col, slot: string_slot } = &ctx {
        let after: Vec<u16> = line_text.encode_utf16().skip(col as usize - 1).collect();
        let closing = after.iter().position(|u| *u == b'"' as u16);
        let range = (quote_col + 1, closing.map_or(col, |c| col + c as u32));
        return string_slot
            .map(|name| string_items(snapshot, slot, name))
            .unwrap_or_default()
            .into_iter()
            .map(|(value, kind, detail)| Item {
                label: value.clone(),
                kind,
                detail: Some(detail.into()),
                insert_text: value,
                range,
                command: Some(""),
                ..Default::default()
            })
            .collect();
    }

    if let Some(asm) = asm_context(&slot.text, line, col) {
        return match asm {
            AsmCtx::Opcode => data::ASM_OPCODES
                .iter()
                .map(|(op, params)| Item {
                    detail: Some(format!("__asm__ opcode · {}", params.iter().map(|p| p.name).collect::<Vec<_>>().join(", "))),
                    insert_text: format!("{op}($0)"),
                    snippet: true,
                    command: Some(""),
                    ..item(op.to_string(), "Function")
                })
                .collect(),
            AsmCtx::Register => registers_in_scope(snapshot, slot, line, col)
                .into_iter()
                .map(|(name, source)| Item { detail: Some(source), insert_text: name.clone(), command: Some(""), ..item(name, "Variable") })
                .collect(),
        };
    }

    let on_col = match ctx {
        Ctx::General { on_col, .. } => on_col,
        _ => None,
    };
    let hat_range = on_col.map_or(range, |c| (c, range.1));
    let mut out: Vec<Item> = data::STATIC_ITEMS
        .iter()
        .map(|s| Item { kind: s.kind, snippet: s.snippet, range: if s.hat_range { hat_range } else { range }, statik: Some(s), ..Default::default() })
        .collect();
    let mut index_vars: Vec<Item> = Vec::new();
    let var_item = |v: &crate::index::VarInfo, detail: String| Item {
        detail: Some(detail),
        insert_text: format!("[{}]", v.name),
        snippet: true,
        ..item(format!("[{}]", v.name), if v.kind == VarKind::List { "Enum" } else { "Variable" })
    };
    for v in &snapshot.index.data.global_variables {
        index_vars.push(var_item(v, format!("Global {}", v.kind.js())));
    }
    if let Some(sprite) = slot.kind.sprite().filter(|s| !s.is_empty() && *s != STAGE) {
        for v in snapshot.index.sprite_vars(sprite) {
            index_vars.push(var_item(v, format!("{sprite} {}", v.kind.js())));
        }
    }
    with_behavior(&mut index_vars);
    out.extend(index_vars);
    let mut defined: Vec<String> = Vec::new();
    let mut dynamic = Vec::new();
    for id in &visible {
        let s = analysis.symbol(*id);
        let name = s.name.text().into_owned();
        match s.kind {
            SymKind::Param | SymKind::LoopVar => {
                let detail = match &s.meta {
                    SymMeta::Param { owner } => format!("Parameter · {}", if js_truthy(owner) { owner.text().into_owned() } else { "custom block".into() }),
                    _ => "Loop variable".into(),
                };
                dynamic.push(Item {
                    detail: Some(detail),
                    insert_text: format!("[{name}]"),
                    sort_text: Some(format!("0_{name}")),
                    ..item(format!("[{name}]"), "Variable")
                });
            }
            SymKind::EnumMember => {
                let value = match &s.meta {
                    SymMeta::Enum { value: EnumValue::Num(n) } => js::number_to_string(*n),
                    SymMeta::Enum { value: EnumValue::Str(v) } => js::json_quote(v),
                    _ => "?".into(),
                };
                dynamic.push(Item { detail: Some(format!("enum constant = {value}")), insert_text: name.clone(), ..item(name, "Constant") });
            }
            SymKind::Define | SymKind::Routine => {
                let params: Vec<String> = s.params().iter().map(|p| p.text().into_owned()).collect();
                let snippet = if params.is_empty() { format!("{name}()") } else { snippet_call(&name, &params) };
                let label = format!("{name}({})", params.join(", "));
                if s.kind == SymKind::Define {
                    defined.push(name.clone());
                    dynamic.push(Item { detail: Some("Custom block (define)".into()), insert_text: snippet, snippet: true, ..item(label, "Function") });
                } else {
                    dynamic.push(Item {
                        detail: Some("scratchroutine".into()),
                        insert_text: format!("launch {snippet}"),
                        snippet: true,
                        ..item(format!("launch {label}"), "Function")
                    });
                    dynamic.push(Item {
                        detail: Some("scratchroutine".into()),
                        insert_text: format!("await {snippet}"),
                        snippet: true,
                        ..item(format!("await {label}"), "Function")
                    });
                }
            }
            _ => {}
        }
    }
    if let FileKind::Sprite(sprite) = &slot.kind {
        for proccode in snapshot.index.data.custom_blocks.get(&**sprite).map(Vec::as_slice).unwrap_or(&[]) {
            let (name, label, snippet) = project_block_call(proccode);
            if defined.contains(&name) {
                continue;
            }
            dynamic.push(Item { detail: Some("Custom block (project)".into()), insert_text: snippet, snippet: true, ..item(label, "Function") });
        }
    }
    with_behavior(&mut dynamic);
    out.extend(dynamic);
    out
}

fn write_item(out: &mut String, item: &Item, line: u32, follow_ups: bool) {
    use std::fmt::Write;
    out.push_str("{\"label\":");
    push_json_str(out, item.label());
    let _ = write!(out, ",\"kind\":{},\"insertTextFormat\":{}", kind_number(item.kind), if item.snippet { 2 } else { 1 });
    let _ = write!(
        out,
        ",\"textEdit\":{{\"range\":{{\"start\":{{\"line\":{l},\"character\":{}}},\"end\":{{\"line\":{l},\"character\":{}}}}},\"newText\":",
        item.range.0 - 1,
        item.range.1 - 1,
        l = line - 1
    );
    push_json_str(out, item.insert_text());
    out.push('}');
    if let Some(detail) = item.detail() {
        out.push_str(",\"detail\":");
        push_json_str(out, detail);
    }
    if let Some(doc) = item.documentation() {
        out.push_str(",\"documentation\":{\"kind\":\"markdown\",\"value\":");
        push_json_str(out, doc);
        out.push('}');
    }
    if let Some(sort) = &item.sort_text {
        out.push_str(",\"sortText\":");
        push_json_str(out, sort);
    }
    if let Some(filter) = item.filter_text() {
        out.push_str(",\"filterText\":");
        push_json_str(out, filter);
    }
    if let Some(command) = item.command().filter(|_| follow_ups) {
        let title = if command.ends_with("Suggest") { "Suggest" } else { "Parameter hints" };
        let _ = write!(out, ",\"command\":{{\"title\":\"{title}\",\"command\":\"{command}\"}}");
    }
    out.push('}');
}

pub fn completion(snapshot: &Snapshot, params: &Value) -> Result<String, (i64, String)> {
    let Some(pos) = super::position(params) else { return Ok("null".into()) };
    let slot = slot_of(snapshot, pos.uri)?;
    let trigger = params["context"]["triggerCharacter"].as_str();
    let follow_ups = snapshot.settings.completion_follow_ups;
    let list = items(snapshot, slot, pos.line, pos.col, trigger);
    let mut out = String::with_capacity(list.len() * 256);
    out.push_str("{\"isIncomplete\":false,\"items\":[");
    for (i, item) in list.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_item(&mut out, item, pos.line, follow_ups);
    }
    out.push_str("]}");
    Ok(out)
}

pub fn debug_items(snapshot: &Snapshot, slot: &Slot, line: u32, col: u32, trigger: Option<&str>) -> Vec<Value> {
    items(snapshot, slot, line, col, trigger)
        .into_iter()
        .map(|i| json!([i.label(), i.kind, i.detail(), i.insert_text(), i.range.0, i.range.1, i.filter_text(), i.sort_text, i.command()]))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn json_strings_match_serde() {
        for s in ["plain", "quote \" and \\ backslash", "line\nbreak\ttab\r", "\u{1}\u{1f}\u{7f}", "雪🐈\u{2028}", "${1:x}", ""] {
            let mut out = String::new();
            super::push_json_str(&mut out, s);
            assert_eq!(out, serde_json::to_string(s).unwrap());
        }
    }
}
