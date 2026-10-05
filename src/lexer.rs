use crate::js;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Tk {
    Num,
    Str,
    IStr,
    Ident,
    Var,
    Kw,
    LBrace,
    RBrace,
    LParen,
    RParen,
    Comma,
    Colon,
    Hash,
    Hex,
    Dot,
    Lt,
    Gt,
    Eq,
    Plus,
    Minus,
    Star,
    Slash,
    Semi,
    Eof,
    Neq,
    Le,
    Ge,
    Bang,
    Question,
    Amp,
    Invalid,
    Comment,
}

impl Tk {
    pub fn js_name(self) -> &'static str {
        match self {
            Tk::Num => "NUM",
            Tk::Str => "STR",
            Tk::IStr => "ISTR",
            Tk::Ident => "IDENT",
            Tk::Var => "VAR",
            Tk::Kw => "KW",
            Tk::LBrace => "{",
            Tk::RBrace => "}",
            Tk::LParen => "(",
            Tk::RParen => ")",
            Tk::Comma => ",",
            Tk::Colon => ":",
            Tk::Hash => "#",
            Tk::Hex => "HEX",
            Tk::Dot => ".",
            Tk::Lt => "<",
            Tk::Gt => ">",
            Tk::Eq => "=",
            Tk::Plus => "+",
            Tk::Minus => "-",
            Tk::Star => "*",
            Tk::Slash => "/",
            Tk::Semi => ";",
            Tk::Eof => "EOF",
            Tk::Neq => "!=",
            Tk::Le => "<=",
            Tk::Ge => ">=",
            Tk::Bang => "!",
            Tk::Question => "?",
            Tk::Amp => "&",
            Tk::Invalid => "INVALID",
            Tk::Comment => "COMMENT",
        }
    }
}

#[derive(Clone, Debug)]
pub enum TokText {
    Src(u32, u32),
    Owned(Box<str>),
}

#[derive(Clone, Debug)]
pub enum Part {
    Str(Box<str>),
    Expr { src: Box<str>, line: u32, col: u32 },
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: Tk,
    pub unterminated: bool,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub num: f64,
    pub text: TokText,
    pub parts: Option<Box<[Part]>>,
}

impl Token {
    pub fn text<'a>(&'a self, src: &'a str) -> &'a str {
        match &self.text {
            TokText::Src(start, end) => &src[*start as usize..*end as usize],
            TokText::Owned(s) => s,
        }
    }

    pub fn has_end(&self) -> bool {
        self.end_line != 0
    }
}

struct Utf16String {
    out: String,
    pending_high: Option<u16>,
}

impl Utf16String {
    fn new() -> Self {
        Utf16String { out: String::new(), pending_high: None }
    }

    fn flush(&mut self) {
        if self.pending_high.take().is_some() {
            self.out.push('\u{FFFD}');
        }
    }

    fn push(&mut self, c: char) {
        self.flush();
        self.out.push(c);
    }

    fn push_unit(&mut self, unit: u16) {
        match unit {
            0xD800..=0xDBFF => {
                self.flush();
                self.pending_high = Some(unit);
            }
            0xDC00..=0xDFFF => match self.pending_high.take() {
                Some(high) => {
                    let code = 0x10000 + (((high as u32) - 0xD800) << 10) + ((unit as u32) - 0xDC00);
                    self.out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                }
                None => self.out.push('\u{FFFD}'),
            },
            _ => self.push(char::from_u32(unit as u32).unwrap_or('\u{FFFD}')),
        }
    }

    fn is_empty(&self) -> bool {
        self.out.is_empty() && self.pending_high.is_none()
    }

    fn take(&mut self) -> String {
        self.flush();
        std::mem::take(&mut self.out)
    }
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    i: usize,
    line: u32,
    col: u32,
}

impl<'a> Lexer<'a> {
    #[inline]
    fn at(&self, offset: usize) -> u8 {
        *self.bytes.get(self.i + offset).unwrap_or(&0)
    }

    #[inline]
    fn current_char(&self) -> char {
        let b = self.bytes[self.i];
        if b < 0x80 { b as char } else { self.src[self.i..].chars().next().unwrap_or('\u{FFFD}') }
    }

    #[inline]
    fn advance(&mut self) -> char {
        let b = self.bytes[self.i];
        if b < 0x80 {
            self.i += 1;
            if b == b'\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            return b as char;
        }
        let c = self.current_char();
        self.i += c.len_utf8();
        self.col += js::char_utf16(c);
        c
    }

    fn done(&self) -> bool {
        self.i >= self.bytes.len()
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

pub fn tokenize(src: &str, comments: bool) -> Vec<Token> {
    let mut lx = Lexer { src, bytes: src.as_bytes(), i: 0, line: 1, col: 1 };
    let mut tokens = Vec::with_capacity(src.len() / 4 + 8);

    let simple = |kind: Tk, start: usize, end: usize, line: u32, col: u32, end_line: u32, end_col: u32| Token {
        kind,
        unterminated: false,
        line,
        col,
        end_line,
        end_col,
        num: 0.0,
        text: TokText::Src(start as u32, end as u32),
        parts: None,
    };

    while !lx.done() {
        let b = lx.bytes[lx.i];
        if b == b' ' || b == b'\n' || b == b'\t' || b == b'\r' || (b >= 0x80 && js::is_space(lx.current_char())) || b == 0x0B || b == 0x0C {
            lx.advance();
            continue;
        }
        if b == b'/' && lx.at(1) == b'/' {
            let from = lx.i;
            let (c_line, c_col) = (lx.line, lx.col);
            let end = memchr_newline(&lx.bytes[from..]).map(|n| from + n).unwrap_or(lx.bytes.len());
            lx.i = end;
            if comments {
                lx.col += js::utf16_len(&src[from..end]);
                tokens.push(simple(Tk::Comment, from, end, c_line, c_col, lx.line, lx.col));
            }
            continue;
        }
        let (start_line, start_col) = (lx.line, lx.col);
        let start = lx.i;

        if b == b'"' {
            lx.advance();
            let mut s = Utf16String::new();
            let mut parts: Vec<Part> = Vec::new();
            while !lx.done() && lx.bytes[lx.i] != b'"' {
                let c = lx.bytes[lx.i];
                if c == b'\\' && lx.i + 1 < lx.bytes.len() {
                    lx.advance();
                    let escape = lx.advance();
                    if escape == 'u' {
                        let hex = lx.bytes.get(lx.i..lx.i + 4);
                        if let Some(hex) = hex.filter(|h| h.iter().all(|d| d.is_ascii_hexdigit())) {
                            let unit = u16::from_str_radix(std::str::from_utf8(hex).unwrap_or("0"), 16).unwrap_or(0);
                            s.push_unit(unit);
                            for _ in 0..4 {
                                lx.advance();
                            }
                            continue;
                        }
                    }
                    match escape {
                        'n' => s.push('\n'),
                        'r' => s.push('\r'),
                        't' => s.push('\t'),
                        'b' => s.push('\u{8}'),
                        'f' => s.push('\u{C}'),
                        '"' => s.push('"'),
                        '\\' => s.push('\\'),
                        '/' => s.push('/'),
                        other => {
                            s.push('\\');
                            s.push(other);
                        }
                    }
                    continue;
                }
                if c == b'{' && lx.at(1) == b'{' {
                    lx.advance();
                    lx.advance();
                    s.push('{');
                    continue;
                }
                if c == b'}' && lx.at(1) == b'}' {
                    lx.advance();
                    lx.advance();
                    s.push('}');
                    continue;
                }
                if c == b'{' {
                    lx.advance();
                    let (e_line, e_col) = (lx.line, lx.col);
                    let mut depth = 1;
                    let mut esrc = String::new();
                    while !lx.done() {
                        let d = lx.bytes[lx.i];
                        if d == b'"' {
                            esrc.push(lx.advance());
                            while !lx.done() && lx.bytes[lx.i] != b'"' {
                                let escaped = lx.bytes[lx.i] == b'\\';
                                esrc.push(lx.advance());
                                if escaped && !lx.done() {
                                    esrc.push(lx.advance());
                                }
                            }
                            if !lx.done() {
                                esrc.push(lx.advance());
                            }
                            continue;
                        }
                        if d == b'{' {
                            depth += 1;
                        } else if d == b'}' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        esrc.push(lx.advance());
                    }
                    if !lx.done() && lx.bytes[lx.i] == b'}' {
                        lx.advance();
                    }
                    parts.push(Part::Str(s.take().into()));
                    parts.push(Part::Expr { src: esrc.into(), line: e_line, col: e_col });
                    continue;
                }
                let ch = lx.advance();
                s.push(ch);
            }
            let unterminated = lx.done();
            if !unterminated {
                lx.advance();
            }
            if parts.is_empty() {
                let value = s.take();
                let raw_end = if unterminated { lx.i } else { lx.i - 1 };
                let text = if src[start + 1..raw_end] == *value { TokText::Src((start + 1) as u32, raw_end as u32) } else { TokText::Owned(value.into()) };
                tokens.push(Token {
                    kind: Tk::Str,
                    unterminated,
                    line: start_line,
                    col: start_col,
                    end_line: lx.line,
                    end_col: lx.col,
                    num: 0.0,
                    text,
                    parts: None,
                });
            } else {
                if !s.is_empty() {
                    parts.push(Part::Str(s.take().into()));
                }
                tokens.push(Token {
                    kind: Tk::IStr,
                    unterminated,
                    line: start_line,
                    col: start_col,
                    end_line: lx.line,
                    end_col: lx.col,
                    num: 0.0,
                    text: TokText::Src(0, 0),
                    parts: Some(parts.into_boxed_slice()),
                });
            }
            continue;
        }

        if b == b'[' {
            lx.advance();
            let from = lx.i;
            while !lx.done() && lx.bytes[lx.i] != b']' {
                lx.advance();
            }
            let raw = &src[from..lx.i];
            let unterminated = lx.done();
            if !unterminated {
                lx.advance();
            }
            let trimmed = js::trim(raw);
            let offset = from + (trimmed.as_ptr() as usize - raw.as_ptr() as usize);
            tokens.push(simple(Tk::Var, offset, offset + trimmed.len(), start_line, start_col, lx.line, lx.col));
            tokens.last_mut().unwrap().unterminated = unterminated;
            continue;
        }

        if b.is_ascii_digit() || (b == b'.' && lx.at(1).is_ascii_digit()) {
            let bytes = lx.bytes;
            let mut j = lx.i;
            if bytes[j] == b'.' {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
            } else {
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'.' {
                    j += 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                }
            }
            if j < bytes.len() && (bytes[j] == b'e' || bytes[j] == b'E') {
                let mut k = j + 1;
                if k < bytes.len() && (bytes[k] == b'+' || bytes[k] == b'-') {
                    k += 1;
                }
                if k < bytes.len() && bytes[k].is_ascii_digit() {
                    while k < bytes.len() && bytes[k].is_ascii_digit() {
                        k += 1;
                    }
                    j = k;
                }
            }
            let text = &src[lx.i..j];
            let value: f64 = text.parse().unwrap_or(f64::NAN);
            lx.col += (j - lx.i) as u32;
            lx.i = j;
            let mut token = simple(Tk::Num, start, j, start_line, start_col, lx.line, lx.col);
            token.num = value;
            tokens.push(token);
            continue;
        }

        if b.is_ascii_alphabetic() || b == b'_' {
            let mut j = lx.i;
            while j < lx.bytes.len() && is_word_byte(lx.bytes[j]) {
                j += 1;
            }
            lx.col += (j - lx.i) as u32;
            lx.i = j;
            let word = &src[start..j];
            let kind = if crate::data::is_keyword(word) { Tk::Kw } else { Tk::Ident };
            tokens.push(simple(kind, start, j, start_line, start_col, lx.line, lx.col));
            continue;
        }

        if b == b'#' && lx.bytes.len() >= lx.i + 7 && lx.bytes[lx.i + 1..lx.i + 7].iter().all(|d| d.is_ascii_hexdigit()) {
            lx.i += 7;
            lx.col += 7;
            tokens.push(simple(Tk::Hex, start, lx.i, start_line, start_col, lx.line, lx.col));
            continue;
        }

        let next = lx.at(1);
        let two = match (b, next) {
            (b'!', b'=') => Some(Tk::Neq),
            (b'<', b'=') => Some(Tk::Le),
            (b'>', b'=') => Some(Tk::Ge),
            _ => None,
        };
        if let Some(kind) = two {
            lx.i += 2;
            lx.col += 2;
            tokens.push(simple(kind, start, lx.i, start_line, start_col, lx.line, lx.col));
            continue;
        }
        let single = match b {
            b'!' => Some(Tk::Bang),
            b'?' => Some(Tk::Question),
            b'{' => Some(Tk::LBrace),
            b'}' => Some(Tk::RBrace),
            b'(' => Some(Tk::LParen),
            b')' => Some(Tk::RParen),
            b',' => Some(Tk::Comma),
            b':' => Some(Tk::Colon),
            b'<' => Some(Tk::Lt),
            b'>' => Some(Tk::Gt),
            b'=' => Some(Tk::Eq),
            b'+' => Some(Tk::Plus),
            b'-' => Some(Tk::Minus),
            b'*' => Some(Tk::Star),
            b'/' => Some(Tk::Slash),
            b'#' => Some(Tk::Hash),
            b'.' => Some(Tk::Dot),
            b';' => Some(Tk::Semi),
            b'&' => Some(Tk::Amp),
            _ => None,
        };
        if let Some(kind) = single {
            lx.i += 1;
            lx.col += 1;
            tokens.push(simple(kind, start, lx.i, start_line, start_col, lx.line, lx.col));
            continue;
        }

        let c = lx.current_char();
        if c.len_utf16() == 2 {
            for _ in 0..2 {
                tokens.push(Token {
                    kind: Tk::Invalid,
                    unterminated: false,
                    line: lx.line,
                    col: lx.col,
                    end_line: lx.line,
                    end_col: lx.col + 1,
                    num: 0.0,
                    text: TokText::Owned("\u{FFFD}".into()),
                    parts: None,
                });
                lx.col += 1;
            }
            lx.i += c.len_utf8();
        } else {
            tokens.push(simple(Tk::Invalid, lx.i, lx.i + c.len_utf8(), lx.line, lx.col, lx.line, lx.col + 1));
            lx.advance();
        }
    }

    tokens.push(Token {
        kind: Tk::Eof,
        unterminated: false,
        line: lx.line,
        col: lx.col,
        end_line: 0,
        end_col: 0,
        num: 0.0,
        text: TokText::Src(0, 0),
        parts: None,
    });
    tokens
}

fn memchr_newline(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|&b| b == b'\n')
}
