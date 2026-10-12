use rustc_hash::FxHashMap;

use super::completion::Item;
use super::lexical::word_until;
use crate::analyzer::Analysis;
use crate::data;
use crate::js;
use crate::lexer::{Tk, Token, tokenize};
use crate::workspace::{FileKind, Slot};

const BLOCK_KEYWORDS_EXPECTING_BOOLEAN: &[&str] = &["if", "elif", "while", "until", "not", "and", "or"];
const WORDS_EXPECTING_NUMBER: &[&str] = &["repeat", "by", "from", "wait", "mod"];
const WORDS_EXPECTING_VALUE: &[&str] = &[
    "if", "elif", "while", "until", "repeat", "return", "set", "change", "to", "by", "from", "in", "for", "pyfor", "match", "switch", "case", "and", "or",
    "not", "mod", "wait",
];
const OPERAND_ENDING_RESERVED: &[&str] = &["forever", "else", "do", "break", "continue", "breakpoint"];

const NUMBER_PARAMS: &[&str] =
    &["steps", "degrees", "x", "y", "dx", "dy", "secs", "seconds", "size", "percent", "amount", "n", "count", "index", "start", "end"];
const TEXT_PARAMS: &[&str] = &["message", "text", "question", "string", "str"];

const NUMBER_VALUES: &[&str] = &[
    "abs",
    "round",
    "sqrt",
    "floor",
    "ceiling",
    "ceil",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "ln",
    "log",
    "exp",
    "pow10",
    "random",
    "clamp",
    "length",
    "xPos",
    "yPos",
    "direction",
    "timer",
    "mouseX",
    "mouseY",
    "loudness",
    "costumeNum",
    "volume",
    "daysSince2000",
    "distanceTo",
    "xOf",
    "yOf",
    "directionOf",
    "costumeNumOf",
    "sizeOf",
    "volumeOf",
    "alloc",
];
const TEXT_VALUES: &[&str] = &["join", "letterOf", "answer", "username", "costumeName", "costumeNameOf", "currentTime", "attributeOf"];
const BOOLEAN_VALUES: &[&str] = &["touching", "key", "mouseDown", "isRunning", "not", "true", "false", "contains"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Top,
    Hat,
    Statement,
    Expression,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Any,
    Number,
    Text,
    Boolean,
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Block {
    Hat,
    Define { returns: bool },
    Routine,
    If,
    Else,
    Loop,
    Match,
    Case,
    Declaration,
    Plain,
}

#[derive(Clone, Debug)]
pub struct Situation {
    pub place: Place,
    pub want: Want,
    pub after_operand: bool,
    pub after_if_block: bool,
    pub only_includes_so_far: bool,
    blocks: Vec<Block>,
}

impl Situation {
    pub fn in_loop(&self) -> bool {
        self.blocks.contains(&Block::Loop)
    }

    pub fn in_routine(&self) -> bool {
        self.blocks.contains(&Block::Routine)
    }

    pub fn in_returning_define(&self) -> bool {
        self.blocks.iter().rev().find_map(|b| if let Block::Define { returns } = b { Some(*returns) } else { None }).unwrap_or(false)
    }

    pub fn directly_in_match(&self) -> bool {
        self.blocks.last() == Some(&Block::Match)
    }
}

fn word<'a>(src: &'a str, t: &'a Token) -> Option<&'a str> {
    matches!(t.kind, Tk::Kw | Tk::Ident).then(|| t.text(src))
}

fn block_kind(src: &str, header: &[Token]) -> Block {
    for t in header.iter().rev() {
        match word(src, t) {
            Some("if" | "elif") => return Block::If,
            Some("else") => return Block::Else,
            Some("forever" | "repeat" | "while" | "until" | "for" | "pyfor" | "do") => return Block::Loop,
            Some("on") => return Block::Hat,
            Some("define") => return Block::Define { returns: header.iter().any(|t| word(src, t) == Some("returns")) },
            Some("scratchroutine") => return Block::Routine,
            Some("match" | "switch") => return Block::Match,
            Some("case" | "default") => return Block::Case,
            Some("struct" | "enum" | "enums") => return Block::Declaration,
            _ => {}
        }
    }
    Block::Plain
}

fn want_from_param(name: &str) -> Want {
    let first = name.split(|c: char| !c.is_ascii_alphanumeric()).find(|w| !w.is_empty()).unwrap_or("");
    if name.contains("cond") {
        Want::Boolean
    } else if name.contains("list") {
        Want::List
    } else if NUMBER_PARAMS.contains(&first) {
        Want::Number
    } else if TEXT_PARAMS.contains(&first) {
        Want::Text
    } else {
        Want::Any
    }
}

fn want_inside_call(src: &str, toks: &[Token], open: usize) -> Want {
    let Some(callee) = open.checked_sub(1).and_then(|i| word(src, &toks[i])) else { return Want::Any };
    if BLOCK_KEYWORDS_EXPECTING_BOOLEAN.contains(&callee) {
        return Want::Boolean;
    }
    let mut depth = 0u32;
    let mut arg = 0usize;
    for t in &toks[open + 1..] {
        match t.kind {
            Tk::LParen => depth += 1,
            Tk::RParen => depth = depth.saturating_sub(1),
            Tk::Comma if depth == 0 => arg += 1,
            _ => {}
        }
    }
    data::sig(callee).and_then(|sig| sig.params.get(arg)).map_or(Want::Any, |(name, _)| want_from_param(name))
}

fn drop_include_lines(src: &str, toks: Vec<Token>) -> Vec<Token> {
    let mut include_lines = Vec::new();
    for pair in toks.windows(2) {
        if pair[0].kind == Tk::Hash && pair[1].line == pair[0].line && pair[1].text(src) == "include" {
            include_lines.push(pair[0].line);
        }
    }
    toks.into_iter().filter(|t| !include_lines.contains(&t.line)).collect()
}

fn ends_postfix_operator(toks: &[Token]) -> bool {
    let [.., before, tail] = toks else { return false };
    matches!(tail.kind, Tk::Plus | Tk::Minus) && before.kind == tail.kind && before.end_col == tail.col && before.end_line == tail.line
}

pub fn situation(head: &str, cursor_line: u32) -> Situation {
    let all = tokenize(head, false);
    let toks = drop_include_lines(head, all.into_iter().filter(|t| t.kind != Tk::Eof).collect());
    let only_includes_so_far = toks.is_empty();
    let mut blocks: Vec<Block> = Vec::new();
    let mut parens: Vec<usize> = Vec::new();
    let mut header_start = 0;
    let mut closed = None;
    for (i, t) in toks.iter().enumerate() {
        match t.kind {
            Tk::LParen => parens.push(i),
            Tk::RParen => {
                parens.pop();
            }
            Tk::LBrace => {
                blocks.push(block_kind(head, &toks[header_start..i]));
                header_start = i + 1;
                parens.clear();
            }
            Tk::RBrace => {
                closed = blocks.pop();
                header_start = i + 1;
                parens.clear();
            }
            _ => {}
        }
    }
    let base = if blocks.is_empty() { Place::Top } else { Place::Statement };
    let mut situation = Situation { place: base, want: Want::Any, after_operand: false, after_if_block: false, only_includes_so_far, blocks };
    let Some(tail) = toks.last() else { return situation };
    if let Some(&open) = parens.last() {
        situation.place = Place::Expression;
        situation.want = want_inside_call(head, &toks, open);
        return situation;
    }
    match tail.kind {
        Tk::LBrace | Tk::Semi | Tk::Hash => {}
        Tk::RBrace => situation.after_if_block = closed == Some(Block::If),
        Tk::Plus
        | Tk::Minus
        | Tk::Star
        | Tk::Slash
        | Tk::Lt
        | Tk::Gt
        | Tk::Eq
        | Tk::Le
        | Tk::Ge
        | Tk::Neq
        | Tk::Bang
        | Tk::Question
        | Tk::Colon
        | Tk::Amp
        | Tk::Comma
            if !ends_postfix_operator(&toks) =>
        {
            situation.place = Place::Expression;
            situation.want = match tail.kind {
                Tk::Plus | Tk::Minus | Tk::Star | Tk::Slash | Tk::Lt | Tk::Gt | Tk::Le | Tk::Ge => Want::Number,
                Tk::Bang => Want::Boolean,
                _ => Want::Any,
            };
        }
        Tk::Kw | Tk::Ident => {
            let text = tail.text(head);
            if text == "on" {
                situation.place = Place::Hat;
            } else if WORDS_EXPECTING_VALUE.contains(&text) {
                situation.place = Place::Expression;
                situation.want = if BLOCK_KEYWORDS_EXPECTING_BOOLEAN.contains(&text) {
                    Want::Boolean
                } else if WORDS_EXPECTING_NUMBER.contains(&text) {
                    Want::Number
                } else {
                    Want::Any
                };
            } else {
                situation.after_operand = !OPERAND_ENDING_RESERVED.contains(&text) && tail.end_line == cursor_line;
            }
        }
        _ => situation.after_operand = tail.end_line == cursor_line,
    }
    situation
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Value {
    Number,
    Text,
    Boolean,
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Local,
    Constant,
    UserVar { list: bool },
    UserBlock,
    Routine,
    Hat,
    Declaration,
    Include,
    Control,
    LoopControl,
    CaseLabel,
    Statement,
    Value(Value),
    Comparison,
}

fn stem(label: &str) -> &str {
    let label = label.strip_prefix("launch ").or_else(|| label.strip_prefix("await ")).unwrap_or(label);
    let end = label.find(['(', ' ', '{']).unwrap_or(label.len());
    &label[..end]
}

fn role(item: &Item) -> Role {
    let label = item.label();
    let detail = item.detail().unwrap_or("");
    if detail.starts_with("Parameter") || detail == "Loop variable" {
        return Role::Local;
    }
    if detail.starts_with("enum constant") {
        return Role::Constant;
    }
    if detail.starts_with("Custom block (") {
        return Role::UserBlock;
    }
    if detail == "scratchroutine" {
        return Role::Routine;
    }
    if item.statik.is_none() && label.starts_with('[') && matches!(item.kind, "Variable" | "Enum") {
        return Role::UserVar { list: item.kind == "Enum" };
    }
    if detail.starts_with("Hat block") {
        return Role::Hat;
    }
    if label.starts_with("#include") {
        return Role::Include;
    }
    if detail.starts_with("Custom block ·") || detail.starts_with("Enum ·") || detail.starts_with("Struct ·") || label == "scratchroutine" {
        return Role::Declaration;
    }
    match label {
        "break" | "continue" => return Role::LoopControl,
        "case" | "default" => return Role::CaseLabel,
        _ => {}
    }
    if detail.starts_with("Control ·") {
        return Role::Control;
    }
    if item.kind == "Operator" {
        return Role::Comparison;
    }
    let name = stem(label);
    if label.starts_with("[list].") || label == "? :" || label.starts_with("&[") || label.starts_with("*[") || label == "[ptr][i]" {
        return Role::Value(if label.starts_with("[list].") { Value::Number } else { Value::Any });
    }
    if NUMBER_VALUES.contains(&name) {
        Role::Value(Value::Number)
    } else if TEXT_VALUES.contains(&name) {
        Role::Value(Value::Text)
    } else if BOOLEAN_VALUES.contains(&name) {
        Role::Value(Value::Boolean)
    } else {
        Role::Statement
    }
}

fn value_matches(value: Value, want: Want) -> bool {
    matches!((value, want), (Value::Number, Want::Number) | (Value::Text, Want::Text) | (Value::Boolean, Want::Boolean))
}

struct Usage<'a> {
    names: FxHashMap<&'a str, u32>,
    variables: FxHashMap<&'a str, u32>,
}

impl<'a> Usage<'a> {
    fn of(analysis: &'a Analysis) -> Usage<'a> {
        let mut usage = Usage { names: FxHashMap::default(), variables: FxHashMap::default() };
        for t in &analysis.tokens {
            let counts = match t.kind {
                Tk::Kw | Tk::Ident => &mut usage.names,
                Tk::Var => &mut usage.variables,
                _ => continue,
            };
            *counts.entry(t.text(&analysis.src)).or_insert(0) += 1;
        }
        usage
    }

    fn count(&self, item: &Item, role: Role) -> u32 {
        let label = item.label();
        match role {
            Role::UserVar { .. } | Role::Local => self.variables.get(label.trim_start_matches('[').trim_end_matches(']')).copied().unwrap_or(0),
            Role::Hat | Role::Declaration | Role::Include | Role::Comparison => 0,
            _ => self.names.get(stem(label)).copied().unwrap_or(0),
        }
    }
}

fn score(role: Role, item: &Item, situation: &Situation, file: &FileKind, used: u32) -> u32 {
    let in_header_file = matches!(file, FileKind::Header(_));
    let want = situation.want;
    let base: u32 = match situation.place {
        Place::Hat => {
            if role == Role::Hat {
                0
            } else {
                90
            }
        }
        Place::Top => match role {
            Role::Declaration => 0,
            Role::Include => {
                if situation.only_includes_so_far {
                    0
                } else {
                    3
                }
            }
            Role::Hat => {
                if in_header_file {
                    90
                } else {
                    0
                }
            }
            Role::Statement | Role::Control | Role::UserBlock | Role::Routine => {
                if in_header_file {
                    60
                } else {
                    50
                }
            }
            _ => 70,
        },
        Place::Statement => match role {
            Role::LoopControl => {
                if situation.in_loop() {
                    4
                } else {
                    50
                }
            }
            Role::CaseLabel => {
                if situation.directly_in_match() {
                    2
                } else {
                    55
                }
            }
            Role::Statement if item.label() == "checkCancel()" && situation.in_routine() => 4,
            Role::Statement | Role::Control | Role::UserBlock | Role::Routine => 10,
            Role::Local | Role::UserVar { .. } => 40,
            Role::Comparison if situation.after_operand => 5,
            Role::Comparison | Role::Value(_) | Role::Constant => 60,
            Role::Hat | Role::Declaration | Role::Include => 80,
        },
        Place::Expression => match role {
            Role::Local => 0,
            Role::UserVar { list } => {
                let variable = if used > 0 { 10 } else { 20 };
                match (want, list) {
                    (Want::List, true) => 5,
                    (Want::List, false) => 40,
                    (Want::Boolean, _) => variable + 10,
                    (_, true) => variable + 6,
                    _ => variable,
                }
            }
            Role::Constant => 14,
            Role::Value(value) => {
                let literal = if item.kind == "Keyword" { 2 } else { 0 };
                let reporter = if item.kind == "Variable" { 1 } else { 0 };
                literal
                    + match want {
                        Want::Any => 16 - 2 * reporter,
                        _ if value_matches(value, want) => 8 - reporter,
                        _ => 28,
                    }
            }
            Role::Comparison => match want {
                Want::Boolean => 6,
                Want::Any => 18,
                _ => 30,
            },
            Role::UserBlock => 22,
            Role::Statement | Role::Control | Role::LoopControl | Role::CaseLabel | Role::Routine => 65,
            Role::Hat | Role::Declaration | Role::Include => 90,
        },
    };
    if base >= 5 { base.saturating_sub(used.min(9)).max(5) } else { base }
}

fn contextual_items(situation: &Situation, range: (u32, u32)) -> Vec<Item> {
    let snippet = |label: &str, kind: &'static str, detail: &str, insert: &str, filter: &str| Item {
        label: label.into(),
        kind,
        detail: Some(detail.into()),
        insert_text: insert.into(),
        snippet: true,
        filter_text: Some(filter.into()),
        command: Some(""),
        range,
        ..Default::default()
    };
    let mut out = Vec::new();
    if situation.place == Place::Statement && situation.after_if_block {
        out.push(snippet("else {}", "Snippet", "Control · else", "else {\n\t$0\n}", "else"));
        out.push(snippet("else if {}", "Snippet", "Control · else if <cond> {}", "else if $1 {\n\t$0\n}", "else if"));
    }
    if situation.place == Place::Statement && situation.in_returning_define() {
        out.push(snippet("return value", "Keyword", "Custom block · return <expr>", "return $0", "return"));
    }
    out
}

pub(super) fn rank(list: &mut Vec<Item>, slot: &Slot, line: u32, col: u32) {
    if !list.iter().any(|i| i.statik.is_some()) {
        return;
    }
    let line_text = crate::text::line_text(&slot.text, line - 1);
    let word = word_until(line_text, col);
    let line_start: usize = slot.text.split('\n').take(line as usize - 1).map(|l| l.len() + 1).sum();
    let head = &slot.text[..line_start + js::utf16_prefix(line_text, word.start as usize - 1).len()];
    let situation = situation(head, line);
    let analysis = slot.analysis();
    let usage = Usage::of(&analysis);
    let mut extra = contextual_items(&situation, (word.start, word.end));
    for (index, item) in extra.iter_mut().enumerate() {
        item.sort_text = Some(format!("000{index:04}"));
    }
    for (index, item) in list.iter_mut().enumerate() {
        let role = role(item);
        let rank = score(role, item, &situation, &slot.kind, usage.count(item, role));
        item.sort_text = Some(format!("{rank:03}{:04}", index.min(9999)));
    }
    extra.append(list);
    *list = extra;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_end(text: &str) -> Situation {
        situation(text, text.split('\n').count() as u32)
    }

    #[test]
    fn top_level_is_for_declarations() {
        assert_eq!(at_end("").place, Place::Top);
        assert_eq!(at_end("on flag {\n}\n").place, Place::Top);
        assert!(at_end("#include <a.h>\n").only_includes_so_far);
    }

    #[test]
    fn block_bodies_are_statement_positions() {
        let s = at_end("on flag {\n    forever {\n        ");
        assert_eq!(s.place, Place::Statement);
        assert!(s.in_loop());
        assert!(!at_end("on flag {\n    ").in_loop());
    }

    #[test]
    fn expression_positions() {
        assert_eq!(at_end("on flag {\n    set [x] to ").place, Place::Expression);
        assert_eq!(at_end("on flag {\n    say(").place, Place::Expression);
        assert_eq!(at_end("on flag {\n    say(join(\"a\", ").place, Place::Expression);
        assert_eq!(at_end("on flag {\n    [x] = [y] + ").place, Place::Expression);
    }

    #[test]
    fn increments_end_statements() {
        assert_eq!(at_end("on flag {\n    [x]++\n    ").place, Place::Statement);
        assert_eq!(at_end("on flag {\n    [x]++ ").place, Place::Statement);
    }

    #[test]
    fn wanted_value_follows_context() {
        assert_eq!(at_end("on flag {\n    if ").want, Want::Boolean);
        assert_eq!(at_end("on flag {\n    while (").want, Want::Boolean);
        assert_eq!(at_end("on flag {\n    move(").want, Want::Number);
        assert_eq!(at_end("on flag {\n    say(").want, Want::Text);
        assert_eq!(at_end("on flag {\n    repeat ").want, Want::Number);
        assert_eq!(at_end("on flag {\n    set [x] to ").want, Want::Any);
        assert_eq!(at_end("on flag {\n    [x] + ").want, Want::Number);
        assert_eq!(at_end("on flag {\n    listAdd(1, ").want, Want::List);
        assert_eq!(at_end("on flag {\n    listAdd(").want, Want::Any);
    }

    #[test]
    fn else_follows_a_closed_if() {
        assert!(at_end("on flag {\n    if [x] {\n    }\n    ").after_if_block);
        assert!(at_end("on flag {\n    if [x] {\n    } else if [y] {\n    }\n    ").after_if_block);
        assert!(!at_end("on flag {\n    forever {\n    }\n    ").after_if_block);
        assert!(!at_end("on flag {\n    if [x] {\n    } else {\n    }\n    ").after_if_block);
    }

    #[test]
    fn returns_and_routines_scope_their_keywords() {
        assert!(at_end("define f(a) returns {\n    ").in_returning_define());
        assert!(!at_end("define f(a) {\n    ").in_returning_define());
        assert!(at_end("scratchroutine r() {\n    ").in_routine());
        assert!(at_end("on flag {\n    match [x] {\n        ").directly_in_match());
    }

    #[test]
    fn after_on_wants_a_trigger() {
        assert_eq!(at_end("on ").place, Place::Hat);
    }
}
