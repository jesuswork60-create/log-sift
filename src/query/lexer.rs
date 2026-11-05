//! Splits query text into tokens.

use super::QueryError;
use crate::value::parse_number;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// A field name, keyword or unquoted text.
    Word(String),
    /// A field name in backticks, for names with odd characters. Never a keyword.
    QuotedField(String),
    Text(String),
    Number(f64),
    LeftParen,
    RightParen,
    Comma,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Tilde,
    NotTilde,
    AndAnd,
    OrOr,
    Bang,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub position: usize,
}

fn ends_word(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | ',' | '=' | '!' | '<' | '>' | '~' | '"' | '\'' | '`' | '&' | '|')
}

pub fn tokenize(text: &str) -> Result<Vec<Token>, QueryError> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        let position = at;
        let next = chars.get(at + 1).copied();
        let two = |kind: TokenKind| (kind, 2);
        let one = |kind: TokenKind| (kind, 1);
        let fixed = match (c, next) {
            ('(', _) => Some(one(TokenKind::LeftParen)),
            (')', _) => Some(one(TokenKind::RightParen)),
            (',', _) => Some(one(TokenKind::Comma)),
            ('=', Some('=')) => Some(two(TokenKind::Eq)),
            ('=', _) => Some(one(TokenKind::Eq)),
            ('!', Some('=')) => Some(two(TokenKind::Ne)),
            ('!', Some('~')) => Some(two(TokenKind::NotTilde)),
            ('!', _) => Some(one(TokenKind::Bang)),
            ('<', Some('=')) => Some(two(TokenKind::Le)),
            ('<', _) => Some(one(TokenKind::Lt)),
            ('>', Some('=')) => Some(two(TokenKind::Ge)),
            ('>', _) => Some(one(TokenKind::Gt)),
            ('~', _) => Some(one(TokenKind::Tilde)),
            ('&', Some('&')) => Some(two(TokenKind::AndAnd)),
            ('|', Some('|')) => Some(two(TokenKind::OrOr)),
            _ => None,
        };
        if let Some((kind, width)) = fixed {
            tokens.push(Token { kind, position });
            at += width;
        } else if c.is_whitespace() {
            at += 1;
        } else if c == '"' || c == '\'' {
            let (value, after) = quoted(&chars, at)?;
            tokens.push(Token { kind: TokenKind::Text(value), position });
            at = after;
        } else if c == '`' {
            let close = chars[at + 1..].iter().position(|other| *other == '`').ok_or(QueryError {
                position,
                message: "unterminated `".to_string(),
            })?;
            let name: String = chars[at + 1..at + 1 + close].iter().collect();
            tokens.push(Token { kind: TokenKind::QuotedField(name), position });
            at += close + 2;
        } else if ends_word(c) {
            return Err(QueryError { position, message: format!("unexpected {c:?}") });
        } else {
            let start = at;
            while at < chars.len() && !ends_word(chars[at]) {
                at += 1;
            }
            let word: String = chars[start..at].iter().collect();
            let kind = match parse_number(&word) {
                Some(number) => TokenKind::Number(number),
                None => TokenKind::Word(word),
            };
            tokens.push(Token { kind, position });
        }
    }
    Ok(tokens)
}

fn quoted(chars: &[char], start: usize) -> Result<(String, usize), QueryError> {
    let quote = chars[start];
    let mut value = String::new();
    let mut at = start + 1;
    while at < chars.len() {
        let c = chars[at];
        if c == quote {
            return Ok((value, at + 1));
        }
        if c == '\\' {
            at += 1;
            match chars.get(at) {
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some('r') => value.push('\r'),
                // Everything else keeps its backslash, so regular expressions read naturally.
                Some(other) if *other == quote || *other == '\\' => value.push(*other),
                Some(other) => {
                    value.push('\\');
                    value.push(*other);
                }
                None => break,
            }
        } else {
            value.push(c);
        }
        at += 1;
    }
    Err(QueryError { position: start, message: "unterminated string".to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<TokenKind> {
        tokenize(text).unwrap().into_iter().map(|token| token.kind).collect()
    }

    fn word(text: &str) -> TokenKind {
        TokenKind::Word(text.to_string())
    }

    #[test]
    fn operators_with_and_without_spaces() {
        assert_eq!(kinds("a>=5"), vec![word("a"), TokenKind::Ge, TokenKind::Number(5.0)]);
        assert_eq!(kinds("a == b"), vec![word("a"), TokenKind::Eq, word("b")]);
        assert_eq!(kinds("a!=b"), vec![word("a"), TokenKind::Ne, word("b")]);
        assert_eq!(kinds("a!~b"), vec![word("a"), TokenKind::NotTilde, word("b")]);
        assert_eq!(kinds("!a"), vec![TokenKind::Bang, word("a")]);
        assert_eq!(kinds("a&&b||c"), vec![word("a"), TokenKind::AndAnd, word("b"), TokenKind::OrOr, word("c")]);
    }

    #[test]
    fn words_may_hold_path_characters() {
        assert_eq!(kinds("/api/v1.2"), vec![word("/api/v1.2")]);
        assert_eq!(kinds("user.id"), vec![word("user.id")]);
        assert_eq!(kinds("@timestamp"), vec![word("@timestamp")]);
        assert_eq!(kinds("2024-01-01"), vec![word("2024-01-01")]);
    }

    #[test]
    fn numbers() {
        assert_eq!(kinds("-1.5"), vec![TokenKind::Number(-1.5)]);
        assert_eq!(kinds("1e3"), vec![TokenKind::Number(1000.0)]);
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(kinds("\"a b\""), vec![TokenKind::Text("a b".into())]);
        assert_eq!(kinds("'it'"), vec![TokenKind::Text("it".into())]);
        assert_eq!(kinds(r#""say \"hi\"""#), vec![TokenKind::Text("say \"hi\"".into())]);
        assert_eq!(kinds(r"'a\nb'"), vec![TokenKind::Text("a\nb".into())]);
        assert_eq!(kinds(r"'\d+\.'"), vec![TokenKind::Text(r"\d+\.".into())]);
        assert_eq!(kinds(r"'a\\b'"), vec![TokenKind::Text(r"a\b".into())]);
    }

    #[test]
    fn backtick_fields() {
        assert_eq!(kinds("`odd name`"), vec![TokenKind::QuotedField("odd name".into())]);
    }

    #[test]
    fn errors_point_at_the_problem() {
        assert_eq!(tokenize("a = \"open").unwrap_err().position, 4);
        assert_eq!(tokenize("`open").unwrap_err().position, 0);
        assert_eq!(tokenize("a & b").unwrap_err().position, 2);
        assert!(tokenize("a | b").is_err());
    }

    #[test]
    fn positions_count_characters() {
        let tokens = tokenize("é = 1").unwrap();
        assert_eq!(tokens[1].position, 2);
    }
}
