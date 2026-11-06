use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Symbol(String),
    Str(String),
    Int(i64),
    Float(f64),
    Unit(f64, String),
    Regex(String, String),
    Punct(&'static str),
    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Tok::Ident(s) => write!(f, "{}", s),
            Tok::Symbol(s) => write!(f, "#{}", s),
            Tok::Str(_) => write!(f, "string"),
            Tok::Int(v) => write!(f, "{}", v),
            Tok::Float(v) => write!(f, "{}", v),
            Tok::Unit(v, u) => write!(f, "{}{}", v, u),
            Tok::Regex(..) => write!(f, "regex"),
            Tok::Punct(p) => write!(f, "{}", p),
            Tok::Eof => write!(f, "end of file"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pos {
    pub offset: usize,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone)]
pub struct ParseError {
    pub pos: Pos,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}: {}", self.pos.line, self.pos.col, self.message)
    }
}

pub type LexResult<T> = std::result::Result<T, ParseError>;

const UNITS: &[&str] = &[
    "ms", "s", "dip", "px", "em", "pt", "pr", "sp", "vw", "vh", "ppx", "in", "cm", "mm",
];

pub struct Lexer {
    src: Vec<char>,
    pub pos: usize,
    line: u32,
    line_start: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Lexer {
        Lexer {
            src: source.chars().collect(),
            pos: 0,
            line: 1,
            line_start: 0,
        }
    }

    pub fn here(&self) -> Pos {
        Pos {
            offset: self.pos,
            line: self.line,
            col: (self.pos - self.line_start) as u32 + 1,
        }
    }

    pub fn save(&self) -> (usize, u32, usize) {
        (self.pos, self.line, self.line_start)
    }

    pub fn restore(&mut self, state: (usize, u32, usize)) {
        self.pos = state.0;
        self.line = state.1;
        self.line_start = state.2;
    }

    fn err<T>(&self, message: impl Into<String>) -> LexResult<T> {
        Err(ParseError {
            pos: self.here(),
            message: message.into(),
        })
    }

    fn peek_char(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.src.get(self.pos + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.src.get(self.pos).copied();
        if let Some(c) = c {
            self.pos += 1;
            if c == '\n' {
                self.line += 1;
                self.line_start = self.pos;
            }
        }
        c
    }

    pub fn skip_ws_and_comments(&mut self) -> LexResult<()> {
        loop {
            match self.peek_char() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.peek_at(1) == Some('/') => {
                    while let Some(c) = self.peek_char() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek_at(1) == Some('*') => {
                    self.bump();
                    self.bump();
                    loop {
                        match self.peek_char() {
                            None => return self.err("unterminated block comment"),
                            Some('*') if self.peek_at(1) == Some('/') => {
                                self.bump();
                                self.bump();
                                break;
                            }
                            _ => {
                                self.bump();
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn is_ident_start(c: char) -> bool {
        c.is_alphabetic() || c == '_' || c == '$'
    }

    fn is_ident_part(c: char) -> bool {
        c.is_alphanumeric() || c == '_' || c == '$'
    }

    fn read_ident(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek_char() {
            if Self::is_ident_part(c) {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        s
    }

    fn read_symbol(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek_char() {
            if Self::is_ident_part(c) {
                s.push(c);
                self.bump();
            } else if c == '-' && self.peek_at(1).map_or(false, Self::is_ident_part) {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        s
    }

    fn read_string(&mut self, quote: char) -> LexResult<String> {
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return self.err("unterminated string literal"),
                Some(c) if c == quote => return Ok(s),
                Some('\\') => match self.bump() {
                    None => return self.err("unterminated string escape"),
                    Some('n') => s.push('\n'),
                    Some('r') => s.push('\r'),
                    Some('t') => s.push('\t'),
                    Some('b') => s.push('\u{8}'),
                    Some('f') => s.push('\u{c}'),
                    Some('0') => s.push('\0'),
                    Some('\n') => {}
                    Some('\r') => {
                        if self.peek_char() == Some('\n') {
                            self.bump();
                        }
                    }
                    Some('u') => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            match self.bump().and_then(|c| c.to_digit(16)) {
                                Some(d) => code = code * 16 + d,
                                None => return self.err("invalid \\u escape"),
                            }
                        }
                        s.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    Some('x') => {
                        let mut code = 0u32;
                        for _ in 0..2 {
                            match self.bump().and_then(|c| c.to_digit(16)) {
                                Some(d) => code = code * 16 + d,
                                None => return self.err("invalid \\x escape"),
                            }
                        }
                        s.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    Some(other) => s.push(other),
                },
                Some(c) => s.push(c),
            }
        }
    }

    fn read_number(&mut self) -> LexResult<Tok> {
        let start = self.pos;
        if self.peek_char() == Some('0')
            && matches!(self.peek_at(1), Some('x') | Some('X'))
        {
            self.bump();
            self.bump();
            let mut value: i64 = 0;
            let mut any = false;
            while let Some(d) = self.peek_char().and_then(|c| c.to_digit(16)) {
                value = value.wrapping_mul(16).wrapping_add(d as i64);
                any = true;
                self.bump();
            }
            if !any {
                return self.err("invalid hex literal");
            }
            return Ok(Tok::Int(value));
        }
        while self.peek_char().map_or(false, |c| c.is_ascii_digit()) {
            self.bump();
        }
        let mut is_float = false;
        if self.peek_char() == Some('.') && self.peek_at(1).map_or(false, |c| c.is_ascii_digit()) {
            is_float = true;
            self.bump();
            while self.peek_char().map_or(false, |c| c.is_ascii_digit()) {
                self.bump();
            }
        } else if self.peek_char() == Some('.')
            && !self.peek_at(1).map_or(false, |c| Self::is_ident_start(c) || c == '.')
        {
            is_float = true;
            self.bump();
        }
        if matches!(self.peek_char(), Some('e') | Some('E')) {
            let save = self.save();
            self.bump();
            if matches!(self.peek_char(), Some('+') | Some('-')) {
                self.bump();
            }
            if self.peek_char().map_or(false, |c| c.is_ascii_digit()) {
                is_float = true;
                while self.peek_char().map_or(false, |c| c.is_ascii_digit()) {
                    self.bump();
                }
            } else {
                self.restore(save);
            }
        }
        let text: String = self.src[start..self.pos].iter().collect();
        for unit in UNITS {
            if self.matches_ident_here(unit) {
                for _ in 0..unit.len() {
                    self.bump();
                }
                let v: f64 = text.parse().map_err(|_| ParseError {
                    pos: self.here(),
                    message: format!("bad number {}", text),
                })?;
                return Ok(Tok::Unit(v, unit.to_string()));
            }
        }
        if is_float {
            Ok(Tok::Float(text.parse().map_err(|_| ParseError {
                pos: self.here(),
                message: format!("bad number {}", text),
            })?))
        } else {
            Ok(Tok::Int(text.parse().map_err(|_| ParseError {
                pos: self.here(),
                message: format!("bad number {}", text),
            })?))
        }
    }

    fn matches_ident_here(&self, word: &str) -> bool {
        let mut i = 0;
        for c in word.chars() {
            if self.peek_at(i) != Some(c) {
                return false;
            }
            i += 1;
        }
        !self.peek_at(i).map_or(false, Self::is_ident_part)
    }

    fn read_regex(&mut self) -> LexResult<Tok> {
        let mut body = String::new();
        let mut in_class = false;
        loop {
            match self.bump() {
                None => return self.err("unterminated regex literal"),
                Some('\\') => {
                    body.push('\\');
                    match self.bump() {
                        None => return self.err("unterminated regex escape"),
                        Some(c) => body.push(c),
                    }
                }
                Some('[') => {
                    in_class = true;
                    body.push('[');
                }
                Some(']') => {
                    in_class = false;
                    body.push(']');
                }
                Some('/') if !in_class => break,
                Some('\n') => return self.err("newline in regex literal"),
                Some(c) => body.push(c),
            }
        }
        let mut flags = String::new();
        while let Some(c) = self.peek_char() {
            if c.is_ascii_alphabetic() {
                flags.push(c);
                self.bump();
            } else {
                break;
            }
        }
        Ok(Tok::Regex(body, flags))
    }

    pub fn next_token(&mut self, regex_ok: bool) -> LexResult<(Pos, Tok)> {
        self.skip_ws_and_comments()?;
        let pos = self.here();
        let c = match self.peek_char() {
            None => return Ok((pos, Tok::Eof)),
            Some(c) => c,
        };
        let tok = match c {
            '"' | '\'' => {
                self.bump();
                Tok::Str(self.read_string(c)?)
            }
            '#' if self.peek_at(1).map_or(false, Self::is_ident_part) => {
                self.bump();
                Tok::Symbol(self.read_symbol())
            }
            '0'..='9' => self.read_number()?,
            '.' if self.peek_at(1).map_or(false, |c| c.is_ascii_digit()) => self.read_number()?,
            c if Self::is_ident_start(c) => Tok::Ident(self.read_ident()),
            '/' if regex_ok => {
                self.bump();
                self.read_regex()?
            }
            _ => {
                let three: String = (0..3).filter_map(|i| self.peek_at(i)).collect();
                let puncts3 = ["===", "!==", ">>>", "<<=", ">>="];
                let puncts2 = [
                    "==", "!=", "<=", ">=", "&&", "||", "??", "++", "--", "+=", "-=", "*=", "/=",
                    "%=", "&=", "|=", "^=", "<<", ">>", "=>", "..", "::",
                ];
                let mut matched: Option<&'static str> = None;
                for p in puncts3 {
                    if three.starts_with(p) {
                        matched = Some(p);
                        break;
                    }
                }
                if matched.is_none() {
                    let two: String = (0..2).filter_map(|i| self.peek_at(i)).collect();
                    for p in puncts2 {
                        if two == *p {
                            matched = Some(p);
                            break;
                        }
                    }
                }
                if matched.is_none() {
                    let singles = "+-*/%=<>!&|^~?:;,.(){}[]@";
                    if let Some(idx) = singles.find(c) {
                        const TABLE: &[&str] = &[
                            "+", "-", "*", "/", "%", "=", "<", ">", "!", "&", "|", "^", "~", "?",
                            ":", ";", ",", ".", "(", ")", "{", "}", "[", "]", "@",
                        ];
                        matched = Some(TABLE[idx]);
                    }
                }
                match matched {
                    Some(p) => {
                        for _ in 0..p.len() {
                            self.bump();
                        }
                        Tok::Punct(p)
                    }
                    None => return self.err(format!("unexpected character '{}'", c)),
                }
            }
        };
        Ok((pos, tok))
    }

    pub fn scan_stringizer_chunk(&mut self, depth: &mut usize) -> LexResult<(String, StringizerEnd)> {
        let mut text = String::new();
        loop {
            match self.peek_char() {
                None => return self.err("unterminated stringizer argument"),
                Some(')') if *depth == 0 => {
                    self.bump();
                    return Ok((text, StringizerEnd::Close));
                }
                Some('(') => {
                    *depth += 1;
                    text.push('(');
                    self.bump();
                }
                Some(')') => {
                    *depth -= 1;
                    text.push(')');
                    self.bump();
                }
                Some('{') => {
                    self.bump();
                    return Ok((text, StringizerEnd::Hole));
                }
                Some('"') | Some('\'') => {
                    let quote = self.bump().unwrap();
                    text.push(quote);
                    loop {
                        match self.bump() {
                            None => return self.err("unterminated string in stringizer"),
                            Some('\\') => {
                                text.push('\\');
                                if let Some(c) = self.bump() {
                                    text.push(c);
                                }
                            }
                            Some(c) => {
                                text.push(c);
                                if c == quote {
                                    break;
                                }
                            }
                        }
                    }
                }
                Some(c) => {
                    text.push(c);
                    self.bump();
                }
            }
        }
    }

    pub fn scan_jsx_text(&mut self) -> LexResult<String> {
        let mut text = String::new();
        loop {
            match self.peek_char() {
                None => return self.err("unterminated element content"),
                Some('<') | Some('{') => break,
                Some(c) => {
                    text.push(c);
                    self.bump();
                }
            }
        }
        Ok(text)
    }

    pub fn peek_is(&mut self, ch: char) -> bool {
        let save = self.save();
        let _ = self.skip_ws_and_comments();
        let r = self.peek_char() == Some(ch);
        self.restore(save);
        r
    }

    pub fn peek_char_raw(&self) -> Option<char> {
        self.peek_char()
    }

    pub fn bump_raw(&mut self) -> Option<char> {
        self.bump()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StringizerEnd {
    Close,
    Hole,
}
