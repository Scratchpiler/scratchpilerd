use crate::ast::*;
use crate::data;
use crate::js;
use crate::lexer::{Part, Tk, Token, tokenize};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParseError {
    pub line: u32,
    pub col: u32,
    pub len: u32,
    pub message: String,
}

pub fn effective_len(len: Option<u32>) -> u32 {
    match len {
        Some(n) if n > 0 => n,
        _ => 1,
    }
}

#[derive(Clone, Copy)]
enum Val<'a> {
    Str(&'a str),
    Num(f64),
}

const REPORTER_KEYWORDS: &[&str] = &[
    "touching",
    "key",
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
];

pub struct Parser<'a> {
    src: &'a str,
    tokens: &'a [Token],
    pos: usize,
    call_ctx: String,
    pub errors: Vec<ParseError>,
}

fn span_of_token(t: &Token) -> Span {
    Span::new(t.line, t.col, t.end_line, t.end_col)
}

const DEFINE_MODIFIERS: [&str; 3] = ["returns", "warp", "noinline"];

impl<'a> Parser<'a> {
    pub fn new(src: &'a str, tokens: &'a [Token]) -> Parser<'a> {
        let errors = tokens
            .iter()
            .filter(|t| t.unterminated)
            .map(|t| ParseError {
                line: t.line,
                col: t.col,
                len: 1,
                message: format!("Unterminated {}", if t.kind == Tk::Var { "variable name" } else { "string literal" }),
            })
            .collect();
        Parser { src, tokens, pos: 0, call_ctx: String::new(), errors }
    }

    #[inline]
    fn peek(&self) -> &'a Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    #[inline]
    fn at(&self, offset: usize) -> Option<&'a Token> {
        self.tokens.get(self.pos + offset)
    }

    #[inline]
    fn text(&self, t: &'a Token) -> &'a str {
        t.text(self.src)
    }

    fn val(&self, t: &'a Token) -> Val<'a> {
        if t.kind == Tk::Num { Val::Num(t.num) } else { Val::Str(self.text(t)) }
    }

    fn jsval(&self, t: &'a Token) -> JsVal {
        match self.val(t) {
            Val::Str(s) => JsVal::Str(s.into()),
            Val::Num(n) => JsVal::Num(n),
        }
    }

    fn value_len(&self, t: &'a Token) -> Option<u32> {
        match self.val(t) {
            Val::Str(s) => Some(js::utf16_len(s)),
            Val::Num(_) => None,
        }
    }

    fn value_or_empty_len(&self, t: &'a Token) -> Option<u32> {
        match self.val(t) {
            Val::Num(n) if n == 0.0 || n.is_nan() => Some(0),
            other => match other {
                Val::Str(s) => Some(js::utf16_len(s)),
                Val::Num(_) => None,
            },
        }
    }

    fn string_len(&self, t: &'a Token) -> u32 {
        match self.val(t) {
            Val::Str(s) => js::utf16_len(s),
            Val::Num(n) => js::number_to_string(n).len() as u32,
        }
    }

    fn value_text(&self, t: &'a Token) -> String {
        match self.val(t) {
            Val::Str(s) => s.to_string(),
            Val::Num(n) => js::number_to_string(n),
        }
    }

    #[inline]
    fn is_value(&self, t: &'a Token, word: &str) -> bool {
        t.kind != Tk::Num && self.text(t) == word
    }

    fn tok(&self, t: &'a Token) -> String {
        match t.kind {
            Tk::Eof => "end of file".to_string(),
            Tk::Str => format!("string \"{}\"", self.text(t)),
            Tk::Num => format!("number {}", js::number_to_string(t.num)),
            Tk::Var => format!("[{}]", self.text(t)),
            Tk::IStr => "\"\"".to_string(),
            _ => format!("\"{}\"", self.text(t)),
        }
    }

    fn push_error(&mut self, line: u32, col: u32, len: Option<u32>, message: String) {
        self.errors.push(ParseError { line, col, len: effective_len(len), message });
    }

    fn eat(&mut self, kind: Tk, ctx: Option<&str>) -> &'a Token {
        let t = self.peek();
        if t.kind == kind {
            self.pos += 1;
            return &self.tokens[self.pos - 1];
        }
        let expected = match kind {
            Tk::LParen => "`(`".to_string(),
            Tk::RParen => "`)`".to_string(),
            Tk::LBrace => "`{`".to_string(),
            Tk::RBrace => "`}`".to_string(),
            Tk::Comma => "`,`".to_string(),
            Tk::Colon => "`:`".to_string(),
            Tk::Semi => "`;`".to_string(),
            Tk::Num => "a number".to_string(),
            Tk::Str => "a string (e.g. \"hello\")".to_string(),
            Tk::Var => "a variable (e.g. [score])".to_string(),
            Tk::Ident => "an identifier".to_string(),
            Tk::Eof => "end of file".to_string(),
            other => format!("\"{}\"", other.js_name()),
        };
        let where_ = ctx.map(str::to_string).unwrap_or_else(|| self.call_ctx.clone());
        let message = if where_.is_empty() {
            format!("Expected {}, got {}", expected, self.tok(t))
        } else {
            format!("{} — expected {}, got {}", where_, expected, self.tok(t))
        };
        let len = self.value_len(t).map(|l| l.max(1));
        self.push_error(t.line, t.col, len, message);
        t
    }

    #[inline]
    fn check(&self, kind: Tk) -> bool {
        self.peek().kind == kind
    }

    #[inline]
    fn check_v(&self, word: &str) -> bool {
        let t = self.peek();
        t.kind != Tk::Str && t.kind != Tk::IStr && self.is_value(t, word)
    }

    fn try_eat(&mut self, kind: Tk) -> bool {
        if self.check(kind) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn try_eat_v(&mut self, word: &str) -> bool {
        if self.check_v(word) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub fn parse_script(&mut self) -> Script {
        let mut script = Script::default();
        while !self.check(Tk::Eof) {
            let first = self.peek();
            let node = self.parse_top_level();
            let last = &self.tokens[(self.pos.max(1) - 1).min(self.tokens.len() - 1)];
            script.spans.push(Span::new(first.line, first.col, last.end_line, last.end_col));
            script.blocks.push(node);
        }
        script
    }

    fn parse_top_level(&mut self) -> Stmt {
        if self.check_v("on") || self.check_v("define") {
            return self.parse_hat_block();
        }
        if self.check_v("scratchroutine") {
            return self.parse_scratchroutine();
        }
        if self.check_v("struct") {
            return self.parse_struct();
        }
        if self.check_v("enum") || self.check_v("enums") {
            return self.parse_enum();
        }
        if self.check(Tk::LBrace) {
            let t = self.peek();
            let body = self.parse_body();
            return Stmt { kind: StmtKind::Orphaned { body }, line: t.line, col: t.col };
        }
        self.parse_statement()
    }

    fn parse_params(&mut self) -> (Vec<JsVal>, Vec<NamedSpan>) {
        let mut params = Vec::new();
        let mut spans = Vec::new();
        while !self.check(Tk::RParen) && !self.check(Tk::Eof) {
            let p = self.peek();
            params.push(self.jsval(p));
            self.pos += 1;
            spans.push(NamedSpan { name: self.jsval(p), span: span_of_token(p) });
            if !self.try_eat(Tk::Comma) {
                break;
            }
        }
        (params, spans)
    }

    fn parse_hat_block(&mut self) -> Stmt {
        if self.check_v("define") {
            self.pos += 1;
            let name_t = self.peek();
            self.pos += 1;
            self.eat(Tk::LParen, Some("`define name(params)`: expected `(` after the block name"));
            let (params, param_spans) = self.parse_params();
            self.eat(Tk::RParen, Some("`define name(params)`: expected `)` to close the parameter list"));
            let mut taken: Vec<&str> = Vec::new();
            while let Some(next) = DEFINE_MODIFIERS.iter().find(|name| !taken.contains(name) && self.check_v(name)) {
                taken.push(next);
                self.pos += 1;
            }
            let body = self.parse_body();
            return Stmt {
                kind: StmtKind::Define(Box::new(DefineBlock {
                    name: self.jsval(name_t),
                    params,
                    body,
                    returns: taken.contains(&"returns"),
                    warp: taken.contains(&"warp"),
                    noinline: taken.contains(&"noinline"),
                    name_end_line: name_t.end_line,
                    name_end_col: name_t.end_col,
                    param_spans,
                })),
                line: name_t.line,
                col: name_t.col,
            };
        }
        self.pos += 1;
        let hat = self.parse_hat_arg();
        let body = self.parse_body();
        let (line, col) = (hat.line, hat.col);
        Stmt { kind: StmtKind::On { hat: Box::new(hat), body }, line, col }
    }

    fn parse_hat_arg(&mut self) -> Hat {
        let t = self.peek();
        let (line, col) = (t.line, t.col);
        let event = if self.check_v("flag") {
            self.pos += 1;
            HatEvent::Flag
        } else if self.check_v("click") {
            self.pos += 1;
            HatEvent::Click
        } else if self.check_v("clone") {
            self.pos += 1;
            HatEvent::Clone
        } else if self.check_v("key") {
            self.pos += 1;
            let key = self.eat(Tk::Str, Some("`on key \"...\"`: expected a quoted key name, e.g. `on key \"space\"`"));
            HatEvent::Key { key: self.jsval(key) }
        } else if self.check_v("receive") {
            self.pos += 1;
            let msg = self.eat(Tk::Str, Some("`on receive \"...\"`: expected a quoted message name"));
            HatEvent::Receive { msg: self.jsval(msg), msg_span: (msg.kind == Tk::Str).then(|| span_of_token(msg)) }
        } else if self.check_v("backdrop") {
            self.pos += 1;
            let backdrop = self.eat(Tk::Str, Some("`on backdrop \"...\"`: expected a quoted backdrop name"));
            HatEvent::Backdrop { backdrop: self.jsval(backdrop) }
        } else if self.check_v("timer") {
            self.pos += 1;
            self.eat(Tk::Gt, Some("`on timer > n`: expected `>` after `timer`"));
            HatEvent::GreaterThan { sense: "TIMER", threshold: Box::new(self.parse_expr()) }
        } else if self.check_v("loudness") {
            self.pos += 1;
            self.eat(Tk::Gt, Some("`on loudness > n`: expected `>` after `loudness`"));
            HatEvent::GreaterThan { sense: "LOUDNESS", threshold: Box::new(self.parse_expr()) }
        } else {
            let len = self.value_len(t).map(|l| l.max(1));
            self.push_error(
                t.line,
                t.col,
                len,
                format!(
                    "Unknown event \"{}\" after `on`. Valid events: flag  click  clone  key \"...\"  receive \"...\"  backdrop \"...\"  timer > n  loudness > n",
                    self.value_text(t)
                ),
            );
            self.pos += 1;
            HatEvent::Unknown
        };
        Hat { event, line, col }
    }

    fn parse_body(&mut self) -> Vec<Stmt> {
        self.eat(Tk::LBrace, Some("expected `{` to open block body"));
        let mut stmts = Vec::new();
        while !self.check(Tk::RBrace) && !self.check(Tk::Eof) {
            stmts.push(self.parse_statement());
        }
        self.eat(Tk::RBrace, Some("expected `}` to close block body"));
        stmts
    }

    fn next_is(&self, word: &str) -> bool {
        self.at(1).is_some_and(|n| self.is_value(n, word))
    }

    fn parse_statement(&mut self) -> Stmt {
        let t = self.peek();
        let v = self.val(t);
        let word = match v {
            Val::Str(s) => s,
            Val::Num(_) => "",
        };
        if matches!(v, Val::Str(_)) {
            match word {
                "if" | "elif" => return self.parse_if(),
                "repeat" if self.next_is("until") => return self.parse_repeat_until(),
                "repeat" => return self.parse_repeat(),
                "forever" => return self.parse_forever(),
                "wait" if self.next_is("until") => return self.parse_wait_until(),
                "while" => return self.parse_while(),
                "for" => return self.parse_for(),
                "pyfor" => return self.parse_pyfor(),
                "scratchroutine" => return self.parse_scratchroutine(),
                "launch" => return self.parse_launch_await(false),
                "await" => return self.parse_launch_await(true),
                "cancel" => return self.parse_cancel(),
                "breakpoint" => {
                    self.pos += 1;
                    return Stmt { kind: StmtKind::Breakpoint, line: t.line, col: t.col };
                }
                "__asm__" => return self.parse_asm(),
                "return" => {
                    self.pos += 1;
                    let value = if self.check(Tk::RBrace) || self.check(Tk::Eof) || self.check(Tk::Semi) || self.peek().line > t.line {
                        None
                    } else {
                        Some(self.parse_expr())
                    };
                    self.try_eat(Tk::Semi);
                    return Stmt { kind: StmtKind::Return { value }, line: t.line, col: t.col };
                }
                "match" | "switch" => return self.parse_match(),
                "do" => return self.parse_do_while(),
                "break" => {
                    self.pos += 1;
                    return Stmt { kind: StmtKind::Break, line: t.line, col: t.col };
                }
                "continue" => {
                    self.pos += 1;
                    return Stmt { kind: StmtKind::Continue, line: t.line, col: t.col };
                }
                _ => {}
            }
        }
        self.parse_simple_statement()
    }

    fn parse_match(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        let subject = self.parse_expr();
        self.eat(Tk::LBrace, Some("`match expr { … }`: expected `{` after the match subject"));
        let mut cases = Vec::new();
        let mut default_body = None;
        while !self.check(Tk::RBrace) && !self.check(Tk::Eof) {
            if self.check_v("case") {
                let ct = self.peek();
                self.pos += 1;
                let mut values = vec![self.parse_expr()];
                while self.try_eat(Tk::Comma) {
                    values.push(self.parse_expr());
                }
                let body = self.parse_body();
                cases.push(Case { values, body, line: ct.line, col: ct.col });
            } else if self.check_v("default") {
                self.pos += 1;
                default_body = Some(self.parse_body());
            } else {
                let bad = self.peek();
                let len = self.string_len(bad).max(1);
                let message = format!("Inside `match`: expected `case <value> {{ … }}` or `default {{ … }}` — got {}", self.tok(bad));
                self.push_error(bad.line, bad.col, Some(len), message);
                self.pos += 1;
            }
        }
        self.eat(Tk::RBrace, Some("`match`: expected `}` to close the match block"));
        if cases.is_empty() {
            let len = self.value_len(t);
            self.push_error(t.line, t.col, len, "`match` needs at least one `case <value> { … }` arm".to_string());
        }
        Stmt { kind: StmtKind::Match { subject, cases, default_body }, line: t.line, col: t.col }
    }

    fn parse_do_while(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        let body = self.parse_body();
        if !self.check_v("while") {
            let bad = self.peek();
            let len = self.string_len(bad).max(1);
            self.push_error(bad.line, bad.col, Some(len), "`do { … } while cond`: expected `while` after the body".to_string());
        } else {
            self.pos += 1;
        }
        let cond = self.parse_expr();
        Stmt { kind: StmtKind::DoWhile { cond, body }, line: t.line, col: t.col }
    }

    fn parse_if(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        let cond = self.parse_expr();
        let then = self.parse_body();
        let mut alt = None;
        if self.check_v("else") {
            self.pos += 1;
            if self.check_v("if") || self.check_v("elif") {
                alt = Some(vec![self.parse_if()]);
            } else {
                alt = Some(self.parse_body());
            }
        }
        Stmt { kind: StmtKind::If { cond, then, alt }, line: t.line, col: t.col }
    }

    fn parse_repeat(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        let count = self.parse_expr();
        let nounroll = self.try_eat_v("nounroll");
        let body = self.parse_body();
        Stmt { kind: StmtKind::Repeat { count, body, nounroll }, line: t.line, col: t.col }
    }

    fn parse_repeat_until(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 2;
        self.eat(Tk::LParen, Some("`repeat until (condition)`: expected `(` before the condition"));
        let cond = self.parse_expr();
        self.eat(Tk::RParen, Some("`repeat until (condition)`: expected `)` after the condition"));
        let body = self.parse_body();
        Stmt { kind: StmtKind::RepeatUntil { cond, body }, line: t.line, col: t.col }
    }

    fn parse_forever(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        let body = self.parse_body();
        Stmt { kind: StmtKind::Forever { body }, line: t.line, col: t.col }
    }

    fn parse_wait_until(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 2;
        let cond = self.parse_expr();
        Stmt { kind: StmtKind::WaitUntil { cond }, line: t.line, col: t.col }
    }

    fn parse_while(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        self.eat(Tk::LParen, Some("`while (condition)`: expected `(` before the condition"));
        let cond = self.parse_expr();
        self.eat(Tk::RParen, Some("`while (condition)`: expected `)` after the condition"));
        let body = self.parse_body();
        Stmt { kind: StmtKind::While { cond, body }, line: t.line, col: t.col }
    }

    fn parse_for(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        if !self.check(Tk::Var) {
            self.push_error(t.line, t.col, Some(3), "`for [var] from expr to expr {}`: expected `[variable]` after `for`".to_string());
        }
        let var_tok = if self.check(Tk::Var) { Some(self.eat(Tk::Var, None)) } else { None };
        let var_name: Box<str> = var_tok.map(|v| self.text(v).into()).unwrap_or_else(|| "_err_".into());
        if !self.check_v("from") {
            let p = self.peek();
            let len = self.value_len(p);
            self.push_error(p.line, p.col, len, "`for [var] from expr to expr {}`: expected `from` after variable".to_string());
        } else {
            self.pos += 1;
        }
        let from = self.parse_expr();
        if !self.check_v("to") {
            let p = self.peek();
            let len = self.value_len(p);
            self.push_error(p.line, p.col, len, "`for [var] from expr to expr {}`: expected `to` after start expression".to_string());
        } else {
            self.pos += 1;
        }
        let to = self.parse_expr();
        let nounroll = self.try_eat_v("nounroll");
        let body = self.parse_body();
        Stmt { kind: StmtKind::For { var_name, from, to, body, nounroll, var_span: var_tok.map(span_of_token) }, line: t.line, col: t.col }
    }

    fn parse_pyfor(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        if !self.check(Tk::Var) {
            self.push_error(t.line, t.col, Some(5), "`pyfor [var] in [list] {}`: expected `[iterator variable]` after `pyfor`".to_string());
        }
        let var_tok = if self.check(Tk::Var) { Some(self.eat(Tk::Var, None)) } else { None };
        if !self.check_v("in") {
            let p = self.peek();
            let len = self.value_len(p).map(|l| l.max(1));
            self.push_error(p.line, p.col, len, "`pyfor [var] in [list] {}`: expected keyword `in` after the iterator variable".to_string());
        } else {
            self.pos += 1;
        }
        if !self.check(Tk::Var) {
            let p = self.peek();
            let len = self.value_len(p).map(|l| l.max(1));
            self.push_error(p.line, p.col, len, "`pyfor [var] in [list] {}`: expected `[list variable]` after `in`".to_string());
        }
        let list_tok = if self.check(Tk::Var) { Some(self.eat(Tk::Var, None)) } else { None };
        let body = self.parse_body();
        Stmt {
            kind: StmtKind::PyFor {
                var_name: var_tok.map(|v| self.text(v).into()).unwrap_or_else(|| "_err_".into()),
                list_name: list_tok.map(|v| self.text(v).into()).unwrap_or_else(|| "_err_".into()),
                body,
                var_span: var_tok.map(span_of_token),
                list_span: list_tok.map(span_of_token),
            },
            line: t.line,
            col: t.col,
        }
    }

    fn parse_scratchroutine(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        let name_tok = self.peek();
        if name_tok.kind != Tk::Ident {
            self.push_error(t.line, t.col, Some(13), "`scratchroutine name(params) {}`: expected a routine name after `scratchroutine`".to_string());
        }
        let name: Box<str> = if name_tok.kind == Tk::Ident {
            self.pos += 1;
            self.text(name_tok).into()
        } else {
            "_err_".into()
        };
        let (mut params, mut param_spans) = (Vec::new(), Vec::new());
        if self.check(Tk::LParen) {
            self.eat(Tk::LParen, None);
            (params, param_spans) = self.parse_params();
            self.eat(Tk::RParen, None);
        }
        let body = self.parse_body();
        Stmt {
            kind: StmtKind::Routine(Box::new(Routine {
                name,
                params,
                body,
                name_span: (name_tok.kind == Tk::Ident).then(|| span_of_token(name_tok)),
                param_spans,
            })),
            line: t.line,
            col: t.col,
        }
    }

    fn parse_launch_await(&mut self, wait: bool) -> Stmt {
        let mode = if wait { "await" } else { "launch" };
        let t = self.peek();
        self.pos += 1;
        let name_tok = self.peek();
        if name_tok.kind != Tk::Ident {
            self.push_error(t.line, t.col, Some(mode.len() as u32), format!("`{mode} name(args)`: expected a scratchroutine name"));
        }
        let name: Box<str> = if name_tok.kind == Tk::Ident {
            self.pos += 1;
            self.text(name_tok).into()
        } else {
            "_err_".into()
        };
        let mut args = Vec::new();
        if self.check(Tk::LParen) {
            self.eat(Tk::LParen, None);
            args = self.parse_args();
            self.eat(Tk::RParen, None);
        }
        Stmt {
            kind: StmtKind::LaunchAwait { wait, name, args, name_span: (name_tok.kind == Tk::Ident).then(|| span_of_token(name_tok)) },
            line: t.line,
            col: t.col,
        }
    }

    fn parse_args(&mut self) -> Vec<Expr> {
        let mut args = Vec::new();
        while !self.check(Tk::RParen) && !self.check(Tk::Eof) {
            args.push(self.parse_expr());
            if !self.try_eat(Tk::Comma) {
                break;
            }
        }
        args
    }

    fn parse_cancel(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        let name_tok = self.peek();
        if name_tok.kind != Tk::Ident {
            self.push_error(t.line, t.col, Some(6), "`cancel name`: expected a scratchroutine name after `cancel`".to_string());
        }
        let name: Box<str> = if name_tok.kind == Tk::Ident {
            self.pos += 1;
            self.text(name_tok).into()
        } else {
            "_err_".into()
        };
        Stmt { kind: StmtKind::Cancel { name, name_span: (name_tok.kind == Tk::Ident).then(|| span_of_token(name_tok)) }, line: t.line, col: t.col }
    }

    fn parse_asm(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        if !self.check_v("volatile") {
            let p = self.peek();
            let len = self.value_or_empty_len(p).map(|l| l.max(1));
            self.push_error(p.line, p.col, len, "`__asm__ volatile(...)`: expected `volatile` immediately after `__asm__`".to_string());
        } else {
            self.pos += 1;
        }
        let mut unsafe_mode = false;
        if self.check_v("unsafe") {
            self.pos += 1;
            unsafe_mode = true;
        }
        let ctx = if self.check(Tk::LBrace) {
            "`__asm__ volatile(...)` uses parens `(...)`, not braces like other blocks"
        } else {
            "`__asm__ volatile(...)`: expected `(` to open the asm block"
        };
        self.eat(Tk::LParen, Some(ctx));
        let mut statements = Vec::new();
        while !self.check(Tk::RParen) && !self.check(Tk::Eof) {
            if let Some(s) = self.parse_asm_inner() {
                statements.push(s);
            }
        }
        self.eat(Tk::RParen, Some("`__asm__ volatile(...)`: expected `)` to close the asm block"));
        Stmt { kind: StmtKind::Asm { unsafe_mode, statements }, line: t.line, col: t.col }
    }

    fn parse_asm_inner(&mut self) -> Option<AsmInst> {
        let op = self.peek();
        if op.kind != Tk::Ident {
            let len = self.value_or_empty_len(op).map(|l| l.max(1));
            let message = format!("__asm__: expected an opcode name (e.g. `looks_say`), got {}", self.tok(op));
            self.push_error(op.line, op.col, len, message);
            self.pos += 1;
            return None;
        }
        let opcode = self.text(op);
        self.pos += 1;
        self.eat(Tk::LParen, Some(&format!("__asm__: expected `(` after opcode `{opcode}`")));
        let mut args = Vec::new();
        while !self.check(Tk::RParen) && !self.check(Tk::Eof) {
            args.push(self.parse_asm_arg());
            if !self.try_eat(Tk::Comma) {
                break;
            }
        }
        self.eat(Tk::RParen, Some(&format!("__asm__: expected `)` to close arguments for `{opcode}`")));
        if !self.try_eat(Tk::Semi) {
            let p = self.peek();
            self.push_error(p.line, p.col, Some(1), format!("__asm__: expected `;` after `{opcode}(...)`"));
        }
        Some(AsmInst { opcode: opcode.into(), args, line: op.line, col: op.col })
    }

    fn parse_asm_arg(&mut self) -> (AsmArg, u32, u32) {
        let t = self.peek();
        let at = |arg| (arg, t.line, t.col);
        if self.check(Tk::Str) {
            self.pos += 1;
            return at(AsmArg::Str(self.text(t).into()));
        }
        if self.check(Tk::Num) {
            self.pos += 1;
            return at(AsmArg::Num(t.num));
        }
        if self.check(Tk::Minus) && self.at(1).is_some_and(|n| n.kind == Tk::Num) {
            self.pos += 1;
            let n = self.eat(Tk::Num, None);
            return at(AsmArg::Num(-n.num));
        }
        if self.check(Tk::Ident) && (self.text(t) == "true" || self.text(t) == "false") {
            self.pos += 1;
            return at(AsmArg::Bool(self.text(t) == "true"));
        }
        if self.check(Tk::Var) {
            self.pos += 1;
            return at(AsmArg::Reg(self.text(t).into()));
        }
        if self.check(Tk::Ident) {
            if self.at(1).is_some_and(|n| n.kind == Tk::LParen) {
                let name = self.text(t);
                self.push_error(
                    t.line,
                    t.col,
                    Some(js::utf16_len(name)),
                    format!(
                        "__asm__: nested opcode calls are not supported as arguments (v1) — `{name}(...)` must be its own statement; args must be a literal or a bare register name"
                    ),
                );
                self.pos += 1;
                let mut depth = 1;
                self.pos += 1;
                while depth > 0 && !self.check(Tk::Eof) {
                    if self.check(Tk::LParen) {
                        depth += 1;
                    } else if self.check(Tk::RParen) {
                        depth -= 1;
                    }
                    self.pos += 1;
                }
                return at(AsmArg::Num(0.0));
            }
            self.pos += 1;
            return at(AsmArg::Reg(self.text(t).into()));
        }
        let len = self.value_or_empty_len(t).map(|l| l.max(1));
        let message = format!("__asm__: expected a literal (string/number/boolean) or a bare register name, got {}", self.tok(t));
        self.push_error(t.line, t.col, len, message);
        self.pos += 1;
        at(AsmArg::Num(0.0))
    }

    fn parse_enum(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        let mut entries = Vec::new();
        self.eat(Tk::LBrace, Some("`enum { ... }` — expected `{` to open enum body"));
        while !self.check(Tk::RBrace) && !self.check(Tk::Eof) {
            let name_tok = self.peek();
            if name_tok.kind != Tk::Ident {
                self.push_error(name_tok.line, name_tok.col, Some(1), "`enum { name = value, ... }`: expected an identifier for the entry name".to_string());
                self.pos += 1;
                self.try_eat(Tk::Comma);
                continue;
            }
            self.pos += 1;
            let name = self.text(name_tok);
            let (value, value_line, value_col);
            if self.try_eat(Tk::Eq) {
                let v = self.peek();
                if self.check(Tk::Num) {
                    self.pos += 1;
                    value = EnumValue::Num(v.num);
                } else if self.check(Tk::Str) {
                    self.pos += 1;
                    value = EnumValue::Str(self.text(v).into());
                } else {
                    self.push_error(v.line, v.col, Some(1), format!("enum entry `{name}`: value must be a number or string literal"));
                    value = EnumValue::Num(0.0);
                }
                (value_line, value_col) = (v.line, v.col);
            } else {
                value = EnumValue::Num(0.0);
                (value_line, value_col) = (name_tok.line, name_tok.col);
            }
            entries.push(EnumEntry { name: name.into(), value, value_line, value_col, span: span_of_token(name_tok) });
            self.try_eat(Tk::Comma);
        }
        self.eat(Tk::RBrace, Some("`enum { ... }` — expected `}` to close enum body"));
        Stmt { kind: StmtKind::Enum { entries }, line: t.line, col: t.col }
    }

    fn parse_struct(&mut self) -> Stmt {
        let t = self.peek();
        self.pos += 1;
        let name_tok = self.peek();
        if name_tok.kind != Tk::Ident {
            self.push_error(t.line, t.col, Some(6), "`struct name { fields }`: expected a name after `struct`".to_string());
        }
        let name: Box<str> = if name_tok.kind == Tk::Ident {
            self.pos += 1;
            self.text(name_tok).into()
        } else {
            "_err_".into()
        };
        let mut fields = Vec::new();
        let mut field_spans = Vec::new();
        self.eat(Tk::LBrace, Some("`struct` declaration — expected `{` after the struct name"));
        while !self.check(Tk::RBrace) && !self.check(Tk::Eof) {
            let f = self.peek();
            if f.kind == Tk::Ident {
                fields.push(self.text(f).into());
                self.pos += 1;
                field_spans.push(NamedSpan { name: self.jsval(f), span: span_of_token(f) });
            } else {
                self.push_error(f.line, f.col, Some(1), "Expected a struct field name".to_string());
                self.pos += 1;
            }
            self.try_eat(Tk::Comma);
        }
        self.eat(Tk::RBrace, Some("`struct` declaration — expected `}` to close field list"));
        Stmt {
            kind: StmtKind::Struct { name, fields, name_span: (name_tok.kind == Tk::Ident).then(|| span_of_token(name_tok)), field_spans },
            line: t.line,
            col: t.col,
        }
    }

    fn var_expr(&self, t: &'a Token) -> Expr {
        Expr::Var { name: self.text(t).into(), line: t.line, col: t.col, end_line: t.end_line, end_col: t.end_col }
    }

    fn parse_simple_statement(&mut self) -> Stmt {
        let t = self.peek();
        let (line, col) = (t.line, t.col);

        if t.kind == Tk::Var && self.at(1).is_some_and(|n| n.kind == Tk::Dot) {
            self.pos += 2;
            let m = self.peek();
            let method: &str = if m.kind == Tk::Ident { self.text(m) } else { "" };
            if m.kind == Tk::Ident {
                self.pos += 1;
            }
            let name = self.text(t);
            self.eat(Tk::LParen, Some(&format!("`[{name}].{method}(...)`: expected `(`")));
            let args = self.parse_args();
            self.eat(Tk::RParen, Some(&format!("`[{name}].{method}(...)`: expected `)`")));
            return Stmt { kind: StmtKind::MemberCall { object: self.var_expr(t), method: method.into(), args }, line, col };
        }

        if t.kind == Tk::Ident && self.text(t) == "pen" && self.at(1).is_some_and(|n| n.kind == Tk::Dot) {
            self.pos += 2;
            let m = self.peek();
            let is_word = m.kind == Tk::Ident || (m.kind != Tk::Str && m.kind != Tk::Num && data::is_keyword(self.text(m)));
            let method: &str = if is_word { self.text(m) } else { "" };
            if is_word {
                self.pos += 1;
            }
            let simple = |p: &mut Self, kind: Simple, names: &[F]| -> Stmt {
                let fields = p.call_args(names);
                Stmt { kind: StmtKind::Simple { kind, fields }, line, col }
            };
            match method {
                "down" => return simple(self, Simple::PenDown, &[]),
                "up" => return simple(self, Simple::PenUp, &[]),
                "clear" => return simple(self, Simple::PenClear, &[]),
                "stamp" => return simple(self, Simple::PenStamp, &[]),
                "setColor" => return simple(self, Simple::SetPenColor, &[F::Color]),
                "setSize" => return simple(self, Simple::SetPenSize, &[F::Value]),
                "changeSize" => return simple(self, Simple::ChangePenSize, &[F::Value]),
                "setColorParam" => return simple(self, Simple::SetPenColorParam, &[F::Param, F::Value]),
                "changeColorParam" => return simple(self, Simple::ChangePenColorParam, &[F::Param, F::Amount]),
                _ => {}
            }
            let known = ["down", "up", "clear", "stamp", "setColor", "setSize", "changeSize", "setColorParam", "changeColorParam"];
            let similar = fuzzy_match(method, known.iter().copied(), 3);
            let hint = if !similar.is_empty() {
                format!("  Did you mean: {}?", similar.iter().map(|k| format!("pen.{k}()")).collect::<Vec<_>>().join("  or  "))
            } else {
                format!("  Known pen.* methods: {}", known.iter().map(|k| format!("pen.{k}()")).collect::<Vec<_>>().join(", "))
            };
            let (el, ec) = (if m.line != 0 { m.line } else { line }, if m.col != 0 { m.col } else { col });
            let len = js::utf16_len(method);
            self.push_error(el, ec, Some(if len == 0 { 1 } else { len }), format!("Unknown method `pen.{method}()`.{hint}"));
            return Stmt { kind: StmtKind::Unknown { value: JsVal::Str(format!("pen.{method}").into()) }, line, col };
        }

        if t.kind == Tk::Var {
            let (n1, n2) = (self.at(1), self.at(2));
            let var_span = Some(span_of_token(t));
            if let (Some(a), Some(b)) = (n1, n2) {
                if a.kind == Tk::Plus && b.kind == Tk::Plus {
                    self.pos += 3;
                    return Stmt {
                        kind: StmtKind::ChangeVar { var_name: self.text(t).into(), value: Expr::Num { value: 1.0, line, col }, var_span },
                        line,
                        col,
                    };
                }
                if a.kind == Tk::Minus && b.kind == Tk::Minus {
                    self.pos += 3;
                    return Stmt {
                        kind: StmtKind::ChangeVar { var_name: self.text(t).into(), value: Expr::Num { value: -1.0, line, col }, var_span },
                        line,
                        col,
                    };
                }
                let op = match a.kind {
                    Tk::Plus => Some(BinOp::Add),
                    Tk::Minus => Some(BinOp::Sub),
                    Tk::Star => Some(BinOp::Mul),
                    Tk::Slash => Some(BinOp::Div),
                    _ => None,
                };
                if let Some(op) = op.filter(|_| b.kind == Tk::Eq) {
                    self.pos += 3;
                    let rhs = self.parse_expr();
                    let var_name: Box<str> = self.text(t).into();
                    if op == BinOp::Add {
                        return Stmt { kind: StmtKind::ChangeVar { var_name, value: rhs, var_span }, line, col };
                    }
                    let var_ref = Expr::Var { name: var_name.clone(), line, col, end_line: 0, end_col: 0 };
                    let value = Expr::Bin { op, left: Box::new(var_ref), right: Box::new(rhs) };
                    return Stmt { kind: StmtKind::SetVar { var_name, value, var_span }, line, col };
                }
            }
        }

        if t.kind == Tk::Ident {
            self.pos += 1;
            let name = self.text(t);
            if self.check(Tk::LParen) {
                self.eat(Tk::LParen, None);
                let args = self.parse_args();
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::Call { name: name.into(), args }, line, col };
            }
            return Stmt { kind: StmtKind::RawKeyword { value: name.into() }, line, col };
        }

        self.pos += 1;
        self.parse_keyword_statement(t)
    }

    fn call_args(&mut self, names: &[F]) -> Vec<(F, Expr)> {
        self.eat(Tk::LParen, None);
        let mut fields = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            if i > 0 {
                self.eat(Tk::Comma, None);
            }
            fields.push((*name, self.parse_expr()));
        }
        self.eat(Tk::RParen, None);
        fields
    }

    fn eat_var_with_span(&mut self) -> (JsVal, Option<Span>) {
        let t = self.eat(Tk::Var, None);
        (self.jsval(t), (t.kind == Tk::Var).then(|| span_of_token(t)))
    }

    fn parse_keyword_statement(&mut self, t: &'a Token) -> Stmt {
        let (line, col) = (t.line, t.col);
        let v = self.val(t);
        let word: &str = match v {
            Val::Str(s) => s,
            Val::Num(_) => "\u{0}",
        };
        let v_text = self.value_text(t);
        let truthy = match v {
            Val::Str(s) => !s.is_empty(),
            Val::Num(n) => n != 0.0 && !n.is_nan(),
        };
        self.call_ctx = match data::call_sig(word) {
            Some(sig) => format!("`{sig}`"),
            None if truthy => format!("`{v_text}(...)`"),
            None => String::new(),
        };

        let simple = |p: &mut Self, kind: Simple, names: &[F]| -> Stmt {
            let fields = p.call_args(names);
            Stmt { kind: StmtKind::Simple { kind, fields }, line, col }
        };
        let simple_kind = match word {
            "move" | "step" | "forward" => Some((Simple::Move, &[F::Steps][..])),
            "turnRight" | "right" => Some((Simple::TurnRight, &[F::Degrees][..])),
            "turnLeft" | "left" => Some((Simple::TurnLeft, &[F::Degrees][..])),
            "bounce" => Some((Simple::Bounce, &[][..])),
            "setX" => Some((Simple::SetX, &[F::Value][..])),
            "setY" => Some((Simple::SetY, &[F::Value][..])),
            "changeX" => Some((Simple::ChangeX, &[F::Value][..])),
            "changeY" => Some((Simple::ChangeY, &[F::Value][..])),
            "say" | "print" | "println" => Some((Simple::Say, &[F::Msg][..])),
            "sayFor" => Some((Simple::SayFor, &[F::Msg, F::Secs][..])),
            "think" => Some((Simple::Think, &[F::Msg][..])),
            "thinkFor" => Some((Simple::ThinkFor, &[F::Msg, F::Secs][..])),
            "switchCostume" => Some((Simple::SwitchCostume, &[F::Name][..])),
            "switchBackdrop" => Some((Simple::SwitchBackdrop, &[F::Name][..])),
            "nextCostume" => Some((Simple::NextCostume, &[][..])),
            "nextBackdrop" => Some((Simple::NextBackdrop, &[][..])),
            "setSize" => Some((Simple::SetSize, &[F::Value][..])),
            "changeSize" => Some((Simple::ChangeSize, &[F::Value][..])),
            "show" => Some((Simple::Show, &[][..])),
            "hide" => Some((Simple::Hide, &[][..])),
            "clearEffects" => Some((Simple::ClearEffects, &[][..])),
            "play" => Some((Simple::Play, &[F::Sound][..])),
            "playUntilDone" => Some((Simple::PlayUntilDone, &[F::Sound][..])),
            "stopSounds" => Some((Simple::StopSounds, &[][..])),
            "broadcast" | "send" => Some((Simple::Broadcast, &[F::Msg][..])),
            "broadcastAndWait" | "sendAndWait" => Some((Simple::BroadcastWait, &[F::Msg][..])),
            "wait" => Some((Simple::Wait, &[F::Duration][..])),
            "stopAll" => Some((Simple::StopAll, &[][..])),
            "stopThis" | "stopMe" => Some((Simple::StopThis, &[][..])),
            "stopOtherScripts" => Some((Simple::StopOther, &[][..])),
            "deleteClone" => Some((Simple::DeleteClone, &[][..])),
            "setDirection" | "turnTo" => Some((Simple::SetDirection, &[F::Degrees][..])),
            "pointTowards" => Some((Simple::PointTowards, &[F::Target][..])),
            "moveForward" => Some((Simple::MoveForwardLayers, &[F::Layers][..])),
            "moveBackward" => Some((Simple::MoveBackwardLayers, &[F::Layers][..])),
            "goToFront" | "front" => Some((Simple::GoToFront, &[][..])),
            "goToBack" | "back" => Some((Simple::GoToBack, &[][..])),
            "setEffect" => Some((Simple::SetEffect, &[F::Effect, F::Value][..])),
            "changeEffect" => Some((Simple::ChangeEffect, &[F::Effect, F::Amount][..])),
            "setVolume" => Some((Simple::SetVolume, &[F::Value][..])),
            "changeVolume" => Some((Simple::ChangeVolume, &[F::Value][..])),
            "askAndWait" | "ask" => Some((Simple::AskAndWait, &[F::Question][..])),
            "resetTimer" => Some((Simple::ResetTimer, &[][..])),
            "setDragMode" => Some((Simple::SetDragMode, &[F::Mode][..])),
            "penDown" | "down" => Some((Simple::PenDown, &[][..])),
            "penUp" | "up" => Some((Simple::PenUp, &[][..])),
            "penClear" => Some((Simple::PenClear, &[][..])),
            "stamp" => Some((Simple::PenStamp, &[][..])),
            "setPenColor" => Some((Simple::SetPenColor, &[F::Color][..])),
            "setPenSize" => Some((Simple::SetPenSize, &[F::Value][..])),
            "changePenSize" => Some((Simple::ChangePenSize, &[F::Value][..])),
            "setPenColorParam" => Some((Simple::SetPenColorParam, &[F::Param, F::Value][..])),
            "changePenColorParam" => Some((Simple::ChangePenColorParam, &[F::Param, F::Amount][..])),
            "setRotationStyle" => Some((Simple::SetRotationStyle, &[F::Style][..])),
            "switchBackdropAndWait" => Some((Simple::SwitchBackdropWait, &[F::Name][..])),
            "setSoundEffect" => Some((Simple::SetSoundEffect, &[F::Effect, F::Value][..])),
            "changeSoundEffect" => Some((Simple::ChangeSoundEffect, &[F::Effect, F::Value][..])),
            "clearSoundEffects" => Some((Simple::ClearSoundEffects, &[][..])),
            "checkCancel" => Some((Simple::CheckCancel, &[][..])),
            _ => None,
        };
        if let Some((kind, names)) = simple_kind {
            return simple(self, kind, names);
        }

        match word {
            "goTo" => {
                self.eat(Tk::LParen, None);
                let first = self.parse_expr();
                if self.try_eat(Tk::Comma) {
                    let y = self.parse_expr();
                    self.eat(Tk::RParen, None);
                    return Stmt { kind: StmtKind::Simple { kind: Simple::GoToXY, fields: vec![(F::X, first), (F::Y, y)] }, line, col };
                }
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::Simple { kind: Simple::GoTo, fields: vec![(F::Target, first)] }, line, col };
            }
            "glide" => {
                self.eat(Tk::LParen, None);
                let secs = self.parse_expr();
                self.eat(Tk::Comma, None);
                let first = self.parse_expr();
                if !self.try_eat(Tk::Comma) {
                    self.eat(Tk::RParen, None);
                    return Stmt { kind: StmtKind::Simple { kind: Simple::GlideTo, fields: vec![(F::Secs, secs), (F::Target, first)] }, line, col };
                }
                let y = self.parse_expr();
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::Simple { kind: Simple::Glide, fields: vec![(F::Secs, secs), (F::X, first), (F::Y, y)] }, line, col };
            }
            "createClone" => {
                self.eat(Tk::LParen, None);
                if self.check(Tk::RParen) {
                    self.eat(Tk::RParen, None);
                    return Stmt { kind: StmtKind::Simple { kind: Simple::CreateCloneMyself, fields: vec![] }, line, col };
                }
                let target = self.parse_expr();
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::Simple { kind: Simple::CreateClone, fields: vec![(F::Target, target)] }, line, col };
            }
            "clone" => {
                self.eat(Tk::LParen, None);
                self.eat(Tk::RParen, None);
                let target = Expr::Str { value: "_myself_".into(), line, col, end_line: 0, end_col: 0 };
                return Stmt { kind: StmtKind::Simple { kind: Simple::CreateClone, fields: vec![(F::Target, target)] }, line, col };
            }
            "listDeleteAll" | "clear" | "pop" => {
                self.eat(Tk::LParen, None);
                let (list_name, list_span) = self.eat_var_with_span();
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::List { op: ListOp::DeleteAll, list_name, list_span, fields: vec![] }, line, col };
            }
            "set" | "change" => {
                let is_set = word == "set";
                if is_set && self.check(Tk::Star) {
                    self.pos += 1;
                    let addr = self.parse_primary_expr();
                    self.try_eat_v("to");
                    let value = self.parse_expr();
                    return Stmt { kind: StmtKind::Simple { kind: Simple::DerefSet, fields: vec![(F::Addr, addr), (F::Value, value)] }, line, col };
                }
                if !self.check(Tk::Var) {
                    let example = if is_set {
                        "`set` must be followed by a [variable], e.g. `set [score] to 0`"
                    } else {
                        "`change` must be followed by a [variable], e.g. `change [score] by 1`"
                    };
                    let message = format!("{example} — got {}", self.tok(self.peek()));
                    self.push_error(line, col, Some(js::utf16_len(word)), message);
                    return Stmt { kind: StmtKind::Unknown { value: JsVal::Str(word.into()) }, line, col };
                }
                let var_t = self.eat(Tk::Var, None);
                self.try_eat_v(if is_set { "to" } else { "by" });
                let value = self.parse_expr();
                let var_name: Box<str> = self.text(var_t).into();
                let var_span = Some(span_of_token(var_t));
                let kind = if is_set { StmtKind::SetVar { var_name, value, var_span } } else { StmtKind::ChangeVar { var_name, value, var_span } };
                return Stmt { kind, line, col };
            }
            "showVariable" | "hideVariable" | "showList" | "hideList" => {
                let kind = match word {
                    "showVariable" => Visibility::ShowVar,
                    "hideVariable" => Visibility::HideVar,
                    "showList" => Visibility::ShowList,
                    _ => Visibility::HideList,
                };
                self.eat(Tk::LParen, None);
                let (name, name_span) = self.eat_var_with_span();
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::Visibility { kind, name, name_span }, line, col };
            }
            "listAdd" => {
                self.eat(Tk::LParen, None);
                let item = self.parse_expr();
                self.eat(Tk::Comma, None);
                let (list_name, list_span) = self.eat_var_with_span();
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::List { op: ListOp::Add, list_name, list_span, fields: vec![(F::Item, item)] }, line, col };
            }
            "listDelete" => {
                self.eat(Tk::LParen, None);
                let index = self.parse_expr();
                self.eat(Tk::Comma, None);
                let (list_name, list_span) = self.eat_var_with_span();
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::List { op: ListOp::Delete, list_name, list_span, fields: vec![(F::Index, index)] }, line, col };
            }
            "listInsert" => {
                self.eat(Tk::LParen, None);
                let item = self.parse_expr();
                self.eat(Tk::Comma, None);
                let index = self.parse_expr();
                self.eat(Tk::Comma, None);
                let (list_name, list_span) = self.eat_var_with_span();
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::List { op: ListOp::Insert, list_name, list_span, fields: vec![(F::Item, item), (F::Index, index)] }, line, col };
            }
            "listReplace" => {
                self.eat(Tk::LParen, None);
                let index = self.parse_expr();
                self.eat(Tk::Comma, None);
                let (list_name, list_span) = self.eat_var_with_span();
                self.eat(Tk::Comma, None);
                let item = self.parse_expr();
                self.eat(Tk::RParen, None);
                return Stmt {
                    kind: StmtKind::List { op: ListOp::Replace, list_name, list_span, fields: vec![(F::Index, index), (F::Item, item)] },
                    line,
                    col,
                };
            }
            "append" | "push" => {
                self.eat(Tk::LParen, None);
                let (list_name, list_span) = self.eat_var_with_span();
                self.eat(Tk::Comma, None);
                let item = self.parse_expr();
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::List { op: ListOp::Add, list_name, list_span, fields: vec![(F::Item, item)] }, line, col };
            }
            "remove" => {
                self.eat(Tk::LParen, None);
                let (list_name, list_span) = self.eat_var_with_span();
                self.eat(Tk::Comma, None);
                let index = self.parse_expr();
                self.eat(Tk::RParen, None);
                return Stmt { kind: StmtKind::List { op: ListOp::Delete, list_name, list_span, fields: vec![(F::Index, index)] }, line, col };
            }
            "insert" | "replace" => {
                self.eat(Tk::LParen, None);
                let (list_name, list_span) = self.eat_var_with_span();
                self.eat(Tk::Comma, None);
                let index = self.parse_expr();
                self.eat(Tk::Comma, None);
                let item = self.parse_expr();
                self.eat(Tk::RParen, None);
                let (op, fields) = if word == "insert" {
                    (ListOp::Insert, vec![(F::Item, item), (F::Index, index)])
                } else {
                    (ListOp::Replace, vec![(F::Index, index), (F::Item, item)])
                };
                return Stmt { kind: StmtKind::List { op, list_name, list_span, fields }, line, col };
            }
            "populateList" | "populateArray" => {
                self.eat(Tk::LParen, None);
                let (list_name, list_span) = self.eat_var_with_span();
                self.eat(Tk::Comma, None);
                let value_expr = self.parse_expr();
                self.eat(Tk::Comma, None);
                let count_expr = if self.check(Tk::Ident) && self.text(self.peek()) == "max" {
                    self.pos += 1;
                    Expr::Num { value: 200000.0, line, col }
                } else {
                    self.parse_expr()
                };
                self.eat(Tk::Comma, None);
                let clear_first = self.parse_expr();
                self.eat(Tk::RParen, None);
                return Stmt {
                    kind: StmtKind::List {
                        op: ListOp::Populate,
                        list_name,
                        list_span,
                        fields: vec![(F::ValueExpr, value_expr), (F::CountExpr, count_expr), (F::ClearFirst, clear_first)],
                    },
                    line,
                    col,
                };
            }
            _ => {}
        }

        let similar: Vec<String> =
            fuzzy_match(&v_text, data::CALL_SIGS.iter().map(|(k, _)| *k), 3).into_iter().map(|k| format!("`{}`", data::call_sig(&k).unwrap_or(&k))).collect();
        let hint = if !similar.is_empty() {
            format!("  Did you mean: {}?", similar.join("  or  "))
        } else {
            "  Commands are camelCase function calls like `move(10)`, `say(\"hi\")`, `forever { }`.".to_string()
        };
        let len = match v {
            Val::Str(s) if !s.is_empty() => js::utf16_len(s),
            _ => 1,
        };
        self.push_error(line, col, Some(len), format!("Unknown statement `{v_text}`.{hint}"));
        Stmt { kind: StmtKind::Unknown { value: self.jsval(t) }, line, col }
    }

    pub fn parse_expr(&mut self) -> Expr {
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> Expr {
        let t = self.peek();
        let cond = self.parse_or();
        if !self.check(Tk::Question) {
            return cond;
        }
        self.pos += 1;
        let then = self.parse_ternary();
        self.eat(Tk::Colon, Some("`cond ? a : b` ternary: expected `:` between the two result values"));
        let alt = self.parse_ternary();
        Expr::Ternary { cond: Box::new(cond), then: Box::new(then), alt: Box::new(alt), line: t.line, col: t.col }
    }

    fn parse_or(&mut self) -> Expr {
        let mut left = self.parse_and();
        while self.check_v("or") {
            self.pos += 1;
            let right = self.parse_and();
            left = Expr::Bin { op: BinOp::Or, left: Box::new(left), right: Box::new(right) };
        }
        left
    }

    fn parse_and(&mut self) -> Expr {
        let mut left = self.parse_not();
        while self.check_v("and") {
            self.pos += 1;
            let right = self.parse_not();
            left = Expr::Bin { op: BinOp::And, left: Box::new(left), right: Box::new(right) };
        }
        left
    }

    fn parse_not(&mut self) -> Expr {
        if self.check_v("not") {
            self.pos += 1;
            return Expr::Unary { op: UnOp::Not, operand: Box::new(self.parse_not()) };
        }
        self.parse_compare()
    }

    fn parse_compare(&mut self) -> Expr {
        let left = self.parse_add();
        let mut pairs: Vec<(Tk, Expr)> = Vec::new();
        while matches!(self.peek().kind, Tk::Lt | Tk::Gt | Tk::Eq | Tk::Neq | Tk::Le | Tk::Ge) {
            let op = self.peek().kind;
            self.pos += 1;
            pairs.push((op, self.parse_add()));
        }
        if pairs.is_empty() {
            return left;
        }
        fn cmp(op: Tk, l: Expr, r: Expr) -> Expr {
            let bin = |op, l, r| Expr::Bin { op, left: Box::new(l), right: Box::new(r) };
            let not = |e| Expr::Unary { op: UnOp::Not, operand: Box::new(e) };
            match op {
                Tk::Neq => not(bin(BinOp::Eq, l, r)),
                Tk::Le => not(bin(BinOp::Gt, l, r)),
                Tk::Ge => not(bin(BinOp::Lt, l, r)),
                Tk::Lt => bin(BinOp::Lt, l, r),
                Tk::Gt => bin(BinOp::Gt, l, r),
                _ => bin(BinOp::Eq, l, r),
            }
        }
        let mut iter = pairs.into_iter();
        let (op0, right0) = iter.next().unwrap();
        let mut prev = right0.clone();
        let mut result = cmp(op0, left, right0);
        for (op, right) in iter {
            let next_prev = right.clone();
            result = Expr::Bin { op: BinOp::And, left: Box::new(result), right: Box::new(cmp(op, prev, right)) };
            prev = next_prev;
        }
        result
    }

    fn parse_add(&mut self) -> Expr {
        let mut left = self.parse_mul();
        while self.check(Tk::Plus) || self.check(Tk::Minus) {
            let op = if self.check(Tk::Plus) { BinOp::Add } else { BinOp::Sub };
            self.pos += 1;
            let right = self.parse_mul();
            left = Expr::Bin { op, left: Box::new(left), right: Box::new(right) };
        }
        left
    }

    fn parse_mul(&mut self) -> Expr {
        let mut left = self.parse_unary();
        while self.check(Tk::Star) || self.check(Tk::Slash) || self.check_v("mod") {
            let op = match self.peek().kind {
                Tk::Star => BinOp::Mul,
                Tk::Slash => BinOp::Div,
                _ => BinOp::Mod,
            };
            self.pos += 1;
            let right = self.parse_unary();
            left = Expr::Bin { op, left: Box::new(left), right: Box::new(right) };
        }
        left
    }

    fn parse_unary(&mut self) -> Expr {
        if self.check(Tk::Minus) {
            self.pos += 1;
            return Expr::Unary { op: UnOp::Neg, operand: Box::new(self.parse_unary()) };
        }
        if self.check(Tk::Star) {
            let t = self.peek();
            self.pos += 1;
            return Expr::Deref { addr: Box::new(self.parse_unary()), line: t.line, col: t.col };
        }
        if self.check(Tk::Amp) {
            let t = self.peek();
            self.pos += 1;
            if !self.check(Tk::Var) {
                self.push_error(t.line, t.col, Some(1), "`&` takes the address of a variable: `&[x]`".to_string());
                return Expr::Num { value: 0.0, line: t.line, col: t.col };
            }
            let v = self.eat(Tk::Var, None);
            return Expr::Addr { var_name: self.text(v).into(), line: t.line, col: t.col, var_span: span_of_token(v) };
        }
        if self.check(Tk::Bang) {
            let t = self.peek();
            self.push_error(t.line, t.col, Some(1), "Use `not` for boolean negation — `!` is only valid as part of `!=`".to_string());
            self.pos += 1;
            return Expr::Unary { op: UnOp::Not, operand: Box::new(self.parse_unary()) };
        }
        self.parse_call_expr()
    }

    fn parse_call_expr(&mut self) -> Expr {
        let t = self.peek();
        let word = if t.kind == Tk::Num { "" } else { self.text(t) };
        let callable = t.kind == Tk::Ident || (t.kind != Tk::Str && t.kind != Tk::Num && data::is_keyword(word) && REPORTER_KEYWORDS.contains(&word));
        if callable {
            self.pos += 1;
            if self.check(Tk::LParen) {
                self.eat(Tk::LParen, None);
                let args = self.parse_args();
                self.eat(Tk::RParen, Some(&format!("`{word}(...)`: expected `)` to close the argument list")));
                return Expr::Call { name: word.into(), args, line: t.line, col: t.col };
            }
            if word == "true" || word == "false" {
                return Expr::Bool { value: word == "true", line: t.line, col: t.col };
            }
            return Expr::Reporter { name: word.into(), line: t.line, col: t.col };
        }
        self.parse_primary_expr()
    }

    fn parse_primary_expr(&mut self) -> Expr {
        let t = self.peek();
        match t.kind {
            Tk::Num => {
                self.pos += 1;
                return Expr::Num { value: t.num, line: t.line, col: t.col };
            }
            Tk::Str => {
                self.pos += 1;
                return Expr::Str { value: self.text(t).into(), line: t.line, col: t.col, end_line: t.end_line, end_col: t.end_col };
            }
            Tk::IStr => {
                self.pos += 1;
                return self.parse_interpolation(t);
            }
            Tk::Hex => {
                self.pos += 1;
                return Expr::Hex { value: self.text(t).into(), line: t.line, col: t.col };
            }
            Tk::Var => {
                self.pos += 1;
                let var_expr = self.var_expr(t);
                if self.check(Tk::Dot) {
                    self.pos += 1;
                    let m = self.peek();
                    if m.kind == Tk::Ident {
                        self.pos += 1;
                        let method = self.text(m);
                        let name = self.text(t);
                        self.eat(Tk::LParen, Some(&format!("`[{name}].{method}(...)`: expected `(`")));
                        let args = self.parse_args();
                        self.eat(Tk::RParen, Some(&format!("`[{name}].{method}(...)`: expected `)`")));
                        if method == "sort" {
                            self.push_error(
                                t.line,
                                t.col,
                                Some(js::utf16_len(name)),
                                format!("`[{name}].sort()` is a statement — write it on its own line, not inside an expression"),
                            );
                            return var_expr;
                        }
                        return Expr::Member { object: Box::new(var_expr), method: method.into(), args, line: t.line, col: t.col };
                    }
                }
                if self.check(Tk::Var) && self.peek().line == t.line {
                    let idx = self.peek();
                    self.pos += 1;
                    return Expr::Member { object: Box::new(var_expr), method: "item".into(), args: vec![self.var_expr(idx)], line: t.line, col: t.col };
                }
                return var_expr;
            }
            Tk::LParen => {
                self.pos += 1;
                let e = self.parse_expr();
                self.eat(Tk::RParen, None);
                return e;
            }
            _ => {}
        }
        let where_ = if self.call_ctx.is_empty() { String::new() } else { format!(" in {}", self.call_ctx) };
        let len = self.value_len(t).map(|l| l.max(1));
        let message = format!("Expected a value (number, string \"...\", [variable], or expression){where_} — got {}", self.tok(t));
        self.push_error(t.line, t.col, len, message);
        self.pos += 1;
        Expr::Num { value: 0.0, line: 0, col: 0 }
    }

    fn parse_interpolation(&mut self, t: &'a Token) -> Expr {
        let mut exprs = Vec::new();
        for part in t.parts.as_deref().unwrap_or(&[]) {
            match part {
                Part::Str(text) => {
                    if !text.is_empty() {
                        exprs.push(Expr::Str { value: text.clone(), line: t.line, col: t.col, end_line: 0, end_col: 0 });
                    }
                }
                Part::Expr { src, line, col } => {
                    if js::trim(src).is_empty() {
                        self.push_error(
                            *line,
                            *col,
                            Some(2),
                            "Empty `{}` in string — put an expression inside, or write `{{`/`}}` for literal braces".to_string(),
                        );
                        continue;
                    }
                    let mut frag = tokenize(src, false);
                    for ft in frag.iter_mut() {
                        if ft.line == 1 {
                            let end_col = if ft.end_col != 0 { ft.end_col } else { ft.col };
                            ft.col = col + ft.col - 1;
                            if ft.end_line == 1 {
                                ft.end_col = col + end_col - 1;
                            }
                        }
                        ft.line = *line;
                        if ft.end_line != 0 {
                            ft.end_line = *line;
                        }
                    }
                    let mut sub = Parser::new(src, &frag);
                    let expr = sub.parse_expr();
                    for e in sub.errors {
                        self.errors.push(ParseError { message: format!("In `{{…}}` inside string: {}  (use `{{{{` for a literal brace)", e.message), ..e });
                    }
                    exprs.push(expr);
                }
            }
        }
        if exprs.is_empty() {
            return Expr::Str { value: "".into(), line: t.line, col: t.col, end_line: 0, end_col: 0 };
        }
        let mut node = exprs.pop().unwrap();
        while let Some(prev) = exprs.pop() {
            node = Expr::Call { name: "join".into(), args: vec![prev, node], line: t.line, col: t.col };
        }
        node
    }
}

pub fn fuzzy_match<'s>(name: &str, candidates: impl Iterator<Item = &'s str>, max: usize) -> Vec<String> {
    let vl = js::to_lower(name);
    candidates
        .filter(|k| {
            let kl = js::to_lower(k);
            kl.starts_with(&vl) || vl.starts_with(js::utf16_prefix(&kl, 3)) || kl.contains(&vl) || vl.contains(js::utf16_prefix(&kl, 4))
        })
        .take(max)
        .map(str::to_string)
        .collect()
}

pub fn parse(src: &str, tokens: &[Token]) -> (Script, Vec<ParseError>) {
    let mut parser = Parser::new(src, tokens);
    let script = parser.parse_script();
    (script, parser.errors)
}
