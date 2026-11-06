use super::ast::*;
use super::lexer::{Lexer, LexResult, ParseError, Pos, StringizerEnd, Tok};

pub struct Parser {
    lx: Lexer,
    tok: Tok,
    tok_pos: Pos,
}

type PResult<T> = LexResult<T>;

fn regex_ok_after(tok: &Tok) -> bool {
    match tok {
        Tok::Ident(name) => matches!(
            name.as_str(),
            "return" | "typeof" | "instanceof" | "in" | "new" | "delete" | "throw" | "case"
                | "do" | "else" | "void"
        ),
        Tok::Int(_) | Tok::Float(_) | Tok::Unit(..) | Tok::Str(_) | Tok::Symbol(_)
        | Tok::Regex(..) => false,
        Tok::Punct(p) => !matches!(*p, ")" | "]" | "}" | "++" | "--"),
        Tok::Eof => true,
    }
}

impl Parser {
    pub fn new(source: &str) -> PResult<Parser> {
        let mut lx = Lexer::new(source);
        let (tok_pos, tok) = lx.next_token(true)?;
        Ok(Parser { lx, tok, tok_pos })
    }

    pub fn parse_program(mut self) -> PResult<Vec<Stmt>> {
        let mut stmts = Vec::new();
        while !matches!(self.tok, Tok::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        Ok(stmts)
    }

    fn err<T>(&self, message: impl Into<String>) -> PResult<T> {
        Err(ParseError {
            pos: self.tok_pos,
            message: format!("{} (at '{}')", message.into(), self.tok),
        })
    }

    fn advance(&mut self) -> PResult<()> {
        let regex_ok = regex_ok_after(&self.tok);
        let (pos, tok) = self.lx.next_token(regex_ok)?;
        self.tok = tok;
        self.tok_pos = pos;
        Ok(())
    }

    fn save(&self) -> ((usize, u32, usize), Tok, Pos) {
        (self.lx.save(), self.tok.clone(), self.tok_pos)
    }

    fn restore(&mut self, s: ((usize, u32, usize), Tok, Pos)) {
        self.lx.restore(s.0);
        self.tok = s.1;
        self.tok_pos = s.2;
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(&self.tok, Tok::Punct(q) if *q == p)
    }

    fn is_ident(&self, name: &str) -> bool {
        matches!(&self.tok, Tok::Ident(n) if n == name)
    }

    fn eat_punct(&mut self, p: &str) -> PResult<bool> {
        if self.is_punct(p) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect_punct(&mut self, p: &str) -> PResult<()> {
        if self.is_punct(p) {
            self.advance()
        } else {
            self.err(format!("expected '{}'", p))
        }
    }

    fn expect_ident(&mut self) -> PResult<String> {
        match &self.tok {
            Tok::Ident(n) => {
                let n = n.clone();
                self.advance()?;
                Ok(n)
            }
            _ => self.err("expected identifier"),
        }
    }

    fn eat_semi(&mut self) -> PResult<()> {
        while self.is_punct(";") {
            self.advance()?;
        }
        Ok(())
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        match &self.tok {
            Tok::Punct(";") => {
                self.advance()?;
                Ok(Stmt::Empty)
            }
            Tok::Punct("{") => {
                self.advance()?;
                let body = self.parse_block_rest()?;
                Ok(Stmt::Block(body))
            }
            Tok::Ident(name) => match name.as_str() {
                "include" => {
                    self.advance()?;
                    let path = match &self.tok {
                        Tok::Str(s) => s.clone(),
                        _ => return self.err("expected string after include"),
                    };
                    self.advance()?;
                    self.eat_semi()?;
                    Ok(Stmt::Include(path))
                }
                "var" => {
                    self.advance()?;
                    let stmt = self.parse_var_rest(true)?;
                    self.eat_semi()?;
                    Ok(stmt)
                }
                "const" => {
                    self.advance()?;
                    let stmt = self.parse_var_rest(true)?;
                    self.eat_semi()?;
                    match stmt {
                        Stmt::VarDecl(d) => Ok(Stmt::ConstDecl(d)),
                        other => Ok(other),
                    }
                }
                "function" => {
                    let save = self.save();
                    self.advance()?;
                    if matches!(self.tok, Tok::Ident(_)) {
                        let f = self.parse_function_rest()?;
                        Ok(Stmt::Function(f))
                    } else {
                        self.restore(save);
                        let e = self.parse_expression(true)?;
                        self.eat_semi()?;
                        Ok(Stmt::Expr(e))
                    }
                }
                "class" => {
                    self.advance()?;
                    let c = self.parse_class_rest()?;
                    Ok(Stmt::Class(c))
                }
                "event" => {
                    self.advance()?;
                    let e = self.parse_event_rest()?;
                    Ok(Stmt::Event(e))
                }
                "if" => {
                    self.advance()?;
                    self.expect_punct("(")?;
                    let cond = self.parse_condition()?;
                    self.expect_punct(")")?;
                    let then = Box::new(self.parse_stmt()?);
                    let mut alt = None;
                    if self.is_ident("else") {
                        self.advance()?;
                        alt = Some(Box::new(self.parse_stmt()?));
                    }
                    Ok(Stmt::If(cond, then, alt))
                }
                "for" => {
                    self.advance()?;
                    self.parse_for_rest()
                }
                "while" => {
                    self.advance()?;
                    self.expect_punct("(")?;
                    let cond = self.parse_condition()?;
                    self.expect_punct(")")?;
                    let body = Box::new(self.parse_stmt()?);
                    Ok(Stmt::While(cond, body))
                }
                "do" => {
                    self.advance()?;
                    let body = Box::new(self.parse_stmt()?);
                    if !self.is_ident("while") {
                        return self.err("expected 'while' after do body");
                    }
                    self.advance()?;
                    self.expect_punct("(")?;
                    let cond = self.parse_expression(true)?;
                    self.expect_punct(")")?;
                    self.eat_semi()?;
                    Ok(Stmt::DoWhile(body, cond))
                }
                "switch" => {
                    self.advance()?;
                    self.expect_punct("(")?;
                    let disc = self.parse_expression(true)?;
                    self.expect_punct(")")?;
                    self.expect_punct("{")?;
                    let mut cases = Vec::new();
                    while !self.is_punct("}") {
                        let test = if self.is_ident("case") {
                            self.advance()?;
                            let t = self.parse_expression(true)?;
                            Some(t)
                        } else if self.is_ident("default") {
                            self.advance()?;
                            None
                        } else {
                            return self.err("expected case or default");
                        };
                        self.expect_punct(":")?;
                        let mut body = Vec::new();
                        while !self.is_punct("}")
                            && !self.is_ident("case")
                            && !self.is_ident("default")
                        {
                            body.push(self.parse_stmt()?);
                        }
                        cases.push(SwitchCase { test, body });
                    }
                    self.expect_punct("}")?;
                    Ok(Stmt::Switch(disc, cases))
                }
                "try" => {
                    self.advance()?;
                    self.expect_punct("{")?;
                    let body = self.parse_block_rest()?;
                    let mut catch = None;
                    let mut finally = None;
                    if self.is_ident("catch") {
                        self.advance()?;
                        let mut var = String::new();
                        if self.eat_punct("(")? {
                            var = self.expect_ident()?;
                            self.expect_punct(")")?;
                        }
                        self.expect_punct("{")?;
                        let cbody = self.parse_block_rest()?;
                        catch = Some((var, cbody));
                    }
                    if self.is_ident("finally") {
                        self.advance()?;
                        self.expect_punct("{")?;
                        finally = Some(self.parse_block_rest()?);
                    }
                    Ok(Stmt::Try(body, catch, finally))
                }
                "throw" => {
                    self.advance()?;
                    let e = self.parse_expression(true)?;
                    self.eat_semi()?;
                    Ok(Stmt::Throw(e))
                }
                "assert" => {
                    self.advance()?;
                    let e = self.parse_assign(true)?;
                    let mut msg = None;
                    if self.eat_punct(":")? {
                        msg = Some(self.parse_assign(true)?);
                    }
                    self.eat_semi()?;
                    Ok(Stmt::Assert(e, msg))
                }
                "return" => {
                    self.advance()?;
                    if self.is_punct(";") || self.is_punct("}") {
                        self.eat_semi()?;
                        Ok(Stmt::Return(None))
                    } else {
                        let e = self.parse_expression(true)?;
                        self.eat_semi()?;
                        Ok(Stmt::Return(Some(e)))
                    }
                }
                "break" => {
                    self.advance()?;
                    self.eat_semi()?;
                    Ok(Stmt::Break)
                }
                "continue" => {
                    self.advance()?;
                    self.eat_semi()?;
                    Ok(Stmt::Continue)
                }
                _ => {
                    let e = self.parse_expression(true)?;
                    self.eat_semi()?;
                    Ok(Stmt::Expr(e))
                }
            },
            _ => {
                let e = self.parse_expression(true)?;
                self.eat_semi()?;
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn parse_condition(&mut self) -> PResult<Expr> {
        if self.is_ident("var") {
            self.advance()?;
            let name = self.expect_ident()?;
            self.expect_punct("=")?;
            let init = self.parse_expression(true)?;
            return Ok(Expr::Let(name, Box::new(init)));
        }
        self.parse_expression(true)
    }

    fn parse_block_rest(&mut self) -> PResult<Vec<Stmt>> {
        let mut body = Vec::new();
        while !self.is_punct("}") {
            if matches!(self.tok, Tok::Eof) {
                return self.err("unterminated block");
            }
            body.push(self.parse_stmt()?);
        }
        self.advance()?;
        Ok(body)
    }

    fn parse_var_rest(&mut self, allow_in_init: bool) -> PResult<Stmt> {
        if self.is_punct("(") {
            self.advance()?;
            let mut names = Vec::new();
            loop {
                names.push(self.expect_ident()?);
                if !self.eat_punct(",")? {
                    break;
                }
            }
            self.expect_punct(")")?;
            self.expect_punct("=")?;
            let init = self.parse_assign(allow_in_init)?;
            return Ok(Stmt::VarDestructure(names, init));
        }
        if self.is_punct("{") {
            self.advance()?;
            let mut names = Vec::new();
            loop {
                names.push(self.expect_ident()?);
                if !self.eat_punct(",")? {
                    break;
                }
            }
            self.expect_punct("}")?;
            self.expect_punct("=")?;
            let init = self.parse_assign(allow_in_init)?;
            return Ok(Stmt::VarObjectDestructure(names, init));
        }
        let mut decls = Vec::new();
        loop {
            let name = self.expect_ident()?;
            let mut init = None;
            if self.eat_punct("=")? {
                init = Some(self.parse_assign(allow_in_init)?);
            }
            decls.push((name, init));
            if !self.eat_punct(",")? {
                break;
            }
        }
        Ok(Stmt::VarDecl(decls))
    }

    fn parse_function_rest(&mut self) -> PResult<FunctionDecl> {
        let mut path = vec![self.expect_ident()?];
        while self.is_punct(".") {
            self.advance()?;
            path.push(self.expect_ident()?);
        }
        let params = self.parse_params()?;
        self.expect_punct("{")?;
        let body = self.parse_block_rest()?;
        Ok(FunctionDecl { path, params, body })
    }

    fn parse_params(&mut self) -> PResult<Vec<Param>> {
        self.expect_punct("(")?;
        let mut params = Vec::new();
        if self.eat_punct(")")? {
            return Ok(params);
        }
        loop {
            let name = self.expect_ident()?;
            let mut rest = false;
            let mut default = None;
            if self.eat_punct("..")? {
                rest = true;
            } else if self.eat_punct("=")? {
                default = Some(self.parse_assign(true)?);
            }
            params.push(Param {
                name,
                default,
                rest,
            });
            if !self.eat_punct(",")? {
                break;
            }
        }
        self.expect_punct(")")?;
        Ok(params)
    }

    fn parse_class_rest(&mut self) -> PResult<ClassDecl> {
        let name = self.expect_ident()?;
        let mut base = None;
        if self.eat_punct(":")? {
            let mut path = vec![self.expect_ident()?];
            while self.is_punct(".") {
                self.advance()?;
                path.push(self.expect_ident()?);
            }
            base = Some(path);
        } else if self.is_ident("extends") {
            self.advance()?;
            let mut path = vec![self.expect_ident()?];
            while self.is_punct(".") {
                self.advance()?;
                path.push(self.expect_ident()?);
            }
            base = Some(path);
        }
        self.expect_punct("{")?;
        let mut members = Vec::new();
        while !self.is_punct("}") {
            if matches!(self.tok, Tok::Eof) {
                return self.err("unterminated class body");
            }
            if self.is_punct(";") {
                self.advance()?;
                continue;
            }
            if self.is_ident("function") {
                self.advance()?;
                members.push(ClassMember::Method(self.parse_function_rest()?));
            } else if self.is_ident("var") {
                self.advance()?;
                match self.parse_var_rest(true)? {
                    Stmt::VarDecl(d) => members.push(ClassMember::Var(d)),
                    _ => return self.err("destructuring not allowed in class body"),
                }
                self.eat_semi()?;
            } else if self.is_ident("const") {
                self.advance()?;
                match self.parse_var_rest(true)? {
                    Stmt::VarDecl(d) => members.push(ClassMember::Const(d)),
                    _ => return self.err("destructuring not allowed in class body"),
                }
                self.eat_semi()?;
            } else if self.is_ident("event") {
                self.advance()?;
                members.push(ClassMember::Event(self.parse_event_rest()?));
            } else if self.is_ident("class") {
                self.advance()?;
                members.push(ClassMember::Class(self.parse_class_rest()?));
            } else if let Tok::Ident(name) = self.tok.clone() {
                self.advance()?;
                if name == "this" && self.is_ident("var") {
                    self.advance()?;
                    match self.parse_var_rest(true)? {
                        Stmt::VarDecl(d) => members.push(ClassMember::Var(d)),
                        _ => return self.err("bad this var declaration"),
                    }
                    self.eat_semi()?;
                } else if self.is_punct("(") {
                    let params = self.parse_params()?;
                    self.expect_punct("{")?;
                    let body = self.parse_block_rest()?;
                    members.push(ClassMember::Method(FunctionDecl {
                        path: vec![name],
                        params,
                        body,
                    }));
                } else {
                    return self.err("unexpected token in class body");
                }
            } else {
                return self.err("unexpected token in class body");
            }
        }
        self.advance()?;
        Ok(ClassDecl {
            name,
            base,
            members,
        })
    }

    fn parse_event_rest(&mut self) -> PResult<EventDecl> {
        let mut name = match &self.tok {
            Tok::Ident(n) => {
                let n = n.clone();
                self.advance()?;
                n
            }
            Tok::Punct("*") => {
                self.advance()?;
                "*".to_string()
            }
            _ => return self.err("expected event name"),
        };
        loop {
            if self.is_ident("bubbling") || self.is_ident("sinking") || self.is_ident("handled") {
                let modif = self.expect_ident()?;
                name.push(' ');
                name.push_str(&modif);
            } else if self.is_punct("~") {
                self.advance()?;
                name.push('~');
            } else {
                break;
            }
        }
        let mut selector = None;
        if let Tok::Ident(n) = &self.tok {
            if n == "$" && self.lx.peek_char_raw() == Some('(') {
                self.lx.bump_raw();
                selector = Some(self.parse_stringizer_parts()?);
                self.advance()?;
            }
        }
        let mut params = Vec::new();
        if self.is_punct("(") {
            self.advance()?;
            while !self.is_punct(")") {
                params.push(self.expect_ident()?);
                if !self.eat_punct(",")? {
                    break;
                }
            }
            self.expect_punct(")")?;
        }
        self.expect_punct("{")?;
        let body = self.parse_block_rest()?;
        Ok(EventDecl {
            name,
            selector,
            params,
            body,
        })
    }

    fn parse_stringizer_parts(&mut self) -> PResult<Vec<TextPart>> {
        let mut parts = Vec::new();
        let mut depth = 0usize;
        loop {
            let (text, end) = self.lx.scan_stringizer_chunk(&mut depth)?;
            if !text.is_empty() {
                parts.push(TextPart::Text(text));
            }
            match end {
                StringizerEnd::Close => break,
                StringizerEnd::Hole => {
                    self.advance()?;
                    let e = self.parse_expression(true)?;
                    if !self.is_punct("}") {
                        return self.err("expected '}' after interpolated expression");
                    }
                    parts.push(TextPart::Expr(e));
                }
            }
        }
        Ok(parts)
    }

    pub fn parse_expression(&mut self, allow_in: bool) -> PResult<Expr> {
        let mut e = self.parse_assign(allow_in)?;
        while self.is_punct(",") {
            self.advance()?;
            let rhs = self.parse_assign(allow_in)?;
            e = Expr::Comma(Box::new(e), Box::new(rhs));
        }
        Ok(e)
    }

    fn parse_assign(&mut self, allow_in: bool) -> PResult<Expr> {
        let lhs = self.parse_ternary(allow_in)?;
        if let Expr::Ident(name) = &lhs {
            if self.is_punct("=>") {
                let name = name.clone();
                self.advance()?;
                let body = self.parse_arrow_body(allow_in)?;
                return Ok(Expr::Arrow(
                    vec![Param {
                        name,
                        default: None,
                        rest: false,
                    }],
                    Box::new(body),
                ));
            }
        }
        let op = match &self.tok {
            Tok::Punct(p @ ("=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>=")) => *p,
            _ => return Ok(lhs),
        };
        self.advance()?;
        let rhs = self.parse_assign(allow_in)?;
        Ok(Expr::Assign(op, Box::new(lhs), Box::new(rhs)))
    }

    fn parse_arrow_body(&mut self, allow_in: bool) -> PResult<ArrowBody> {
        if self.is_punct("{") {
            self.advance()?;
            Ok(ArrowBody::Block(self.parse_block_rest()?))
        } else {
            Ok(ArrowBody::Expr(self.parse_assign(allow_in)?))
        }
    }

    fn parse_ternary(&mut self, allow_in: bool) -> PResult<Expr> {
        let cond = self.parse_binary(0, allow_in)?;
        if self.is_punct("?") {
            self.advance()?;
            let then = self.parse_assign(true)?;
            self.expect_punct(":")?;
            let alt = self.parse_assign(allow_in)?;
            return Ok(Expr::Ternary(Box::new(cond), Box::new(then), Box::new(alt)));
        }
        Ok(cond)
    }

    fn binop_prec(&self, allow_in: bool) -> Option<(&'static str, u8)> {
        let p = match &self.tok {
            Tok::Punct(p) => *p,
            Tok::Ident(n) if n == "in" && allow_in => "in",
            Tok::Ident(n) if n == "instanceof" => "instanceof",
            Tok::Ident(n) if n == "like" => "like",
            _ => return None,
        };
        let prec = match p {
            "||" | "??" => 1,
            "&&" => 2,
            "|" => 3,
            "^" => 4,
            "&" => 5,
            "==" | "!=" | "===" | "!==" => 6,
            "<" | ">" | "<=" | ">=" | "in" | "instanceof" | "like" => 7,
            "<<" | ">>" | ">>>" => 8,
            "+" | "-" => 9,
            "*" | "/" | "%" => 10,
            _ => return None,
        };
        Some((p, prec))
    }

    fn parse_binary(&mut self, min_prec: u8, allow_in: bool) -> PResult<Expr> {
        let mut lhs = self.parse_unary(allow_in)?;
        loop {
            let (op, prec) = match self.binop_prec(allow_in) {
                Some(v) => v,
                None => break,
            };
            if prec < min_prec {
                break;
            }
            if op == "<<" {
                let save = self.save();
                self.advance()?;
                if self.is_ident("event") {
                    self.advance()?;
                    let decl = self.parse_event_rest()?;
                    lhs = Expr::EventAttach(Box::new(lhs), Box::new(decl));
                    continue;
                }
                self.restore(save);
            }
            self.advance()?;
            let rhs = self.parse_binary(prec + 1, allow_in)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self, allow_in: bool) -> PResult<Expr> {
        match &self.tok {
            Tok::Punct(p @ ("!" | "~" | "+" | "-" | "++" | "--")) => {
                let op = *p;
                self.advance()?;
                let e = self.parse_unary(allow_in)?;
                Ok(Expr::Unary(op, Box::new(e)))
            }
            Tok::Ident(n) if n == "typeof" => {
                self.advance()?;
                let e = self.parse_unary(allow_in)?;
                Ok(Expr::Unary("typeof", Box::new(e)))
            }
            Tok::Ident(n) if n == "void" => {
                self.advance()?;
                let e = self.parse_unary(allow_in)?;
                Ok(Expr::Unary("void", Box::new(e)))
            }
            Tok::Ident(n) if n == "delete" => {
                self.advance()?;
                let e = self.parse_unary(allow_in)?;
                Ok(Expr::Delete(Box::new(e)))
            }
            Tok::Ident(n) if n == "new" => {
                self.advance()?;
                let prim = self.parse_primary_for_new()?;
                let callee = self.parse_postfix_chain(prim, false, allow_in)?;
                let mut args = Vec::new();
                if self.is_punct("(") {
                    args = self.parse_args()?;
                }
                let e = Expr::New(Box::new(callee), args);
                self.parse_postfix_chain(e, true, allow_in)
            }
            _ => self.parse_postfix(allow_in),
        }
    }

    fn parse_primary_for_new(&mut self) -> PResult<Expr> {
        let e = self.parse_primary(true)?;
        Ok(e)
    }

    fn parse_postfix(&mut self, allow_in: bool) -> PResult<Expr> {
        let prim = self.parse_primary(allow_in)?;
        let mut e = self.parse_postfix_chain(prim, true, allow_in)?;
        while matches!(&self.tok, Tok::Punct(p) if *p == "++" || *p == "--") {
            let op = match &self.tok {
                Tok::Punct(p) => *p,
                _ => unreachable!(),
            };
            self.advance()?;
            e = Expr::Postfix(op, Box::new(e));
        }
        Ok(e)
    }

    fn parse_postfix_chain(&mut self, mut e: Expr, allow_call: bool, _allow_in: bool) -> PResult<Expr> {
        loop {
            if self.is_punct(".") {
                self.advance()?;
                let name = match &self.tok {
                    Tok::Ident(n) => n.clone(),
                    _ => return self.err("expected member name after '.'"),
                };
                if name.starts_with('$') && self.lx.peek_char_raw() == Some('(') {
                    self.lx.bump_raw();
                    let parts = self.parse_stringizer_parts()?;
                    self.advance()?;
                    e = Expr::Stringizer(Some(Box::new(e)), name, parts);
                    continue;
                }
                self.advance()?;
                e = Expr::Member(Box::new(e), name);
            } else if self.is_punct("[") {
                self.advance()?;
                let idx = self.parse_expression(true)?;
                self.expect_punct("]")?;
                e = Expr::Index(Box::new(e), Box::new(idx));
            } else if allow_call && self.is_punct("(") {
                let args = self.parse_args()?;
                e = Expr::Call(Box::new(e), args);
            } else if allow_call
                && self.is_punct("{")
                && matches!(e, Expr::Member(..) | Expr::Stringizer(..))
            {
                let arg = self.parse_primary(true)?;
                e = Expr::Call(Box::new(e), vec![arg]);
            } else if let Tok::Symbol(s) = &self.tok {
                let key = Expr::Symbol(s.clone());
                self.advance()?;
                e = Expr::Index(Box::new(e), Box::new(key));
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn parse_args(&mut self) -> PResult<Vec<Expr>> {
        self.expect_punct("(")?;
        let mut args = Vec::new();
        if self.eat_punct(")")? {
            return Ok(args);
        }
        loop {
            args.push(self.parse_assign(true)?);
            if !self.eat_punct(",")? {
                break;
            }
        }
        self.expect_punct(")")?;
        Ok(args)
    }

    fn try_parse_arrow_params(&mut self) -> PResult<Option<Vec<Param>>> {
        let save = self.save();
        self.advance()?;
        let mut params = Vec::new();
        if !self.is_punct(")") {
            loop {
                let name = match &self.tok {
                    Tok::Ident(n) => n.clone(),
                    _ => {
                        self.restore(save);
                        return Ok(None);
                    }
                };
                self.advance()?;
                let mut default = None;
                if self.is_punct("=") {
                    self.advance()?;
                    default = Some(self.parse_assign(true)?);
                }
                params.push(Param {
                    name,
                    default,
                    rest: false,
                });
                if self.is_punct(",") {
                    self.advance()?;
                    continue;
                }
                break;
            }
        }
        if !self.is_punct(")") {
            self.restore(save);
            return Ok(None);
        }
        self.advance()?;
        if !self.is_punct("=>") {
            self.restore(save);
            return Ok(None);
        }
        self.advance()?;
        Ok(Some(params))
    }

    fn parse_primary(&mut self, allow_in: bool) -> PResult<Expr> {
        match &self.tok {
            Tok::Int(v) => {
                let v = *v;
                self.advance()?;
                Ok(Expr::Int(v))
            }
            Tok::Float(v) => {
                let v = *v;
                self.advance()?;
                Ok(Expr::Float(v))
            }
            Tok::Unit(v, u) => {
                let (v, u) = (*v, u.clone());
                self.advance()?;
                Ok(Expr::Unit(v, u))
            }
            Tok::Str(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(Expr::Str(s))
            }
            Tok::Symbol(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(Expr::Symbol(s))
            }
            Tok::Regex(body, flags) => {
                let (b, f) = (body.clone(), flags.clone());
                self.advance()?;
                Ok(Expr::Regex(b, f))
            }
            Tok::Punct("(") => {
                if let Some(params) = self.try_parse_arrow_params()? {
                    let body = self.parse_arrow_body(allow_in)?;
                    return Ok(Expr::Arrow(params, Box::new(body)));
                }
                self.advance()?;
                // A parenthesized comma list is a TIScript tuple -- it decays to
                // an array so `view.windowMinSize = (w, h)` and multi-value
                // returns feed the same destructuring machinery.
                let first = self.parse_assign(true)?;
                if self.is_punct(",") {
                    let mut items = vec![first];
                    while self.eat_punct(",")? {
                        items.push(self.parse_assign(true)?);
                    }
                    self.expect_punct(")")?;
                    return Ok(Expr::Array(items));
                }
                self.expect_punct(")")?;
                Ok(first)
            }
            Tok::Punct("[") => {
                self.advance()?;
                let mut items = Vec::new();
                while !self.is_punct("]") {
                    items.push(self.parse_assign(true)?);
                    if !self.eat_punct(",")? {
                        break;
                    }
                }
                self.expect_punct("]")?;
                Ok(Expr::Array(items))
            }
            Tok::Punct("{") => {
                self.advance()?;
                let mut entries = Vec::new();
                while !self.is_punct("}") {
                    let key = match &self.tok {
                        Tok::Ident(n) => MapKey::Ident(n.clone()),
                        Tok::Str(s) => MapKey::Str(s.clone()),
                        Tok::Symbol(s) => MapKey::Symbol(s.clone()),
                        Tok::Int(v) => MapKey::Int(*v),
                        _ => return self.err("expected map key"),
                    };
                    self.advance()?;
                    let key = if let MapKey::Ident(mut name) = key {
                        while self.is_punct("-") {
                            self.advance()?;
                            name.push('-');
                            name.push_str(&self.expect_ident()?);
                        }
                        MapKey::Ident(name)
                    } else {
                        key
                    };
                    if self.eat_punct(":")? {
                        let value = self.parse_assign(true)?;
                        entries.push((key, value));
                    } else {
                        let value = match &key {
                            MapKey::Ident(n) => Expr::Ident(n.clone()),
                            _ => return self.err("expected ':' after map key"),
                        };
                        entries.push((key, value));
                    }
                    if !self.eat_punct(",")? {
                        break;
                    }
                }
                self.expect_punct("}")?;
                Ok(Expr::Map(entries))
            }
            Tok::Punct("<") => {
                let node = self.jsx_parse_element()?;
                self.advance()?;
                Ok(Expr::Jsx(node))
            }
            Tok::Punct(":") => {
                self.advance()?;
                let mut params = Vec::new();
                while let Tok::Ident(n) = &self.tok {
                    params.push(Param {
                        name: n.clone(),
                        default: None,
                        rest: false,
                    });
                    self.advance()?;
                    if !self.eat_punct(",")? {
                        break;
                    }
                }
                self.expect_punct(":")?;
                let body = self.parse_arrow_body(allow_in)?;
                Ok(Expr::Arrow(params, Box::new(body)))
            }
            Tok::Ident(name) => {
                let name = name.clone();
                match name.as_str() {
                    "true" => {
                        self.advance()?;
                        Ok(Expr::Bool(true))
                    }
                    "false" => {
                        self.advance()?;
                        Ok(Expr::Bool(false))
                    }
                    "null" => {
                        self.advance()?;
                        Ok(Expr::Null)
                    }
                    "undefined" => {
                        self.advance()?;
                        Ok(Expr::Undefined)
                    }
                    "this" => {
                        self.advance()?;
                        Ok(Expr::This)
                    }
                    "super" => {
                        self.advance()?;
                        Ok(Expr::Super)
                    }
                    "function" => {
                        self.advance()?;
                        if matches!(self.tok, Tok::Ident(_)) {
                            let f = self.parse_function_rest()?;
                            let name = f.path.join(".");
                            let _ = name;
                            return Ok(Expr::Function(f.params, f.body));
                        }
                        let params = self.parse_params()?;
                        self.expect_punct("{")?;
                        let body = self.parse_block_rest()?;
                        Ok(Expr::Function(params, body))
                    }
                    _ => {
                        if name.starts_with('$') && self.lx.peek_char_raw() == Some('(') {
                            self.lx.bump_raw();
                            let parts = self.parse_stringizer_parts()?;
                            self.advance()?;
                            return Ok(Expr::Stringizer(None, name, parts));
                        }
                        self.advance()?;
                        Ok(Expr::Ident(name))
                    }
                }
            }
            _ => self.err("unexpected token in expression"),
        }
    }

    fn jsx_skip_ws(&mut self) {
        while let Some(c) = self.lx.peek_char_raw() {
            if c.is_whitespace() {
                self.lx.bump_raw();
            } else {
                break;
            }
        }
    }

    fn jsx_read_name(&mut self) -> PResult<String> {
        let mut s = String::new();
        while let Some(c) = self.lx.peek_char_raw() {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                s.push(c);
                self.lx.bump_raw();
            } else {
                break;
            }
        }
        if s.is_empty() {
            return Err(ParseError {
                pos: self.tok_pos,
                message: "expected element name".into(),
            });
        }
        Ok(s)
    }

    fn jsx_parse_element(&mut self) -> PResult<JsxNode> {
        let tag = self.jsx_read_name()?;
        let mut node = JsxNode {
            tag,
            type_suffix: None,
            name_binding: None,
            attrs: Vec::new(),
            children: Vec::new(),
            self_closing: false,
        };
        if self.lx.peek_char_raw() == Some('|') {
            self.lx.bump_raw();
            node.type_suffix = Some(self.jsx_read_name()?);
            if self.lx.peek_char_raw() == Some('(') {
                self.lx.bump_raw();
                node.name_binding = Some(self.jsx_read_name()?);
                if self.lx.peek_char_raw() == Some(')') {
                    self.lx.bump_raw();
                } else {
                    return Err(ParseError {
                        pos: self.tok_pos,
                        message: "expected ')' after element name binding".into(),
                    });
                }
            }
        }
        loop {
            self.jsx_skip_ws();
            match self.lx.peek_char_raw() {
                None => {
                    return Err(ParseError {
                        pos: self.tok_pos,
                        message: "unterminated element".into(),
                    })
                }
                Some('>') => {
                    self.lx.bump_raw();
                    break;
                }
                Some('/') => {
                    self.lx.bump_raw();
                    if self.lx.peek_char_raw() == Some('>') {
                        self.lx.bump_raw();
                        node.self_closing = true;
                        return Ok(node);
                    }
                    return Err(ParseError {
                        pos: self.tok_pos,
                        message: "expected '>' after '/'".into(),
                    });
                }
                Some('.') => {
                    self.lx.bump_raw();
                    let class = self.jsx_read_name()?;
                    node.attrs.push(JsxAttr::Class(class));
                }
                Some('#') => {
                    self.lx.bump_raw();
                    let mut parts = Vec::new();
                    let mut text = String::new();
                    loop {
                        match self.lx.peek_char_raw() {
                            Some(c) if c.is_alphanumeric() || c == '_' || c == '-' => {
                                text.push(c);
                                self.lx.bump_raw();
                            }
                            Some('{') => {
                                if !text.is_empty() {
                                    parts.push(TextPart::Text(std::mem::take(&mut text)));
                                }
                                self.lx.bump_raw();
                                self.advance()?;
                                let e = self.parse_expression(true)?;
                                if !self.is_punct("}") {
                                    return self.err("expected '}' in element id");
                                }
                                parts.push(TextPart::Expr(e));
                            }
                            _ => break,
                        }
                    }
                    if !text.is_empty() {
                        parts.push(TextPart::Text(text));
                    }
                    node.attrs.push(JsxAttr::Id(parts));
                }
                Some('@') => {
                    self.lx.bump_raw();
                    if self.lx.peek_char_raw() != Some('{') {
                        return Err(ParseError {
                            pos: self.tok_pos,
                            message: "expected '{' after '@'".into(),
                        });
                    }
                    self.lx.bump_raw();
                    self.advance()?;
                    let e = self.parse_expression(true)?;
                    if !self.is_punct("}") {
                        return self.err("expected '}' after @ ref binding");
                    }
                    node.attrs.push(JsxAttr::Ref(e));
                }
                Some('{') => {
                    self.lx.bump_raw();
                    self.advance()?;
                    let e = self.parse_expression(true)?;
                    if !self.is_punct("}") {
                        return self.err("expected '}' after attribute map");
                    }
                    node.attrs.push(JsxAttr::Splat(e));
                }
                Some(c) if c.is_alphanumeric() || c == '_' || c == '-' => {
                    let mut name = self.jsx_read_name()?;
                    while self.lx.peek_char_raw() == Some(':') {
                        self.lx.bump_raw();
                        name.push(':');
                        name.push_str(&self.jsx_read_name()?);
                    }
                    self.jsx_skip_ws();
                    if self.lx.peek_char_raw() == Some('=') {
                        self.lx.bump_raw();
                        self.jsx_skip_ws();
                        let value = match self.lx.peek_char_raw() {
                            Some('"') | Some('\'') => {
                                let quote = self.lx.bump_raw().unwrap();
                                let mut s = String::new();
                                loop {
                                    match self.lx.bump_raw() {
                                        None => {
                                            return Err(ParseError {
                                                pos: self.tok_pos,
                                                message: "unterminated attribute string".into(),
                                            })
                                        }
                                        Some(c) if c == quote => break,
                                        Some('\\') => {
                                            if let Some(c) = self.lx.bump_raw() {
                                                match c {
                                                    'n' => s.push('\n'),
                                                    't' => s.push('\t'),
                                                    'r' => s.push('\r'),
                                                    other => s.push(other),
                                                }
                                            }
                                        }
                                        Some(c) => s.push(c),
                                    }
                                }
                                JsxAttrValue::Str(s)
                            }
                            Some('{') => {
                                self.lx.bump_raw();
                                self.advance()?;
                                let e = self.parse_expression(true)?;
                                if !self.is_punct("}") {
                                    return self.err("expected '}' after attribute expression");
                                }
                                JsxAttrValue::Expr(e)
                            }
                            _ => {
                                let word = self.jsx_read_name()?;
                                if let Ok(v) = word.parse::<i64>() {
                                    JsxAttrValue::Int(v)
                                } else if let Ok(v) = word.parse::<f64>() {
                                    JsxAttrValue::Float(v)
                                } else {
                                    JsxAttrValue::Ident(word)
                                }
                            }
                        };
                        node.attrs.push(JsxAttr::Named(name, Some(value)));
                    } else {
                        node.attrs.push(JsxAttr::Named(name, None));
                    }
                }
                Some(c) => {
                    return Err(ParseError {
                        pos: self.tok_pos,
                        message: format!("unexpected character '{}' in element tag", c),
                    })
                }
            }
        }
        loop {
            let text = self.lx.scan_jsx_text()?;
            if !text.trim().is_empty() {
                node.children.push(JsxChild::Text(text));
            }
            match self.lx.peek_char_raw() {
                Some('{') => {
                    self.lx.bump_raw();
                    self.advance()?;
                    let e = self.parse_expression(true)?;
                    if !self.is_punct("}") {
                        return self.err("expected '}' after element child expression");
                    }
                    node.children.push(JsxChild::Expr(e));
                }
                Some('<') => {
                    self.lx.bump_raw();
                    if self.lx.peek_char_raw() == Some('/') {
                        self.lx.bump_raw();
                        let close = self.jsx_read_name()?;
                        self.jsx_skip_ws();
                        if self.lx.peek_char_raw() == Some('>') {
                            self.lx.bump_raw();
                        } else {
                            return Err(ParseError {
                                pos: self.tok_pos,
                                message: "expected '>' in closing tag".into(),
                            });
                        }
                        if close != node.tag {
                            return Err(ParseError {
                                pos: self.tok_pos,
                                message: format!(
                                    "mismatched closing tag </{}> for <{}>",
                                    close, node.tag
                                ),
                            });
                        }
                        return Ok(node);
                    }
                    let child = self.jsx_parse_element()?;
                    node.children.push(JsxChild::Element(child));
                }
                _ => {
                    return Err(ParseError {
                        pos: self.tok_pos,
                        message: "unterminated element content".into(),
                    })
                }
            }
        }
    }

    fn parse_for_rest(&mut self) -> PResult<Stmt> {
        self.expect_punct("(")?;
        if self.is_ident("var") {
            let save = self.save();
            self.advance()?;
            if self.is_punct("(") {
                self.advance()?;
                let mut names = Vec::new();
                loop {
                    names.push(self.expect_ident()?);
                    if !self.eat_punct(",")? {
                        break;
                    }
                }
                self.expect_punct(")")?;
                if self.is_ident("in") {
                    self.advance()?;
                    let coll = self.parse_expression(true)?;
                    self.expect_punct(")")?;
                    let body = Box::new(self.parse_stmt()?);
                    let head = match names.len() {
                        1 => ForInHead::One(names.remove(0)),
                        2 => {
                            let b = names.remove(1);
                            ForInHead::Pair(names.remove(0), b)
                        }
                        3 => {
                            let c = names.remove(2);
                            let b = names.remove(1);
                            ForInHead::Triple(names.remove(0), b, c)
                        }
                        _ => return self.err("too many names in for-in head"),
                    };
                    return Ok(Stmt::ForIn(head, coll, body));
                }
                self.expect_punct("=")?;
                let init = self.parse_expression(false)?;
                let init_stmt = Stmt::VarDestructure(names, init);
                return self.parse_c_for_tail(Some(Box::new(init_stmt)));
            }
            let name = self.expect_ident()?;
            if self.is_ident("in") {
                self.advance()?;
                let coll = self.parse_expression(true)?;
                self.expect_punct(")")?;
                let body = Box::new(self.parse_stmt()?);
                return Ok(Stmt::ForIn(ForInHead::One(name), coll, body));
            }
            self.restore(save);
            self.advance()?;
            let decl = self.parse_var_rest(false)?;
            return self.parse_c_for_tail(Some(Box::new(decl)));
        }
        if self.is_punct(";") {
            return self.parse_c_for_tail(None);
        }
        let save = self.save();
        if let Tok::Ident(name) = self.tok.clone() {
            self.advance()?;
            if self.is_ident("in") {
                self.advance()?;
                let coll = self.parse_expression(true)?;
                self.expect_punct(")")?;
                let body = Box::new(self.parse_stmt()?);
                return Ok(Stmt::ForIn(ForInHead::One(name), coll, body));
            }
            self.restore(save);
        }
        let init = self.parse_expression(false)?;
        self.parse_c_for_tail(Some(Box::new(Stmt::Expr(init))))
    }

    fn parse_c_for_tail(&mut self, init: Option<Box<Stmt>>) -> PResult<Stmt> {
        self.expect_punct(";")?;
        let cond = if self.is_punct(";") {
            None
        } else {
            Some(self.parse_expression(true)?)
        };
        self.expect_punct(";")?;
        let update = if self.is_punct(")") {
            None
        } else {
            Some(self.parse_expression(true)?)
        };
        self.expect_punct(")")?;
        let body = Box::new(self.parse_stmt()?);
        Ok(Stmt::For(init, cond, update, body))
    }
}

pub fn parse(source: &str) -> LexResult<Vec<Stmt>> {
    Parser::new(source)?.parse_program()
}
