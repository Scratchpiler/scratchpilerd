use crate::ast::*;

#[derive(Clone, Copy)]
pub enum Node<'a> {
    Stmt(&'a Stmt, Option<&'a Span>),
    Expr(&'a Expr),
    Hat(&'a Hat),
    Span(&'a Span),
    NamedSpan(&'a NamedSpan),
    Case(&'a Case),
    EnumEntry(&'a EnumEntry),
    EnumValue(&'a EnumEntry),
    AsmInst(&'a AsmInst),
    AsmArg(&'a (AsmArg, u32, u32)),
}

#[derive(Clone, Copy)]
pub enum Val<'a> {
    Null,
    Bool(bool),
    Num(f64),
    Pos(u32),
    Str(&'a str),
    JsVal(&'a JsVal),
    Node(Node<'a>),
    Stmts(&'a [Stmt]),
    Exprs(&'a [Expr]),
    Cases(&'a [Case]),
    NamedSpans(&'a [NamedSpan]),
    Entries(&'a [EnumEntry]),
    AsmInsts(&'a [AsmInst]),
    AsmArgs(&'a [(AsmArg, u32, u32)]),
    JsVals(&'a [JsVal]),
    Strs(&'a [Box<str>]),
}

impl<'a> Val<'a> {
    fn opt_span(span: &'a Option<Span>) -> Val<'a> {
        span.as_ref().map_or(Val::Null, |s| Val::Node(Node::Span(s)))
    }

    fn opt_stmts(stmts: &'a Option<Vec<Stmt>>) -> Val<'a> {
        stmts.as_ref().map_or(Val::Null, |s| Val::Stmts(s))
    }

    fn expr(e: &'a Expr) -> Val<'a> {
        Val::Node(Node::Expr(e))
    }
}

type Visit<'a, 'f> = &'f mut dyn FnMut(&'static str, Val<'a>);

impl<'a> Node<'a> {
    pub fn type_name(&self) -> Option<&'static str> {
        match self {
            Node::Stmt(s, _) => Some(s.type_name()),
            Node::Expr(e) => Some(e.type_name()),
            Node::EnumValue(e) => Some(match e.value {
                EnumValue::Num(_) => "Num",
                EnumValue::Str(_) => "Str",
            }),
            _ => None,
        }
    }

    pub fn fields(&self, f: Visit<'a, '_>) {
        match *self {
            Node::Stmt(stmt, span) => {
                stmt_fields(stmt, f);
                if let Some(span) = span {
                    f("span", Val::Node(Node::Span(span)));
                }
            }
            Node::Expr(e) => expr_fields(e, f),
            Node::Hat(hat) => {
                f("event", Val::Str(hat.event_name()));
                match &hat.event {
                    HatEvent::Key { key } => f("key", Val::JsVal(key)),
                    HatEvent::Receive { msg, msg_span } => {
                        f("msg", Val::JsVal(msg));
                        f("msgSpan", Val::opt_span(msg_span));
                    }
                    HatEvent::Backdrop { backdrop } => f("backdrop", Val::JsVal(backdrop)),
                    HatEvent::GreaterThan { sense, threshold } => {
                        f("sense", Val::Str(sense));
                        f("threshold", Val::expr(threshold));
                    }
                    _ => {}
                }
                pos(f, hat.line, hat.col);
            }
            Node::Span(span) => span_fields(span, f),
            Node::NamedSpan(named) => {
                f("name", Val::JsVal(&named.name));
                span_fields(&named.span, f);
            }
            Node::Case(case) => {
                f("values", Val::Exprs(&case.values));
                f("body", Val::Stmts(&case.body));
                pos(f, case.line, case.col);
            }
            Node::EnumEntry(entry) => {
                f("name", Val::Str(&entry.name));
                f("value", Val::Node(Node::EnumValue(entry)));
                span_fields(&entry.span, f);
            }
            Node::EnumValue(entry) => {
                f("type", Val::Str(Node::EnumValue(entry).type_name().unwrap_or("Num")));
                match &entry.value {
                    EnumValue::Num(n) => f("value", Val::Num(*n)),
                    EnumValue::Str(s) => f("value", Val::Str(s)),
                }
                pos(f, entry.value_line, entry.value_col);
            }
            Node::AsmInst(inst) => {
                f("opcode", Val::Str(&inst.opcode));
                f("args", Val::AsmArgs(&inst.args));
                pos(f, inst.line, inst.col);
            }
            Node::AsmArg((arg, line, col)) => {
                match arg {
                    AsmArg::Reg(name) => {
                        f("kind", Val::Str("reg"));
                        f("name", Val::Str(name));
                    }
                    AsmArg::Str(s) => {
                        f("kind", Val::Str("lit"));
                        f("valueType", Val::Str("string"));
                        f("value", Val::Str(s));
                    }
                    AsmArg::Num(n) => {
                        f("kind", Val::Str("lit"));
                        f("valueType", Val::Str("number"));
                        f("value", Val::Num(*n));
                    }
                    AsmArg::Bool(b) => {
                        f("kind", Val::Str("lit"));
                        f("valueType", Val::Str("boolean"));
                        f("value", Val::Bool(*b));
                    }
                }
                pos(f, *line, *col);
            }
        }
    }

    pub fn children(&self, out: &mut Vec<Node<'a>>) {
        self.fields(&mut |_, v| push_nodes(v, out));
    }

    pub fn line(&self) -> u32 {
        let mut line = 0;
        self.fields(&mut |k, v| {
            if k == "line"
                && let Val::Pos(n) = v
            {
                line = n;
            }
        });
        line
    }
}

pub fn push_nodes<'a>(v: Val<'a>, out: &mut Vec<Node<'a>>) {
    match v {
        Val::Node(n) => out.push(n),
        Val::Stmts(list) => out.extend(list.iter().map(|s| Node::Stmt(s, None))),
        Val::Exprs(list) => out.extend(list.iter().map(Node::Expr)),
        Val::Cases(list) => out.extend(list.iter().map(Node::Case)),
        Val::NamedSpans(list) => out.extend(list.iter().map(Node::NamedSpan)),
        Val::Entries(list) => out.extend(list.iter().map(Node::EnumEntry)),
        Val::AsmInsts(list) => out.extend(list.iter().map(Node::AsmInst)),
        Val::AsmArgs(list) => out.extend(list.iter().map(Node::AsmArg)),
        _ => {}
    }
}

fn pos(f: Visit<'_, '_>, line: u32, col: u32) {
    if line != 0 {
        f("line", Val::Pos(line));
    }
    if col != 0 {
        f("col", Val::Pos(col));
    }
}

fn end(f: Visit<'_, '_>, end_line: u32, end_col: u32) {
    if end_line != 0 {
        f("endLine", Val::Pos(end_line));
    }
    if end_col != 0 {
        f("endCol", Val::Pos(end_col));
    }
}

fn span_fields(span: &Span, f: Visit<'_, '_>) {
    pos(f, span.line, span.col);
    end(f, span.end_line, span.end_col);
}

fn stmt_fields<'a>(stmt: &'a Stmt, f: Visit<'a, '_>) {
    f("type", Val::Str(stmt.type_name()));
    let (line, col) = (stmt.line, stmt.col);
    match &stmt.kind {
        StmtKind::Define(d) => {
            f("name", Val::JsVal(&d.name));
            f("params", Val::JsVals(&d.params));
            f("body", Val::Stmts(&d.body));
            f("returns", Val::Bool(d.returns));
            f("warp", Val::Bool(d.warp));
            f("noinline", Val::Bool(d.noinline));
            pos(f, line, col);
            if d.name_end_line != 0 {
                f("nameEndLine", Val::Pos(d.name_end_line));
            }
            if d.name_end_col != 0 {
                f("nameEndCol", Val::Pos(d.name_end_col));
            }
            f("paramSpans", Val::NamedSpans(&d.param_spans));
        }
        StmtKind::On { hat, body } => {
            f("hat", Val::Node(Node::Hat(hat)));
            f("body", Val::Stmts(body));
            pos(f, line, col);
        }
        StmtKind::Routine(r) => {
            f("name", Val::Str(&r.name));
            f("params", Val::JsVals(&r.params));
            f("body", Val::Stmts(&r.body));
            pos(f, line, col);
            f("nameSpan", Val::opt_span(&r.name_span));
            f("paramSpans", Val::NamedSpans(&r.param_spans));
        }
        StmtKind::Struct { name, fields, name_span, field_spans } => {
            f("name", Val::Str(name));
            f("fields", Val::Strs(fields));
            pos(f, line, col);
            f("nameSpan", Val::opt_span(name_span));
            f("fieldSpans", Val::NamedSpans(field_spans));
        }
        StmtKind::Enum { entries } => {
            f("entries", Val::Entries(entries));
            pos(f, line, col);
        }
        StmtKind::Orphaned { body } | StmtKind::Forever { body } => {
            f("body", Val::Stmts(body));
            pos(f, line, col);
        }
        StmtKind::If { cond, then, alt } => {
            f("cond", Val::expr(cond));
            f("then", Val::Stmts(then));
            f("alt", Val::opt_stmts(alt));
            pos(f, line, col);
        }
        StmtKind::Repeat { count, body, nounroll } => {
            f("count", Val::expr(count));
            f("body", Val::Stmts(body));
            f("nounroll", Val::Bool(*nounroll));
            pos(f, line, col);
        }
        StmtKind::RepeatUntil { cond, body } | StmtKind::While { cond, body } | StmtKind::DoWhile { cond, body } => {
            f("cond", Val::expr(cond));
            f("body", Val::Stmts(body));
            pos(f, line, col);
        }
        StmtKind::WaitUntil { cond } => {
            f("cond", Val::expr(cond));
            pos(f, line, col);
        }
        StmtKind::For { var_name, from, to, body, nounroll, var_span } => {
            f("varName", Val::Str(var_name));
            f("from", Val::expr(from));
            f("to", Val::expr(to));
            f("body", Val::Stmts(body));
            f("nounroll", Val::Bool(*nounroll));
            pos(f, line, col);
            f("varSpan", Val::opt_span(var_span));
        }
        StmtKind::PyFor { var_name, list_name, body, var_span, list_span } => {
            f("varName", Val::Str(var_name));
            f("listName", Val::Str(list_name));
            f("body", Val::Stmts(body));
            pos(f, line, col);
            f("varSpan", Val::opt_span(var_span));
            f("listSpan", Val::opt_span(list_span));
        }
        StmtKind::LaunchAwait { name, args, name_span, .. } => {
            f("name", Val::Str(name));
            f("args", Val::Exprs(args));
            pos(f, line, col);
            f("nameSpan", Val::opt_span(name_span));
        }
        StmtKind::Cancel { name, name_span } => {
            f("name", Val::Str(name));
            pos(f, line, col);
            f("nameSpan", Val::opt_span(name_span));
        }
        StmtKind::Breakpoint | StmtKind::Break | StmtKind::Continue => pos(f, line, col),
        StmtKind::Asm { unsafe_mode, statements } => {
            f("mode", Val::Str(if *unsafe_mode { "unsafe" } else { "strict" }));
            f("statements", Val::AsmInsts(statements));
            pos(f, line, col);
        }
        StmtKind::Return { value } => {
            f("value", value.as_ref().map_or(Val::Null, Val::expr));
            pos(f, line, col);
        }
        StmtKind::Match { subject, cases, default_body } => {
            f("subject", Val::expr(subject));
            f("cases", Val::Cases(cases));
            f("defaultBody", Val::opt_stmts(default_body));
            pos(f, line, col);
        }
        StmtKind::MemberCall { object, method, args } => {
            f("object", Val::expr(object));
            f("method", Val::Str(method));
            f("args", Val::Exprs(args));
            pos(f, line, col);
        }
        StmtKind::Unknown { value } => {
            f("value", Val::JsVal(value));
            pos(f, line, col);
        }
        StmtKind::RawKeyword { value } => {
            f("value", Val::Str(value));
            pos(f, line, col);
        }
        StmtKind::SetVar { var_name, value, var_span } | StmtKind::ChangeVar { var_name, value, var_span } => {
            f("varName", Val::Str(var_name));
            f("value", Val::expr(value));
            pos(f, line, col);
            f("varSpan", Val::opt_span(var_span));
        }
        StmtKind::Call { name, args } => {
            f("name", Val::Str(name));
            f("args", Val::Exprs(args));
            pos(f, line, col);
        }
        StmtKind::Simple { kind, fields } => {
            match kind {
                Simple::TurnRight => f("dir", Val::Str("right")),
                Simple::TurnLeft => f("dir", Val::Str("left")),
                Simple::CreateCloneMyself => f("target", Val::Str("_myself_")),
                _ => {}
            }
            if let Some(option) = kind.stop_option() {
                f("option", Val::Str(option));
            }
            for (name, e) in fields {
                f(name.js(), Val::expr(e));
            }
            pos(f, line, col);
        }
        StmtKind::List { list_name, list_span, fields, .. } => {
            f("listName", Val::JsVal(list_name));
            f("listSpan", Val::opt_span(list_span));
            for (name, e) in fields {
                f(name.js(), Val::expr(e));
            }
            pos(f, line, col);
        }
        StmtKind::Visibility { name, name_span, .. } => {
            f("name", Val::JsVal(name));
            f("nameSpan", Val::opt_span(name_span));
            pos(f, line, col);
        }
    }
}

fn expr_fields<'a>(e: &'a Expr, f: Visit<'a, '_>) {
    f("type", Val::Str(e.type_name()));
    match e {
        Expr::Num { value, line, col } => {
            f("value", Val::Num(*value));
            pos(f, *line, *col);
        }
        Expr::Str { value, line, col, end_line, end_col } => {
            f("value", Val::Str(value));
            pos(f, *line, *col);
            end(f, *end_line, *end_col);
        }
        Expr::Hex { value, line, col } => {
            f("value", Val::Str(value));
            pos(f, *line, *col);
        }
        Expr::Bool { value, line, col } => {
            f("value", Val::Bool(*value));
            pos(f, *line, *col);
        }
        Expr::Var { name, line, col, end_line, end_col } => {
            f("name", Val::Str(name));
            pos(f, *line, *col);
            end(f, *end_line, *end_col);
        }
        Expr::Reporter { name, line, col } => {
            f("name", Val::Str(name));
            pos(f, *line, *col);
        }
        Expr::Call { name, args, line, col } => {
            f("name", Val::Str(name));
            f("args", Val::Exprs(args));
            pos(f, *line, *col);
        }
        Expr::Member { object, method, args, line, col } => {
            f("object", Val::expr(object));
            f("method", Val::Str(method));
            f("args", Val::Exprs(args));
            pos(f, *line, *col);
        }
        Expr::Bin { op, left, right } => {
            f("op", Val::Str(op.js()));
            f("left", Val::expr(left));
            f("right", Val::expr(right));
        }
        Expr::Unary { op, operand } => {
            f("op", Val::Str(if *op == UnOp::Neg { "-" } else { "not" }));
            f("operand", Val::expr(operand));
        }
        Expr::Ternary { cond, then, alt, line, col } => {
            f("cond", Val::expr(cond));
            f("then", Val::expr(then));
            f("alt", Val::expr(alt));
            pos(f, *line, *col);
        }
        Expr::Deref { addr, line, col } => {
            f("addr", Val::expr(addr));
            pos(f, *line, *col);
        }
        Expr::Addr { var_name, line, col, var_span } => {
            f("varName", Val::Str(var_name));
            pos(f, *line, *col);
            f("varSpan", Val::Node(Node::Span(var_span)));
        }
    }
}

pub fn script_nodes(script: &Script) -> impl Iterator<Item = Node<'_>> {
    script.blocks.iter().zip(script.spans.iter()).map(|(stmt, span)| Node::Stmt(stmt, Some(span)))
}

pub fn to_json(node: Node<'_>, out: &mut String) {
    write_json(node, out, false);
}

pub fn expr_key(e: &Expr) -> String {
    let mut out = String::new();
    write_json(Node::Expr(e), &mut out, true);
    out
}

fn write_json(node: Node<'_>, out: &mut String, skip_positions: bool) {
    out.push('{');
    let mut first = true;
    node.fields(&mut |key, value| {
        if skip_positions && matches!(key, "line" | "col" | "endLine" | "endCol") {
            return;
        }
        if !first {
            out.push(',');
        }
        first = false;
        out.push('"');
        out.push_str(key);
        out.push_str("\":");
        value_json(value, out, skip_positions);
    });
    out.push('}');
}

fn list_json<T>(items: &[T], out: &mut String, mut each: impl FnMut(&T, &mut String)) {
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        each(item, out);
    }
    out.push(']');
}

fn number_json(n: f64, out: &mut String) {
    if n.is_finite() {
        crate::js::write_number(out, n);
    } else {
        out.push_str("null");
    }
}

fn value_json(value: Val<'_>, out: &mut String, sp: bool) {
    match value {
        Val::Null => out.push_str("null"),
        Val::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        Val::Num(n) => number_json(n, out),
        Val::Pos(n) => out.push_str(&n.to_string()),
        Val::Str(s) => out.push_str(&crate::js::json_quote(s)),
        Val::JsVal(JsVal::Str(s)) => out.push_str(&crate::js::json_quote(s)),
        Val::JsVal(JsVal::Num(n)) => number_json(*n, out),
        Val::Node(n) => write_json(n, out, sp),
        Val::Stmts(list) => list_json(list, out, |s, o| write_json(Node::Stmt(s, None), o, sp)),
        Val::Exprs(list) => list_json(list, out, |e, o| write_json(Node::Expr(e), o, sp)),
        Val::Cases(list) => list_json(list, out, |c, o| write_json(Node::Case(c), o, sp)),
        Val::NamedSpans(list) => list_json(list, out, |s, o| write_json(Node::NamedSpan(s), o, sp)),
        Val::Entries(list) => list_json(list, out, |e, o| write_json(Node::EnumEntry(e), o, sp)),
        Val::AsmInsts(list) => list_json(list, out, |i, o| write_json(Node::AsmInst(i), o, sp)),
        Val::AsmArgs(list) => list_json(list, out, |a, o| write_json(Node::AsmArg(a), o, sp)),
        Val::JsVals(list) => list_json(list, out, |v, o| value_json(Val::JsVal(v), o, sp)),
        Val::Strs(list) => list_json(list, out, |s, o| o.push_str(&crate::js::json_quote(s))),
    }
}

pub fn script_json(script: &Script) -> String {
    let mut out = String::from("{\"type\":\"Script\",\"blocks\":[");
    for (i, node) in script_nodes(script).enumerate() {
        if i > 0 {
            out.push(',');
        }
        to_json(node, &mut out);
    }
    out.push_str("]}");
    out
}
