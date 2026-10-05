use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::ast::*;
use crate::data;
use crate::diag::{Category, Diag, Severity};
use crate::index::{ScratchIndex, VarSets};
use crate::js;
use crate::lexer::{Token, tokenize};
use crate::parser::{ParseError, fuzzy_match, parse};
use crate::preprocess::{Expansion, Headers, expand};
use crate::walk::{Node, Val, script_nodes};

pub const REPORTER_BUILTINS: &[&str] = &[
    "xPos",
    "yPos",
    "direction",
    "size",
    "timer",
    "answer",
    "mouseDown",
    "mouseX",
    "mouseY",
    "loudness",
    "costumeNum",
    "costumeName",
    "volume",
    "username",
    "daysSince2000",
    "isRunning",
    "true",
    "false",
];

pub const EXPR_FN_BUILTINS: &[&str] = &[
    "touching",
    "key",
    "round",
    "random",
    "join",
    "letterOf",
    "contains",
    "distanceTo",
    "currentTime",
    "length",
    "attributeOf",
    "clamp",
    "yield",
    "isRunning",
    "abs",
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
    "xOf",
    "yOf",
    "directionOf",
    "costumeNumOf",
    "costumeNameOf",
    "sizeOf",
    "volumeOf",
    "alloc",
];

const BLOCKING_STMTS: &[&str] = &["WaitStmt", "WaitUntilStmt", "GlideStmt", "GlideToStmt", "SayForStmt", "ThinkForStmt", "AwaitStmt"];
const BORING_NUMBERS: &[f64] = &[-1.0, 0.0, 1.0, 2.0, 10.0, 100.0, 180.0, 360.0];

pub fn is_stmt_builtin(name: &str) -> bool {
    data::call_sig(name).is_some() || name == "alloc" || name == "free"
}

fn stmt_builtins() -> impl Iterator<Item = &'static str> {
    data::CALL_SIGS.iter().map(|(k, _)| *k)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SymKind {
    Define,
    Routine,
    EnumMember,
    Struct,
    StructField,
    ProjectVar,
    ProjectList,
    Param,
    LoopVar,
}

impl SymKind {
    pub fn js(self) -> &'static str {
        match self {
            SymKind::Define => "define",
            SymKind::Routine => "routine",
            SymKind::EnumMember => "enumMember",
            SymKind::Struct => "struct",
            SymKind::StructField => "structField",
            SymKind::ProjectVar => "projectVar",
            SymKind::ProjectList => "projectList",
            SymKind::Param => "param",
            SymKind::LoopVar => "loopVar",
        }
    }

    pub fn token_type(self) -> Option<&'static str> {
        match self {
            SymKind::Param => Some("parameter"),
            SymKind::LoopVar => Some("variable"),
            SymKind::Define | SymKind::Routine => Some("function"),
            SymKind::EnumMember => Some("enumMember"),
            SymKind::Struct => Some("type"),
            SymKind::StructField => Some("property"),
            SymKind::ProjectVar | SymKind::ProjectList => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Access {
    Read,
    Write,
    ReadWrite,
}

impl Access {
    pub fn js(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::ReadWrite => "readwrite",
        }
    }
}

pub type SymId = u32;

#[derive(Clone, Debug)]
pub enum SymMeta {
    None,
    Callable { params: Vec<JsVal>, returns: bool },
    Enum { value: EnumValue },
    Struct { fields: Vec<Box<str>> },
    Field { struct_name: Box<str>, field: Box<str>, struct_sym: Option<SymId> },
    Param { owner: JsVal },
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: JsVal,
    pub kind: SymKind,
    pub def_range: Option<Span>,
    pub refs: Vec<(Span, Access)>,
    pub meta: SymMeta,
    pub header_origin: Option<String>,
}

impl Symbol {
    pub fn params(&self) -> &[JsVal] {
        match &self.meta {
            SymMeta::Callable { params, .. } => params,
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Occurrence {
    pub span: Span,
    pub symbol: SymId,
    pub is_def: bool,
    pub is_bracketed: bool,
    pub access: Option<Access>,
}

#[derive(Clone, Debug)]
pub struct Scope {
    pub start_line: u32,
    pub end_line: u32,
    pub symbols: Vec<SymId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guess {
    Variable,
    List,
    Routine,
    Call,
    ExprFn,
    Reporter,
}

#[derive(Clone, Debug)]
pub struct Unresolved {
    pub name: Box<str>,
    pub guess: Guess,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Duplicate {
    pub name: JsVal,
    pub line: u32,
    pub col: u32,
}

pub struct Analysis {
    pub src: Arc<str>,
    pub tokens: Vec<Token>,
    pub ast: Script,
    pub parse_errors: Vec<ParseError>,
    pub symbols: Vec<Symbol>,
    pub doc_keys: Vec<SymId>,
    by_key: FxHashMap<(SymKind, Box<str>), SymId>,
    pub occurrences: Vec<Occurrence>,
    pub scopes: Vec<Scope>,
    pub unresolved: Vec<Unresolved>,
    pub duplicates: Vec<Duplicate>,
    pub sprite: Box<str>,
    pub project_indexed: bool,
    pub expansion: Option<Expansion>,
    pub index: Arc<ScratchIndex>,
}

impl Analysis {
    pub fn lookup(&self, kind: SymKind, name: &str) -> Option<SymId> {
        self.by_key.get(&(kind, Box::from(name))).copied()
    }

    pub fn symbol(&self, id: SymId) -> &Symbol {
        &self.symbols[id as usize]
    }

    pub fn var_sets(&self) -> &VarSets {
        self.index.var_sets(&self.sprite)
    }

    pub fn user_line_count(&self) -> Option<u32> {
        self.expansion.as_ref().map(|e| e.user_line_count)
    }

    pub fn names_of(&self, kind: SymKind) -> Vec<String> {
        self.doc_keys.iter().map(|id| self.symbol(*id)).filter(|s| s.kind == kind).map(|s| s.name.text().into_owned()).collect()
    }

    fn map_diag(&self, mut diag: Diag) -> Diag {
        if let Some(ex) = &self.expansion
            && let Some((line, col, len, message)) = ex.map(diag.line, diag.col, diag.len, &diag.message)
        {
            diag.line = line;
            diag.col = col;
            diag.len = len;
            diag.message = message;
        }
        diag
    }
}

fn span_of_expr(e: &Expr) -> Option<Span> {
    let (line, col) = e.pos();
    let (end_line, end_col) = e.end();
    if end_line != 0 && end_col != 0 {
        return Some(Span::new(line, col, end_line, end_col));
    }
    if line == 0 {
        return None;
    }
    let len = e.name().map(js::utf16_len).filter(|l| *l > 0).unwrap_or(1);
    Some(Span::new(line, col, line, col + len))
}

fn named_span(line: u32, col: u32, name: &str) -> Span {
    let len = js::utf16_len(name);
    Span::new(line, col, line, col + if len == 0 { 1 } else { len })
}

pub fn subtree_max_line(node: Node<'_>) -> u32 {
    let mut max = 0;
    fn walk(node: Node<'_>, max: &mut u32) {
        let mut children = Vec::new();
        node.fields(&mut |key, value| match value {
            Val::Pos(n) if key == "line" || key == "endLine" => *max = (*max).max(n),
            other => crate::walk::push_nodes(other, &mut children),
        });
        for child in children {
            walk(child, max);
        }
    }
    walk(node, &mut max);
    max
}

struct Builder<'a> {
    symbols: Vec<Symbol>,
    doc_keys: Vec<SymId>,
    by_key: FxHashMap<(SymKind, Box<str>), SymId>,
    occurrences: Vec<Occurrence>,
    scopes: Vec<Scope>,
    scope_stack: Vec<Vec<SymId>>,
    unresolved: Vec<Unresolved>,
    duplicates: Vec<Duplicate>,
    sets: &'a VarSets,
    project_indexed: bool,
    top_span: Option<Span>,
}

impl<'a> Builder<'a> {
    fn mk(&mut self, name: JsVal, kind: SymKind, def_range: Option<Span>, meta: SymMeta) -> SymId {
        self.symbols.push(Symbol { name, kind, def_range, refs: Vec::new(), meta, header_origin: None });
        (self.symbols.len() - 1) as SymId
    }

    fn doc_symbol(&mut self, name: &JsVal, kind: SymKind, def: Option<Span>, meta: SymMeta) -> Option<SymId> {
        if name.is("_err_") {
            return None;
        }
        let key = (kind, Box::<str>::from(name.text().as_ref()));
        if let Some(id) = self.by_key.get(&key) {
            if let Some(def) = def {
                self.duplicates.push(Duplicate { name: name.clone(), line: def.line, col: def.col });
            }
            return Some(*id);
        }
        let id = self.mk(name.clone(), kind, def, meta);
        self.by_key.insert(key, id);
        self.doc_keys.push(id);
        if let Some(def) = def {
            self.occurrences.push(Occurrence { span: def, symbol: id, is_def: true, is_bracketed: kind == SymKind::LoopVar, access: None });
        }
        Some(id)
    }

    fn get(&self, kind: SymKind, name: &str) -> Option<SymId> {
        self.by_key.get(&(kind, Box::from(name))).copied()
    }

    fn add_occ(&mut self, sym: SymId, span: Option<Span>, bracketed: bool, access: Access) {
        let Some(span) = span else { return };
        self.symbols[sym as usize].refs.push((span, access));
        self.occurrences.push(Occurrence { span, symbol: sym, is_def: false, is_bracketed: bracketed, access: Some(access) });
    }

    fn lookup_local(&self, name: &str) -> Option<SymId> {
        for scope in self.scope_stack.iter().rev() {
            for id in scope.iter().rev() {
                if self.symbols[*id as usize].name.is(name) {
                    return Some(*id);
                }
            }
        }
        None
    }

    fn push_scope(&mut self, node: Node<'_>, entries: Vec<SymId>) {
        let start_line = node.line();
        let end_line = subtree_max_line(node) + 1;
        let mut unique: Vec<SymId> = Vec::new();
        for id in entries {
            let name = &self.symbols[id as usize].name;
            if let Some(slot) = unique.iter_mut().find(|e| self.symbols[**e as usize].name == *name) {
                *slot = id;
            } else {
                unique.push(id);
            }
        }
        self.scopes.push(Scope { start_line, end_line, symbols: unique.clone() });
        self.scope_stack.push(unique);
    }

    fn resolve_var(&mut self, name: &str, span: Option<Span>, list_ctx: bool, access: Access) -> Option<SymId> {
        if let Some(local) = self.lookup_local(name) {
            self.add_occ(local, span, true, access);
            return Some(local);
        }
        if name.contains('.')
            && let Some(field) = self.get(SymKind::StructField, name)
        {
            self.add_occ(field, span, true, access);
            return Some(field);
        }
        let is_list = self.sets.lists.contains(name) && (list_ctx || !self.sets.vars.contains(name));
        if is_list || self.sets.vars.contains(name) {
            let kind = if is_list { SymKind::ProjectList } else { SymKind::ProjectVar };
            let sym = self.doc_symbol(&JsVal::Str(name.into()), kind, None, SymMeta::None)?;
            self.add_occ(sym, span, true, access);
            return Some(sym);
        }
        if name != "_err_"
            && self.project_indexed
            && let Some(span) = span
        {
            self.unresolved.push(Unresolved { name: name.into(), guess: if list_ctx { Guess::List } else { Guess::Variable }, span });
        }
        None
    }

    fn param_symbols(&mut self, params: &[JsVal], spans: &[NamedSpan], owner: &JsVal) -> Vec<SymId> {
        let kept: Vec<&JsVal> = params.iter().filter(|p| !p.is("_err_")).collect();
        let mut out = Vec::new();
        for (i, p) in kept.into_iter().enumerate() {
            let span = spans.get(i).filter(|s| s.name == *p).map(|s| s.span);
            let id = self.mk(p.clone(), SymKind::Param, span, SymMeta::Param { owner: owner.clone() });
            if let Some(def) = span {
                self.occurrences.push(Occurrence { span: def, symbol: id, is_def: true, is_bracketed: false, access: None });
            }
            out.push(id);
        }
        out
    }

    fn loop_var(&mut self, name: &str, span: Option<Span>) -> Option<SymId> {
        if name.is_empty() || name == "_err_" {
            return None;
        }
        let id = self.mk(JsVal::Str(name.into()), SymKind::LoopVar, span, SymMeta::None);
        if let Some(def) = span {
            self.occurrences.push(Occurrence { span: def, symbol: id, is_def: true, is_bracketed: true, access: None });
        }
        Some(id)
    }

    fn call_target(&mut self, name: &str, span: Span, is_launch: bool) {
        if name == "_err_" {
            return;
        }
        let routine = self.get(SymKind::Routine, name);
        let define = self.get(SymKind::Define, name);
        if is_launch {
            match routine {
                Some(r) => self.add_occ(r, Some(span), false, Access::Read),
                None => self.unresolved.push(Unresolved { name: name.into(), guess: Guess::Routine, span }),
            }
            return;
        }
        if let Some(d) = define {
            self.add_occ(d, Some(span), false, Access::Read);
            return;
        }
        if let Some(r) = routine {
            self.add_occ(r, Some(span), false, Access::Read);
            return;
        }
        if is_stmt_builtin(name) {
            return;
        }
        self.unresolved.push(Unresolved { name: name.into(), guess: Guess::Call, span });
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Var { name, .. } => {
                self.resolve_var(name, span_of_expr(e), false, Access::Read);
            }
            Expr::Reporter { name, .. } => {
                if let Some(local) = self.lookup_local(name) {
                    self.add_occ(local, span_of_expr(e), false, Access::Read);
                } else if let Some(member) = self.get(SymKind::EnumMember, name) {
                    self.add_occ(member, span_of_expr(e), false, Access::Read);
                } else if !REPORTER_BUILTINS.contains(&&**name)
                    && &**name != "_err_"
                    && let Some(span) = span_of_expr(e)
                {
                    self.unresolved.push(Unresolved { name: name.clone(), guess: Guess::Reporter, span });
                }
            }
            Expr::Call { name, args, .. } => {
                if let Some(define) = self.get(SymKind::Define, name) {
                    self.add_occ(define, span_of_expr(e), false, Access::Read);
                } else if !EXPR_FN_BUILTINS.contains(&&**name)
                    && &**name != "_err_"
                    && let Some(span) = span_of_expr(e)
                {
                    self.unresolved.push(Unresolved { name: name.clone(), guess: Guess::ExprFn, span });
                }
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Member { object, args, .. } => {
                if let Expr::Var { name, .. } = &**object {
                    self.resolve_var(name, span_of_expr(object), true, Access::Read);
                }
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Bin { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Unary { operand, .. } => self.expr(operand),
            Expr::Ternary { cond, then, alt, .. } => {
                self.expr(cond);
                self.expr(then);
                self.expr(alt);
            }
            Expr::Deref { addr, .. } => self.expr(addr),
            Expr::Addr { var_name, var_span, .. } => {
                self.resolve_var(var_name, Some(*var_span), false, Access::ReadWrite);
            }
            _ => {}
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn generic(&mut self, node: Node<'_>) {
        let mut children: Vec<Node<'_>> = Vec::new();
        node.fields(&mut |key, value| {
            if matches!(key, "line" | "col" | "endLine" | "endCol" | "type" | "varSpan" | "nameSpan" | "listSpan") {
                return;
            }
            crate::walk::push_nodes(value, &mut children);
        });
        for child in children {
            match child {
                Node::Stmt(s, _) => self.stmt(s),
                Node::Expr(e) => self.expr(e),
                _ => {}
            }
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        let top_span = self.top_span.take();
        let scope_node = Node::Stmt(stmt, top_span.as_ref());
        match &stmt.kind {
            StmtKind::Asm { .. } => {}
            StmtKind::SetVar { var_name, value, var_span } | StmtKind::ChangeVar { var_name, value, var_span } => {
                self.expr(value);
                if var_span.is_some() {
                    let access = if matches!(stmt.kind, StmtKind::SetVar { .. }) { Access::Write } else { Access::ReadWrite };
                    self.resolve_var(var_name, *var_span, false, access);
                }
            }
            StmtKind::MemberCall { object, method, args } => {
                if let Expr::Var { name, .. } = object {
                    let access = if &**method == "sort" { Access::ReadWrite } else { Access::Read };
                    self.resolve_var(name, span_of_expr(object), true, access);
                }
                for a in args {
                    self.expr(a);
                }
            }
            StmtKind::List { list_name, list_span, fields, .. } => {
                for key in [F::Item, F::Index, F::ValueExpr, F::CountExpr, F::ClearFirst] {
                    if let Some((_, e)) = fields.iter().find(|(f, _)| *f == key) {
                        self.expr(e);
                    }
                }
                if list_span.is_some() {
                    self.resolve_var(&list_name.text(), *list_span, true, Access::Write);
                }
            }
            StmtKind::Visibility { kind, name, name_span } => {
                if name_span.is_some() {
                    self.resolve_var(&name.text(), *name_span, kind.is_list(), Access::Read);
                }
            }
            StmtKind::Call { name, args } => {
                self.call_target(name, named_span(stmt.line, stmt.col, name), false);
                for a in args {
                    self.expr(a);
                }
            }
            StmtKind::LaunchAwait { name, args, name_span, .. } => {
                let span = name_span.unwrap_or_else(|| Span::new(stmt.line, stmt.col, stmt.line, stmt.col + js::utf16_len(name)));
                self.call_target(name, span, true);
                for a in args {
                    self.expr(a);
                }
            }
            StmtKind::Cancel { name, name_span } => {
                let span = name_span.unwrap_or_else(|| Span::new(stmt.line, stmt.col, stmt.line, stmt.col + js::utf16_len(name)));
                self.call_target(name, span, true);
            }
            StmtKind::For { var_name, from, to, body, var_span, .. } => {
                self.expr(from);
                self.expr(to);
                let lv = self.loop_var(var_name, *var_span);
                self.push_scope(scope_node, lv.into_iter().collect());
                self.stmts(body);
                self.scope_stack.pop();
            }
            StmtKind::PyFor { var_name, list_name, body, var_span, list_span } => {
                if list_span.is_some() && &**list_name != "_err_" {
                    self.resolve_var(list_name, *list_span, true, Access::Read);
                }
                let lv = self.loop_var(var_name, *var_span);
                self.push_scope(scope_node, lv.into_iter().collect());
                self.stmts(body);
                self.scope_stack.pop();
            }
            StmtKind::Match { subject, cases, default_body } => {
                self.expr(subject);
                for c in cases {
                    for v in &c.values {
                        self.expr(v);
                    }
                    self.stmts(&c.body);
                }
                if let Some(d) = default_body {
                    self.stmts(d);
                }
            }
            _ => self.generic(Node::Stmt(stmt, None)),
        }
    }
}

pub fn analyze(src: &str, sprite: &str, index: &Arc<ScratchIndex>, headers: &dyn Headers) -> Analysis {
    let mut expansion = None;
    let mut text: Arc<str> = Arc::from(src);
    if src.contains("#include") {
        let ex = expand(src, headers);
        if !ex.segments.is_empty() || !ex.errors.is_empty() {
            text = Arc::from(ex.text.as_str());
            expansion = Some(ex);
        }
    }
    let tokens = tokenize(&text, false);
    let (ast, mut parse_errors) = parse(&text, &tokens);
    if let Some(ex) = &expansion {
        parse_errors.extend(ex.errors.iter().cloned());
    }

    let sets = index.var_sets(sprite);
    let project_indexed = !sets.vars.is_empty() || !sets.lists.is_empty();
    let mut b = Builder {
        symbols: Vec::new(),
        doc_keys: Vec::new(),
        by_key: FxHashMap::default(),
        occurrences: Vec::new(),
        scopes: Vec::new(),
        scope_stack: Vec::new(),
        unresolved: Vec::new(),
        duplicates: Vec::new(),
        sets,
        project_indexed,
        top_span: None,
    };

    for block in &ast.blocks {
        match &block.kind {
            StmtKind::Define(d) => {
                let end_line = if d.name_end_line != 0 { d.name_end_line } else { block.line };
                let end_col = if d.name_end_col != 0 { d.name_end_col } else { block.col + d.name.js_len().unwrap_or(0) };
                let def = Span::new(block.line, block.col, end_line, end_col);
                b.doc_symbol(&d.name, SymKind::Define, Some(def), SymMeta::Callable { params: d.params.clone(), returns: d.returns });
            }
            StmtKind::Routine(r) => {
                let def = r.name_span.unwrap_or_else(|| Span::new(block.line, block.col, block.line, block.col + js::utf16_len(&r.name)));
                b.doc_symbol(&JsVal::Str(r.name.clone()), SymKind::Routine, Some(def), SymMeta::Callable { params: r.params.clone(), returns: false });
            }
            StmtKind::Enum { entries } => {
                for e in entries {
                    b.doc_symbol(&JsVal::Str(e.name.clone()), SymKind::EnumMember, Some(e.span), SymMeta::Enum { value: e.value.clone() });
                }
            }
            StmtKind::Struct { name, fields, name_span, field_spans } => {
                let struct_sym = b.doc_symbol(&JsVal::Str(name.clone()), SymKind::Struct, *name_span, SymMeta::Struct { fields: fields.clone() });
                for f in field_spans {
                    let field = f.name.text();
                    let full = JsVal::Str(format!("{name}.{field}").into());
                    let meta = SymMeta::Field { struct_name: name.clone(), field: field.as_ref().into(), struct_sym: None };
                    if let Some(fs) = b.doc_symbol(&full, SymKind::StructField, Some(f.span), meta)
                        && let (Some(ss), SymMeta::Field { struct_sym: slot, .. }) = (struct_sym, &mut b.symbols[fs as usize].meta)
                    {
                        *slot = Some(ss);
                    }
                }
            }
            _ => {}
        }
    }

    for node in script_nodes(&ast) {
        let Node::Stmt(block, _) = node else { continue };
        match &block.kind {
            StmtKind::Define(d) => {
                let params = b.param_symbols(&d.params, &d.param_spans, &d.name);
                b.push_scope(node, params);
                b.stmts(&d.body);
                b.scope_stack.pop();
            }
            StmtKind::Routine(r) => {
                let params = b.param_symbols(&r.params, &r.param_spans, &JsVal::Str(r.name.clone()));
                b.push_scope(node, params);
                b.stmts(&r.body);
                b.scope_stack.pop();
            }
            StmtKind::On { hat, body } => {
                if let HatEvent::GreaterThan { threshold, .. } = &hat.event {
                    b.expr(threshold);
                }
                b.stmts(body);
            }
            StmtKind::Orphaned { body } => b.stmts(body),
            StmtKind::Enum { .. } | StmtKind::Struct { .. } => {}
            _ => {
                if let Node::Stmt(_, span) = node {
                    b.top_span = span.copied();
                }
                b.stmt(block);
            }
        }
    }

    b.occurrences.sort_by(|a, c| a.span.line.cmp(&c.span.line).then(a.span.col.cmp(&c.span.col)));

    let mut occurrences = b.occurrences;
    let mut scopes = b.scopes;
    let mut symbols = b.symbols;
    if let Some(ex) = &expansion {
        let user = ex.user_line_count;
        occurrences.retain(|o| o.span.line <= user);
        scopes.retain(|s| s.start_line <= user);
        for sym in symbols.iter_mut() {
            if let Some(def) = sym.def_range.filter(|d| d.line > user) {
                let seg = ex.segment_for(def.line);
                sym.header_origin = Some(seg.map(|s| s.header.clone()).unwrap_or_else(|| "header".into()));
                let line = seg.map(|s| s.include_line).unwrap_or(user);
                sym.def_range = Some(Span::new(line, 1, line, 2));
            }
        }
    }

    Analysis {
        src: text,
        tokens,
        ast,
        parse_errors,
        symbols,
        doc_keys: b.doc_keys,
        by_key: b.by_key,
        occurrences,
        scopes,
        unresolved: b.unresolved,
        duplicates: b.duplicates,
        sprite: sprite.into(),
        project_indexed,
        expansion,
        index: index.clone(),
    }
}

pub fn symbol_at(analysis: &Analysis, line: u32, col: u32) -> Option<&Occurrence> {
    analysis.occurrences.iter().take_while(|o| o.span.line <= line).find(|occ| {
        let end_line = occ.span.end_line;
        if line > end_line {
            return false;
        }
        let after_start = line > occ.span.line || col >= occ.span.col;
        let before_end = line < end_line || col <= occ.span.end_col;
        after_start && before_end
    })
}

pub fn visible_symbols(analysis: &Analysis, line: u32) -> Vec<SymId> {
    let mut out = Vec::new();
    let mut seen_names: Vec<String> = Vec::new();
    for scope in analysis.scopes.iter().rev() {
        if line < scope.start_line || line > scope.end_line {
            continue;
        }
        for id in &scope.symbols {
            let name = analysis.symbol(*id).name.text().into_owned();
            if !seen_names.contains(&name) {
                seen_names.push(name);
                out.push(*id);
            }
        }
    }
    let mut seen_keys: rustc_hash::FxHashSet<String> = Default::default();
    for (id, sym) in analysis.symbols.iter().enumerate() {
        if matches!(sym.kind, SymKind::Param | SymKind::LoopVar) {
            continue;
        }
        if seen_keys.insert(format!("{}:{}", sym.kind.js(), sym.name.text())) {
            out.push(id as SymId);
        }
    }
    out
}

fn push(items: &mut Vec<Diag>, line: u32, col: u32, len: Option<u32>, message: String, severity: Severity, category: Category) {
    let len = len.filter(|l| *l > 0).unwrap_or(1);
    items.push(Diag::new(line.max(1), col.max(1), len, message, severity, category));
}

fn did_you_mean(similar: &[String]) -> String {
    if similar.is_empty() { String::new() } else { format!(" — did you mean {}?", similar.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")) }
}

fn walk_all<'a>(node: Node<'a>, visit: &mut dyn FnMut(Node<'a>)) {
    visit(node);
    let mut children = Vec::new();
    node.children(&mut children);
    for child in children {
        walk_all(child, visit);
    }
}

pub fn semantic_diagnostics(analysis: &Analysis) -> Vec<Diag> {
    let mut items = Vec::new();
    let sem = |items: &mut Vec<Diag>, line, col, len, message| push(items, line, col, len, message, Severity::Warning, Category::Semantic);

    for d in &analysis.duplicates {
        sem(&mut items, d.line, d.col, d.name.js_len().map(|l| l.max(1)), format!("`{}` is already declared", d.name.text()));
    }

    let define_names = analysis.names_of(SymKind::Define);
    let routine_names = analysis.names_of(SymKind::Routine);
    let enum_names = analysis.names_of(SymKind::EnumMember);

    for u in &analysis.unresolved {
        let end_col = if u.span.end_col != 0 { u.span.end_col } else { u.span.col + 1 };
        let len = (end_col as i64 - u.span.col as i64).max(js::utf16_len(&u.name) as i64).max(1) as u32;
        let message = match u.guess {
            Guess::Routine => {
                format!("Unknown scratchroutine `{}`{}", u.name, did_you_mean(&fuzzy_match(&u.name, routine_names.iter().map(String::as_str), 3)))
            }
            Guess::Call => {
                let mut candidates: Vec<&str> = define_names.iter().chain(&routine_names).map(String::as_str).collect();
                for k in stmt_builtins() {
                    candidates.push(k);
                }
                format!("Unknown block `{}`{}", u.name, did_you_mean(&fuzzy_match(&u.name, candidates.into_iter(), 3)))
            }
            Guess::ExprFn => format!("Unknown function `{}`{}", u.name, did_you_mean(&fuzzy_match(&u.name, EXPR_FN_BUILTINS.iter().copied(), 3))),
            Guess::Reporter => {
                let mut candidates: Vec<&str> = enum_names.iter().map(String::as_str).collect();
                for k in REPORTER_BUILTINS {
                    candidates.push(k);
                }
                let similar = fuzzy_match(&u.name, candidates.into_iter(), 3);
                if similar.is_empty() {
                    format!("Unknown name `{}` — variables need brackets: `[{}]`", u.name, u.name)
                } else {
                    format!("Unknown name `{}`{}", u.name, did_you_mean(&similar))
                }
            }
            Guess::List | Guess::Variable => format!("`[{}]` doesn't exist in Scratch", u.name),
        };
        sem(&mut items, u.span.line, u.span.col, Some(len), message);
    }

    let arity = |items: &mut Vec<Diag>, name: &str, line: u32, col: u32, got: usize, sym: &Symbol| {
        let want = sym.params().len();
        if want != got {
            sem(items, line, col, Some(js::utf16_len(name)), format!("`{name}` takes {want} argument{}, got {got}", if want == 1 { "" } else { "s" }));
        }
    };
    for root in script_nodes(&analysis.ast) {
        walk_all(root, &mut |node| match node {
            Node::Stmt(stmt, _) => match &stmt.kind {
                StmtKind::Call { name, args } => {
                    if let Some(d) = analysis.lookup(SymKind::Define, name) {
                        arity(&mut items, name, stmt.line, stmt.col, args.len(), analysis.symbol(d));
                    }
                }
                StmtKind::LaunchAwait { name, args, .. } => {
                    if let Some(r) = analysis.lookup(SymKind::Routine, name) {
                        arity(&mut items, name, stmt.line, stmt.col, args.len(), analysis.symbol(r));
                    }
                }
                _ => {}
            },
            Node::Expr(Expr::Call { name, args, line, col }) => {
                if let Some(d) = analysis.lookup(SymKind::Define, name) {
                    let sym = analysis.symbol(d);
                    arity(&mut items, name, *line, *col, args.len(), sym);
                    if !matches!(sym.meta, SymMeta::Callable { returns: true, .. }) {
                        sem(&mut items, *line, *col, Some(js::utf16_len(name)), format!("`{name}` doesn't return a value — add `returns` to its define"));
                    }
                }
            }
            _ => {}
        });
    }

    for block in &analysis.ast.blocks {
        let StmtKind::Define(d) = &block.kind else { continue };
        if !d.returns {
            continue;
        }
        let mut has_return = false;
        for s in &d.body {
            walk_all(Node::Stmt(s, None), &mut |node| {
                if let Node::Stmt(Stmt { kind: StmtKind::Return { value: Some(_) }, .. }, _) = node {
                    has_return = true;
                }
            });
        }
        if !has_return {
            sem(&mut items, block.line, block.col, d.name.js_len(), format!("`{}` is `returns` but never returns a value", d.name.text()));
        }
    }

    let sets = analysis.var_sets();
    for sym in &analysis.symbols {
        if !matches!(sym.kind, SymKind::Param | SymKind::LoopVar) {
            continue;
        }
        let Some(def) = sym.def_range else { continue };
        let name = sym.name.as_str();
        if name.is_some_and(|n| sets.vars.contains(n) || sets.lists.contains(n)) {
            let n = name.unwrap_or_default();
            sem(&mut items, def.line, def.col, sym.name.js_len(), format!("`{n}` shadows the Scratch variable `[{n}]`"));
        } else if analysis.lookup(SymKind::EnumMember, &sym.name.text()).is_some() {
            sem(&mut items, def.line, def.col, sym.name.js_len(), format!("`{}` shadows the enum constant", sym.name.text()));
        }
    }

    items.into_iter().map(|d| analysis.map_diag(d)).collect()
}

fn subtree_has(nodes: &[Stmt], types: &[&str]) -> bool {
    let mut found = false;
    for s in nodes {
        walk_all(Node::Stmt(s, None), &mut |node| {
            if !found && node.type_name().is_some_and(|t| types.contains(&t)) {
                found = true;
            }
        });
    }
    found
}

fn expr_reads_var(e: &Expr, name: &str) -> bool {
    let mut found = false;
    walk_all(Node::Expr(e), &mut |node| {
        if let Node::Expr(Expr::Var { name: n, .. }) = node
            && &**n == name
        {
            found = true;
        }
    });
    found
}

fn is_nesting(stmt: &Stmt) -> bool {
    matches!(
        stmt.kind,
        StmtKind::If { .. }
            | StmtKind::Forever { .. }
            | StmtKind::Repeat { .. }
            | StmtKind::RepeatUntil { .. }
            | StmtKind::While { .. }
            | StmtKind::For { .. }
            | StmtKind::PyFor { .. }
            | StmtKind::Match { .. }
            | StmtKind::DoWhile { .. }
    )
}

struct Smells<'a> {
    items: Vec<Diag>,
    last_set: Option<(&'a str, u32, u32)>,
    nums: Vec<(f64, Vec<(u32, u32)>)>,
}

impl<'a> Smells<'a> {
    fn smell(&mut self, line: u32, col: u32, len: u32, message: String) {
        push(&mut self.items, line, col, Some(len), message, Severity::Info, Category::Smell);
    }

    fn collect_nums(&mut self, stmt: &'a Stmt) {
        let mut roots = Vec::new();
        Node::Stmt(stmt, None).fields(&mut |key, value| {
            if !matches!(key, "body" | "then" | "alt" | "cases" | "defaultBody") {
                crate::walk::push_nodes(value, &mut roots);
            }
        });
        fn walk(node: Node<'_>, nums: &mut Vec<(f64, Vec<(u32, u32)>)>) {
            if let Node::Expr(Expr::Num { value, line, col }) = node
                && *line != 0
                && !BORING_NUMBERS.contains(value)
            {
                match nums.iter_mut().find(|(k, _)| k == value) {
                    Some((_, sites)) => sites.push((*line, *col)),
                    None => nums.push((*value, vec![(*line, *col)])),
                }
                return;
            }
            let mut children = Vec::new();
            node.children(&mut children);
            for child in children {
                walk(child, nums);
            }
        }
        for root in roots {
            walk(root, &mut self.nums);
        }
    }

    fn body(&mut self, stmts: &'a [Stmt], depth: u32) {
        for s in stmts {
            self.stmt(s, depth);
        }
    }

    fn stmt(&mut self, stmt: &'a Stmt, depth: u32) {
        if let StmtKind::SetVar { var_name, value, .. } = &stmt.kind {
            if let Some((last, line, col)) = self.last_set
                && last == &**var_name
                && !expr_reads_var(value, var_name)
            {
                self.smell(line, col, 3, format!("Overwritten on line {} before it's read", stmt.line));
            }
            self.last_set = Some((var_name, stmt.line, stmt.col));
        } else {
            self.last_set = None;
        }

        self.collect_nums(stmt);

        let nesting = is_nesting(stmt);
        let nested = depth + u32::from(nesting);
        if nesting && nested > 4 {
            self.smell(stmt.line, stmt.col, 4, format!("Nested {nested} levels deep — extract a `define`"));
        }

        match &stmt.kind {
            StmtKind::Forever { body } | StmtKind::RepeatUntil { body, .. } => {
                let is_forever = matches!(stmt.kind, StmtKind::Forever { .. });
                if body.is_empty() {
                    self.smell(stmt.line, stmt.col, 7, format!("Empty `{}`", if is_forever { "forever" } else { "repeat until" }));
                } else if is_forever && body.iter().all(|s| matches!(s.kind, StmtKind::If { .. })) && !subtree_has(body, BLOCKING_STMTS) {
                    self.smell(stmt.line, stmt.col, 7, "Busy-wait — use `wait until`".into());
                }
            }
            StmtKind::Repeat { body, .. } | StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::PyFor { body, .. } if body.is_empty() => {
                self.smell(stmt.line, stmt.col, 6, "Empty loop".into());
            }
            StmtKind::If { then, alt: None, .. } if then.is_empty() => {
                self.smell(stmt.line, stmt.col, 2, "Empty `if`".into());
            }
            _ => {}
        }

        let saved = self.last_set;
        if let StmtKind::If { then, alt, .. } = &stmt.kind {
            self.last_set = None;
            self.body(then, nested);
            if let Some(alt) = alt {
                self.last_set = None;
                self.body(alt, nested);
            }
        }
        if let Some(body) = stmt.body() {
            self.last_set = None;
            self.body(body, nested);
        }
        if let StmtKind::Match { cases, default_body, .. } = &stmt.kind {
            for c in cases {
                self.last_set = None;
                self.body(&c.body, nested);
            }
            if let Some(d) = default_body {
                self.last_set = None;
                self.body(d, nested);
            }
        }
        self.last_set = saved;
    }
}

pub fn smell_diagnostics(analysis: &Analysis) -> Vec<Diag> {
    let mut s = Smells { items: Vec::new(), last_set: None, nums: Vec::new() };
    for sym in &analysis.symbols {
        let Some(def) = sym.def_range else { continue };
        if !sym.refs.is_empty() {
            continue;
        }
        let unused = match sym.kind {
            SymKind::Define => "is never called",
            SymKind::Routine => "is never launched",
            SymKind::Param | SymKind::LoopVar => "is unused",
            _ => continue,
        };
        let len = sym.name.js_len().filter(|l| *l > 0).unwrap_or(1);
        s.smell(def.line, def.col, len, format!("`{}` {unused}", sym.name.text()));
    }

    for block in &analysis.ast.blocks {
        if matches!(block.kind, StmtKind::Enum { .. } | StmtKind::Struct { .. }) {
            continue;
        }
        s.last_set = None;
        match block.body() {
            Some(body) => s.body(body, 0),
            None if block.type_name().ends_with("Stmt") => s.body(std::slice::from_ref(block), 0),
            None => {}
        }
    }

    let nums = std::mem::take(&mut s.nums);
    for (value, sites) in nums {
        if sites.len() >= 3 {
            let (line, col) = sites[0];
            let text = js::number_to_string(value);
            s.smell(line, col, text.len() as u32, format!("{text} appears {} times — make it an `enum` constant", sites.len()));
        }
    }

    s.items.into_iter().map(|d| analysis.map_diag(d)).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemToken {
    pub line: u32,
    pub col: u32,
    pub length: u32,
    pub token_type: &'static str,
}

pub fn semantic_tokens(analysis: &Analysis) -> Vec<SemToken> {
    let mut out = Vec::new();
    for occ in &analysis.occurrences {
        if occ.span.end_line != 0 && occ.span.end_line != occ.span.line {
            continue;
        }
        let sym = analysis.symbol(occ.symbol);
        let Some(token_type) = sym.kind.token_type() else { continue };
        let end = if occ.span.end_col != 0 { occ.span.end_col as i64 } else { occ.span.col as i64 + sym.name.js_len().unwrap_or(0) as i64 };
        let length = end - occ.span.col as i64;
        if length <= 0 {
            continue;
        }
        out.push(SemToken { line: occ.span.line, col: occ.span.col, length: length as u32, token_type });
    }
    for u in &analysis.unresolved {
        if u.span.end_line != 0 && u.span.end_line != u.span.line {
            continue;
        }
        let name_len = js::utf16_len(&u.name) as i64;
        let end = if u.span.end_col != 0 { u.span.end_col as i64 } else { u.span.col as i64 + name_len };
        let length = (end - u.span.col as i64).max(name_len);
        out.push(SemToken { line: u.span.line, col: u.span.col, length: length.max(0) as u32, token_type: "invalid" });
    }
    out.sort_by(|a, b| a.line.cmp(&b.line).then(a.col.cmp(&b.col)));
    out
}

pub fn parse_diagnostics(analysis: &Analysis) -> Vec<Diag> {
    analysis.parse_errors.iter().map(|e| Diag::new(e.line, e.col, e.len, e.message.clone(), Severity::Error, Category::Parse)).collect()
}

pub fn local_diagnostics(analysis: &Analysis, settings: &crate::diag::LintSettings) -> Vec<Diag> {
    let mut items = parse_diagnostics(analysis);
    items.extend(crate::lint::lint(&analysis.ast).into_iter().filter(|d| settings.allows(d.category)));
    if settings.lint_typecheck {
        items.extend(crate::lint::type_check(&analysis.ast, analysis.var_sets()));
    }
    if settings.lint_semantic {
        items.extend(semantic_diagnostics(analysis));
    }
    if settings.lint_smells {
        items.extend(smell_diagnostics(analysis));
    }
    items
}
