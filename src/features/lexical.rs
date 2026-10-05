const MATHFUNCS: &[&str] = &[
    "abs",
    "sqrt",
    "floor",
    "ceiling",
    "ceil",
    "round",
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
    "join",
    "letterOf",
    "contains",
    "distanceTo",
    "xOf",
    "yOf",
    "directionOf",
    "costumeNumOf",
    "costumeNameOf",
    "sizeOf",
    "volumeOf",
];

fn is_separator(c: char) -> bool {
    c.is_whitespace() || "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?".contains(c)
}

pub struct Word {
    pub text: String,
    pub start: u32,
    pub end: u32,
}

fn word_matches(line: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let numeric = {
            let mut j = i;
            if chars.get(j) == Some(&'-') {
                j += 1;
            }
            while chars.get(j).is_some_and(|c| c.is_ascii_digit()) {
                j += 1;
            }
            if chars.get(j) == Some(&'.') && chars.get(j + 1).is_some_and(|c| c.is_ascii_digit()) {
                j += 2;
                while chars.get(j).is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_') {
                    j += 1;
                }
                Some(j)
            } else {
                None
            }
        };
        if let Some(end) = numeric {
            out.push((i, end));
            i = end;
            continue;
        }
        if !is_separator(chars[i]) {
            let mut j = i;
            while j < chars.len() && !is_separator(chars[j]) {
                j += 1;
            }
            out.push((i, j));
            i = j;
            continue;
        }
        i += 1;
    }
    let units: Vec<u32> = std::iter::once(0)
        .chain(chars.iter().scan(0u32, |acc, c| {
            *acc += c.len_utf16() as u32;
            Some(*acc)
        }))
        .collect();
    out.into_iter().map(|(s, e)| (units[s] as usize, units[e] as usize)).collect()
}

fn utf16_slice(line: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = line.encode_utf16().collect();
    String::from_utf16_lossy(&units[start.min(units.len())..end.min(units.len())])
}

pub fn word_at(line: &str, col: u32) -> Option<Word> {
    let pos = col as usize - 1;
    word_matches(line).into_iter().find(|(s, e)| *s <= pos && pos <= *e).map(|(s, e)| Word {
        text: utf16_slice(line, s, e),
        start: s as u32 + 1,
        end: e as u32 + 1,
    })
}

pub fn word_until(line: &str, col: u32) -> Word {
    match word_at(line, col) {
        Some(w) => {
            let text = utf16_slice(line, w.start as usize - 1, col as usize - 1);
            Word { text, start: w.start, end: col }
        }
        None => Word { text: String::new(), start: col, end: col },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lex {
    Keyword,
    String,
    Number,
    Comment,
    Operator,
    Variable,
    MathFunc,
    TypeIdentifier,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Root,
    AsmHeader,
    AsmBody,
    AsmArgs,
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

struct Line<'a> {
    chars: &'a [char],
}

impl Line<'_> {
    fn at(&self, i: usize) -> Option<char> {
        self.chars.get(i).copied()
    }

    fn word_end(&self, i: usize) -> Option<usize> {
        if !self.at(i).is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
            return None;
        }
        let mut j = i + 1;
        while self.at(j).is_some_and(is_word) {
            j += 1;
        }
        Some(j)
    }

    fn exact_word(&self, i: usize, word: &str) -> Option<usize> {
        let len = word.len();
        if self.chars.len() < i + len || !self.chars[i..i + len].iter().zip(word.bytes()).all(|(c, b)| *c == b as char) {
            return None;
        }
        let end = i + len;
        if self.at(end).is_some_and(is_word) {
            return None;
        }
        Some(end)
    }

    fn string(&self, i: usize) -> Option<usize> {
        if self.at(i) != Some('"') {
            return None;
        }
        (i + 1..self.chars.len()).find(|j| self.chars[*j] == '"').map(|j| j + 1)
    }

    fn variable(&self, i: usize) -> Option<usize> {
        if self.at(i) != Some('[') {
            return None;
        }
        let close = (i + 1..self.chars.len()).find(|j| self.chars[*j] == ']')?;
        (close > i + 1).then_some(close + 1)
    }

    fn number(&self, i: usize) -> Option<usize> {
        if !self.at(i).is_some_and(|c| c.is_ascii_digit()) {
            return None;
        }
        let mut j = i;
        while self.at(j).is_some_and(|c| c.is_ascii_digit()) {
            j += 1;
        }
        if self.at(j) == Some('.') && self.at(j + 1).is_some_and(|c| c.is_ascii_digit()) {
            j += 1;
            while self.at(j).is_some_and(|c| c.is_ascii_digit()) {
                j += 1;
            }
        }
        Some(j)
    }

    fn hex(&self, i: usize) -> Option<usize> {
        (self.at(i) == Some('#') && (1..=6).all(|k| self.at(i + k).is_some_and(|c| c.is_ascii_hexdigit()))).then_some(i + 7)
    }

    fn spaces(&self, i: usize) -> Option<usize> {
        let mut j = i;
        while self.at(j).is_some_and(char::is_whitespace) {
            j += 1;
        }
        (j > i).then_some(j)
    }
}

pub fn tokenize(text: &str) -> Vec<(u32, u32, u32, Lex)> {
    let mut out = Vec::with_capacity(text.len() / 6);
    let mut stack = vec![State::Root];
    let mut chars: Vec<char> = Vec::new();
    let mut word = String::new();
    let mut units: Vec<u32> = Vec::new();
    for (line_index, line_text) in text.split('\n').enumerate() {
        chars.clear();
        chars.extend(line_text.chars());
        let line = Line { chars: &chars };
        units.clear();
        let mut acc = 0u32;
        units.push(0);
        for c in &chars {
            acc += c.len_utf16() as u32;
            units.push(acc);
        }
        let mut emit = |start: usize, end: usize, kind: Lex| {
            if end > start {
                out.push((line_index as u32 + 1, units[start] + 1, units[end] - units[start], kind));
            }
        };
        let mut i = 0;
        while i < chars.len() {
            let state = *stack.last().unwrap_or(&State::Root);
            match state {
                State::Root => {
                    if line.at(i) == Some('/') && line.at(i + 1) == Some('/') {
                        emit(i, chars.len(), Lex::Comment);
                        i = chars.len();
                    } else if let Some(end) = line.exact_word(i, "__asm__") {
                        emit(i, end, Lex::Keyword);
                        stack.push(State::AsmHeader);
                        i = end;
                    } else if let Some(end) = line.string(i) {
                        emit(i, end, Lex::String);
                        i = end;
                    } else if let Some(end) = line.variable(i) {
                        emit(i, end, Lex::Variable);
                        i = end;
                    } else if let Some(end) = line.hex(i).or_else(|| line.number(i)) {
                        emit(i, end, Lex::Number);
                        i = end;
                    } else if let Some(end) = line.exact_word(i, "true").or_else(|| line.exact_word(i, "false")) {
                        emit(i, end, Lex::Keyword);
                        i = end;
                    } else if let Some(end) = line.word_end(i) {
                        word.clear();
                        word.extend(&chars[i..end]);
                        if crate::data::is_keyword(&word) {
                            emit(i, end, Lex::Keyword);
                        } else if MATHFUNCS.contains(&word.as_str()) {
                            emit(i, end, Lex::MathFunc);
                        }
                        i = end;
                    } else if "{}()[],:".contains(chars[i]) {
                        i += 1;
                    } else if ["!=", "<=", ">="].iter().any(|op| line.at(i) == op.chars().next() && line.at(i + 1) == op.chars().nth(1)) {
                        emit(i, i + 2, Lex::Operator);
                        i += 2;
                    } else if "+-*/<>=?&".contains(chars[i]) {
                        emit(i, i + 1, Lex::Operator);
                        i += 1;
                    } else {
                        i += 1;
                    }
                }
                State::AsmHeader => {
                    if let Some(end) = line.spaces(i) {
                        i = end;
                    } else if let Some(end) = line.exact_word(i, "volatile").or_else(|| line.exact_word(i, "unsafe")) {
                        emit(i, end, Lex::Keyword);
                        i = end;
                    } else if chars[i] == '(' {
                        stack.push(State::AsmBody);
                        i += 1;
                    } else {
                        stack.pop();
                    }
                }
                State::AsmBody => {
                    if line.at(i) == Some('/') && line.at(i + 1) == Some('/') {
                        emit(i, chars.len(), Lex::Comment);
                        i = chars.len();
                    } else if let Some(end) = line.spaces(i) {
                        i = end;
                    } else if chars[i] == ';' {
                        i += 1;
                    } else if chars[i] == ')' {
                        stack.pop();
                        i += 1;
                    } else if let Some(end) = line.word_end(i).filter(|e| {
                        let mut k = *e;
                        while line.at(k).is_some_and(char::is_whitespace) {
                            k += 1;
                        }
                        line.at(k) == Some('(')
                    }) {
                        emit(i, end, Lex::TypeIdentifier);
                        i = end;
                    } else if chars[i] == '(' {
                        stack.push(State::AsmArgs);
                        i += 1;
                    } else {
                        i += 1;
                    }
                }
                State::AsmArgs => {
                    if let Some(end) = line.spaces(i) {
                        i = end;
                    } else if chars[i] == ')' {
                        stack.pop();
                        i += 1;
                    } else if chars[i] == '(' {
                        stack.push(State::AsmArgs);
                        i += 1;
                    } else if let Some(end) = line.string(i) {
                        emit(i, end, Lex::String);
                        i = end;
                    } else if let Some(end) = line.number(i) {
                        emit(i, end, Lex::Number);
                        i = end;
                    } else if let Some(end) = line.exact_word(i, "true").or_else(|| line.exact_word(i, "false")) {
                        emit(i, end, Lex::Keyword);
                        i = end;
                    } else if chars[i] == ',' {
                        i += 1;
                    } else if let Some(end) = line.variable(i).or_else(|| line.word_end(i)) {
                        emit(i, end, Lex::Variable);
                        i = end;
                    } else {
                        i += 1;
                    }
                }
            }
        }
    }
    out
}
