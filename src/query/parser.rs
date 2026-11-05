//! Recursive descent over the grammar:
//!
//! ```text
//! query   := or
//! or      := and (("or" | "||") and)*
//! and     := unary (("and" | "&&") unary)*
//! unary   := ("not" | "!") unary | "(" or ")" | "exists" "(" field ")" | test
//! test    := field op literal
//!          | field ("~" | "!~") text
//!          | field ("contains" | "startswith" | "endswith" | "glob") text
//!          | field ["not"] "in" "(" literal ("," literal)* ")"
//! ```

use super::lexer::{tokenize, Token, TokenKind};
use super::{CompareOp, Expr, QueryError, TextOp};
use crate::regex::Regex;
use crate::value::{format_number, Value};

const MAX_DEPTH: usize = 64;

pub fn parse(text: &str) -> Result<Expr, QueryError> {
    let tokens = tokenize(text)?;
    let mut parser = Parser { tokens, at: 0, end: text.chars().count() };
    if parser.tokens.is_empty() {
        return Err(QueryError { position: 0, message: "the query is empty".to_string() });
    }
    let expr = parser.or(0)?;
    if parser.at < parser.tokens.len() {
        return parser.error("unexpected text after the query");
    }
    Ok(expr)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    end: usize,
}

impl Parser {
    fn peek(&self) -> Option<&TokenKind> {
        self.tokens.get(self.at).map(|token| &token.kind)
    }

    fn position(&self) -> usize {
        self.tokens.get(self.at).map_or(self.end, |token| token.position)
    }

    fn error<T>(&self, message: &str) -> Result<T, QueryError> {
        Err(QueryError { position: self.position(), message: message.to_string() })
    }

    fn keyword(&self, word: &str) -> bool {
        matches!(self.peek(), Some(TokenKind::Word(found)) if found.eq_ignore_ascii_case(word))
    }

    fn eat_keyword(&mut self, word: &str) -> bool {
        let found = self.keyword(word);
        if found {
            self.at += 1;
        }
        found
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        let found = self.peek() == Some(kind);
        if found {
            self.at += 1;
        }
        found
    }

    fn or(&mut self, depth: usize) -> Result<Expr, QueryError> {
        let mut left = self.and(depth)?;
        while self.eat_keyword("or") || self.eat(&TokenKind::OrOr) {
            let right = self.and(depth)?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and(&mut self, depth: usize) -> Result<Expr, QueryError> {
        let mut left = self.unary(depth)?;
        while self.eat_keyword("and") || self.eat(&TokenKind::AndAnd) {
            let right = self.unary(depth)?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self, depth: usize) -> Result<Expr, QueryError> {
        if depth > MAX_DEPTH {
            return self.error("the query is nested too deeply");
        }
        if self.eat_keyword("not") || self.eat(&TokenKind::Bang) {
            return Ok(Expr::Not(Box::new(self.unary(depth + 1)?)));
        }
        if self.eat(&TokenKind::LeftParen) {
            let inner = self.or(depth + 1)?;
            if !self.eat(&TokenKind::RightParen) {
                return self.error("missing )");
            }
            return Ok(inner);
        }
        if self.keyword("exists") && self.tokens.get(self.at + 1).map(|t| &t.kind) == Some(&TokenKind::LeftParen) {
            self.at += 2;
            let field = self.field()?;
            if !self.eat(&TokenKind::RightParen) {
                return self.error("missing )");
            }
            return Ok(Expr::Exists(field));
        }
        self.test()
    }

    fn field(&mut self) -> Result<String, QueryError> {
        let name = match self.peek() {
            Some(TokenKind::Word(name) | TokenKind::QuotedField(name)) => name.clone(),
            // A key such as 404 lexes as a number, so it has to be written in backticks.
            Some(TokenKind::Number(_)) => {
                return self.error("expected a field name; put numeric names in backticks");
            }
            _ => return self.error("expected a field name"),
        };
        self.at += 1;
        Ok(name)
    }

    fn test(&mut self) -> Result<Expr, QueryError> {
        let field = self.field()?;
        if let Some(op) = self.compare_op() {
            self.at += 1;
            let value = self.literal()?;
            return Ok(Expr::Compare { field, op, value });
        }
        if matches!(self.peek(), Some(TokenKind::Tilde | TokenKind::NotTilde)) {
            let negated = self.peek() == Some(&TokenKind::NotTilde);
            self.at += 1;
            let position = self.position();
            let pattern = self.text()?;
            let regex = Regex::new(&pattern)
                .map_err(|error| QueryError { position, message: format!("bad pattern: {error}") })?;
            return Ok(Expr::Regex { field, regex, negated });
        }
        for (word, op) in [
            ("contains", TextOp::Contains),
            ("startswith", TextOp::StartsWith),
            ("endswith", TextOp::EndsWith),
            ("glob", TextOp::Glob),
        ] {
            if self.eat_keyword(word) {
                let needle = self.needle()?;
                return Ok(Expr::Text { field, op, needle });
            }
        }
        let negated = self.keyword("not");
        if negated {
            self.at += 1;
            if !self.keyword("in") {
                return self.error("expected in after not");
            }
        }
        if self.eat_keyword("in") {
            return self.list(field, negated);
        }
        self.error("expected an operator")
    }

    fn compare_op(&self) -> Option<CompareOp> {
        match self.peek()? {
            TokenKind::Eq => Some(CompareOp::Eq),
            TokenKind::Ne => Some(CompareOp::Ne),
            TokenKind::Lt => Some(CompareOp::Lt),
            TokenKind::Le => Some(CompareOp::Le),
            TokenKind::Gt => Some(CompareOp::Gt),
            TokenKind::Ge => Some(CompareOp::Ge),
            _ => None,
        }
    }

    fn list(&mut self, field: String, negated: bool) -> Result<Expr, QueryError> {
        if !self.eat(&TokenKind::LeftParen) {
            return self.error("expected ( after in");
        }
        let mut values = vec![self.literal()?];
        while self.eat(&TokenKind::Comma) {
            values.push(self.literal()?);
        }
        if !self.eat(&TokenKind::RightParen) {
            return self.error("missing )");
        }
        Ok(Expr::In { field, values, negated })
    }

    /// A value on the right hand side. Unquoted words are text, which saves quoting in the
    /// common `level = error`, so the right hand side can never name another field.
    fn literal(&mut self) -> Result<Value, QueryError> {
        let value = match self.peek() {
            Some(TokenKind::Number(number)) => Value::Number(*number),
            Some(TokenKind::Text(text)) => Value::Text(text.clone()),
            Some(TokenKind::Word(word)) => match word.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                "null" => Value::Null,
                other => Value::Text(other.to_string()),
            },
            _ => return self.error("expected a value"),
        };
        self.at += 1;
        Ok(value)
    }

    /// What a text test looks for. A bare number works too, read back in its shortest form,
    /// so `n contains 1.50` looks for `1.5` and needs quotes to mean otherwise.
    fn needle(&mut self) -> Result<String, QueryError> {
        if let Some(TokenKind::Number(number)) = self.peek() {
            let text = format_number(*number);
            self.at += 1;
            return Ok(text);
        }
        self.text()
    }

    fn text(&mut self) -> Result<String, QueryError> {
        let text = match self.peek() {
            Some(TokenKind::Text(text) | TokenKind::Word(text)) => text.clone(),
            _ => return self.error("expected text"),
        };
        self.at += 1;
        Ok(text)
    }
}
