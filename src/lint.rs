use crate::ast::*;
use crate::data;
use crate::diag::{Category, Diag, Severity};
use crate::index::VarSets;
use crate::walk::Node;

fn has_break(node: Node<'_>) -> bool {
    if let Node::Stmt(stmt, _) = node {
        if matches!(stmt.kind, StmtKind::Break) {
            return true;
        }
        if stmt.is_loop() {
            return false;
        }
    }
    let mut children = Vec::new();
    node.children(&mut children);
    children.into_iter().any(has_break)
}

fn body_has_break(body: &[Stmt]) -> bool {
    body.iter().any(|s| has_break(Node::Stmt(s, None)))
}

fn is_terminator(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Forever { body } => !body_has_break(body),
        StmtKind::Simple { kind, .. } => matches!(kind, Simple::StopAll | Simple::StopThis),
        _ => false,
    }
}

struct Lint {
    items: Vec<Diag>,
}

impl Lint {
    fn warn(&mut self, line: u32, col: u32, message: String, category: Option<Category>) {
        let whole_line = matches!(category, Some(Category::Unreachable | Category::Orphaned));
        self.items.push(Diag {
            line: line.max(1),
            col: col.max(1),
            len: 1,
            message,
            severity: Severity::Warning,
            category: category.unwrap_or(Category::Asm),
            whole_line,
            unnecessary: false,
        });
    }

    fn body(&mut self, stmts: &[Stmt]) {
        let mut dead = false;
        for stmt in stmts {
            if dead {
                self.warn(stmt.line, stmt.col, "Unreachable code".into(), Some(Category::Unreachable));
                self.children(stmt);
                continue;
            }
            self.children(stmt);
            if is_terminator(stmt) {
                dead = true;
            }
        }
    }

    fn children(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::On { body, .. } | StmtKind::Orphaned { body } => self.body(body),
            StmtKind::Define(d) => self.body(&d.body),
            StmtKind::If { then, alt, .. } => {
                self.body(then);
                if let Some(alt) = alt {
                    self.body(alt);
                }
            }
            StmtKind::Forever { body }
            | StmtKind::Repeat { body, .. }
            | StmtKind::RepeatUntil { body, .. }
            | StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::PyFor { body, .. } => self.body(body),
            StmtKind::Routine(r) => self.body(&r.body),
            StmtKind::Asm { unsafe_mode: true, statements } => {
                for s in statements {
                    if data::asm_opcode(&s.opcode).is_none() {
                        self.warn(s.line, s.col, format!("Unknown opcode `{}` — inputs are wired by guesswork", s.opcode), None);
                    }
                }
            }
            _ => {}
        }
    }
}

pub fn lint(script: &Script) -> Vec<Diag> {
    let mut lint = Lint { items: Vec::new() };
    for block in &script.blocks {
        match &block.kind {
            StmtKind::Orphaned { .. } => {
                lint.warn(block.line, block.col, "Never runs — not under a hat block".into(), Some(Category::Orphaned));
                lint.children(block);
            }
            StmtKind::Struct { .. } | StmtKind::Enum { .. } => {}
            StmtKind::On { .. } | StmtKind::Define(_) | StmtKind::Routine(_) => lint.children(block),
            _ => {
                lint.warn(block.line, block.col, "Never runs — not inside `on` or `define`".into(), Some(Category::Orphaned));
                lint.children(block);
            }
        }
    }
    lint.items
}

const LIST_BUILTINS: &[&str] =
    &["listAdd", "listDelete", "listInsert", "listReplace", "listDeleteAll", "showList", "hideList", "populateList", "populateArray"];
const VAR_BUILTINS: &[&str] = &["showVariable", "hideVariable"];
const DEFAULT_EXPR_KEYS: &[F] = &[F::Value, F::Msg, F::Secs, F::X, F::Y, F::Degrees, F::Duration, F::Steps, F::Effect];

struct TypeCheck<'a> {
    sets: &'a VarSets,
    items: Vec<Diag>,
}

impl TypeCheck<'_> {
    fn err(&mut self, line: u32, col: u32, message: String) {
        self.items.push(Diag::new(line.max(1), col.max(1), 1, message, Severity::Warning, Category::TypeCheck));
    }

    fn list_check(&mut self, line: u32, col: u32, name: &str, needs: &str) {
        if self.sets.vars.contains(name) {
            self.err(line, col, format!("`{needs}` needs a list; `[{name}]` is a variable"));
        } else if !self.sets.lists.contains(name) {
            self.err(line, col, format!("`[{name}]` isn't a list in Scratch"));
        }
    }

    fn expr(&mut self, node: &Expr) {
        match node {
            Expr::Member { object, method, args, .. } => {
                if let Expr::Var { name, line, col, .. } = &**object {
                    let needs = format!(".{method}()");
                    self.list_check(*line, *col, name, &needs);
                }
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Bin { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            _ => {}
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Call { name, args } => {
                if let Some(Expr::Var { name: first, line, col, .. }) = args.first() {
                    if LIST_BUILTINS.contains(&&**name) {
                        self.list_check(*line, *col, first, name);
                    }
                    if VAR_BUILTINS.contains(&&**name) && self.sets.lists.contains(&**first) {
                        self.err(*line, *col, format!("`{name}` needs a variable; `[{first}]` is a list"));
                    }
                }
                for a in args {
                    self.expr(a);
                }
            }
            StmtKind::SetVar { var_name, value, .. } | StmtKind::ChangeVar { var_name, value, .. } => {
                if self.sets.lists.contains(&**var_name) {
                    self.err(stmt.line, stmt.col, format!("`[{var_name}]` is a list — use list functions"));
                }
                self.expr(value);
            }
            StmtKind::PyFor { list_name, body, .. } => {
                if !list_name.is_empty() {
                    self.list_check(stmt.line, stmt.col, list_name, "pyfor");
                }
                self.stmts(body);
            }
            StmtKind::List { op: ListOp::Populate, list_name, fields, .. } => {
                let name = list_name.text();
                let is_var = list_name.as_str().is_some_and(|n| self.sets.vars.contains(n));
                let is_list = list_name.as_str().is_some_and(|n| self.sets.lists.contains(n));
                if is_var {
                    self.err(stmt.line, stmt.col, format!("`populateList` needs a list; `[{name}]` is a variable"));
                } else if !is_list {
                    self.err(stmt.line, stmt.col, format!("`[{name}]` isn't a list in Scratch"));
                }
                for (_, e) in fields {
                    self.expr(e);
                }
            }
            StmtKind::If { cond, then, alt } => {
                self.expr(cond);
                self.stmts(then);
                if let Some(alt) = alt {
                    self.stmts(alt);
                }
            }
            StmtKind::Forever { body } | StmtKind::Repeat { body, .. } => self.stmts(body),
            StmtKind::RepeatUntil { cond, body } | StmtKind::While { cond, body } => {
                self.expr(cond);
                self.stmts(body);
            }
            StmtKind::For { from, to, body, .. } => {
                self.expr(from);
                self.expr(to);
                self.stmts(body);
            }
            _ => {
                for key in DEFAULT_EXPR_KEYS {
                    if let Some(e) = stmt.field(*key) {
                        self.expr(e);
                    }
                }
                if let Some(body) = stmt.body() {
                    self.stmts(body);
                }
            }
        }
    }
}

pub fn type_check(script: &Script, sets: &VarSets) -> Vec<Diag> {
    let mut check = TypeCheck { sets, items: Vec::new() };
    for block in &script.blocks {
        if let Some(body) = block.body() {
            check.stmts(body);
        }
    }
    check.items
}
