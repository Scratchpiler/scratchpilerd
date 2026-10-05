use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::ast::StmtKind;
use crate::js;
use crate::lexer::tokenize;
use crate::parser::{ParseError, parse};

pub trait Headers: Sync {
    fn header(&self, name: &str) -> Option<Arc<str>>;
}

pub struct NoHeaders;

impl Headers for NoHeaders {
    fn header(&self, _: &str) -> Option<Arc<str>> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub header: String,
    pub include_line: u32,
    pub start_line: u32,
    pub line_count: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Expansion {
    pub text: String,
    pub errors: Vec<ParseError>,
    pub user_line_count: u32,
    pub segments: Vec<Segment>,
    pub header_roots: FxHashMap<String, String>,
    pub includes: Vec<String>,
}

pub fn include_target(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches(js::is_space).strip_prefix("#include")?;
    let rest = rest.trim_start_matches(js::is_space).strip_prefix('<')?;
    let close = rest.find(['<', '>'])?;
    if !rest[close..].starts_with('>') || close == 0 {
        return None;
    }
    let tail = &rest[close + 1..];
    tail.chars().all(js::is_space).then(|| &rest[..close])
}

pub fn is_header_name(name: &str) -> bool {
    name.strip_suffix(".h").is_some_and(|stem| !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
}

pub fn expand(src: &str, headers: &dyn Headers) -> Expansion {
    let mut errors = Vec::new();
    let mut segments = Vec::new();
    let mut header_roots = FxHashMap::default();
    let mut include_order = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let user_lines: Vec<&str> = src.split('\n').collect();
    let user_line_count = user_lines.len() as u32;
    let mut out_user: Vec<&str> = Vec::with_capacity(user_lines.len());
    let mut queue: std::collections::VecDeque<(String, u32, Vec<String>)> = Default::default();

    for (i, line) in user_lines.iter().enumerate() {
        match include_target(line) {
            Some(name) => {
                out_user.push("");
                queue.push_back((name.to_string(), i as u32 + 1, Vec::new()));
            }
            None => out_user.push(line),
        }
    }

    let line_len = |include_line: u32| -> u32 {
        let len = user_lines.get(include_line as usize - 1).map(|l| js::utf16_len(l)).unwrap_or(0);
        if len == 0 { 8 } else { len }
    };
    let mut appended: Vec<String> = Vec::new();
    let mut appended_lines = 0u32;

    while let Some((name, include_line, stack)) = queue.pop_front() {
        let error = |message: String| ParseError { line: include_line, col: 1, len: line_len(include_line), message };
        if !is_header_name(&name) {
            errors.push(error(format!("Invalid header name <{name}> — header names use letters/digits/_/- and end with .h")));
            continue;
        }
        if seen.contains(&name) {
            continue;
        }
        if stack.contains(&name) {
            let mut chain = stack.clone();
            chain.push(name.clone());
            errors.push(error(format!("Include cycle: {}", chain.join(" → "))));
            continue;
        }
        let Some(content) = headers.header(&name) else {
            errors.push(error(format!("Header not found: {name} — create it in the Headers panel first")));
            continue;
        };
        seen.push(name.clone());
        include_order.push(name.clone());

        let mut hdr_out: Vec<&str> = Vec::new();
        for line in content.split('\n') {
            match include_target(line) {
                Some(nested) => {
                    hdr_out.push("");
                    let mut nested_stack = stack.clone();
                    nested_stack.push(name.clone());
                    queue.push_back((nested.to_string(), include_line, nested_stack));
                }
                None => hdr_out.push(line),
            }
        }
        let hdr_text = hdr_out.join("\n");

        let tokens = tokenize(&hdr_text, false);
        let (ast, parse_errors) = parse(&hdr_text, &tokens);
        if !parse_errors.is_empty() {
            for pe in parse_errors {
                errors.push(error(format!("{name}:{}:{}: {}", pe.line, pe.col, pe.message)));
            }
            continue;
        }
        let mut bad = false;
        for block in &ast.blocks {
            if !matches!(block.kind, StmtKind::Define(_) | StmtKind::Routine(_) | StmtKind::Enum { .. } | StmtKind::Struct { .. }) {
                errors.push(error(format!(
                    "{name}:{}: headers may only contain `define`, `scratchroutine`, `enum` and `struct` declarations",
                    block.line.max(1)
                )));
                bad = true;
            }
        }
        if bad {
            continue;
        }
        for block in &ast.blocks {
            match &block.kind {
                StmtKind::Define(d) => {
                    header_roots.insert(d.name.text().into_owned(), name.clone());
                }
                StmtKind::Routine(r) => {
                    header_roots.insert(r.name.to_string(), name.clone());
                }
                _ => {}
            }
        }
        segments.push(Segment { header: name.clone(), include_line, start_line: user_line_count + 1 + appended_lines, line_count: hdr_out.len() as u32 });
        appended_lines += hdr_out.len() as u32;
        appended.push(hdr_text);
    }

    let mut text = out_user.join("\n");
    if !appended.is_empty() {
        text.push('\n');
        text.push_str(&appended.join("\n"));
    }
    Expansion { text, errors, user_line_count, segments, header_roots, includes: include_order }
}

impl Expansion {
    pub fn map(&self, line: u32, col: u32, len: u32, message: &str) -> Option<(u32, u32, u32, String)> {
        if line <= self.user_line_count {
            return None;
        }
        for seg in &self.segments {
            if line >= seg.start_line && line < seg.start_line + seg.line_count {
                let hdr_line = line - seg.start_line + 1;
                return Some((seg.include_line, 1, 200, format!("{}:{}:{}: {}", seg.header, hdr_line, col, message)));
            }
        }
        Some((self.user_line_count, 1, len, message.to_string()))
    }

    pub fn segment_for(&self, line: u32) -> Option<&Segment> {
        self.segments.iter().find(|s| line >= s.start_line && line < s.start_line + s.line_count)
    }
}
