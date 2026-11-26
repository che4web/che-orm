//! Relation names in registered CREATE statements. Keep quoted identifiers
//! distinct from keywords, literals and comments when ordering schema changes.
use super::{SchemaObjectKind, SchemaObjectState};

#[derive(Clone, Copy)]
enum Token<'a> {
    Word(&'a str),
    Quoted(&'a str, char),
    StringLiteral(&'a str),
    Literal,
    Symbol(char),
}

impl Token<'_> {
    fn keyword(self, word: &str) -> bool {
        matches!(self, Self::Word(s) if s.eq_ignore_ascii_case(word))
    }

    fn named(self, name: &str) -> bool {
        match self {
            Self::Word(s) => {
                #[cfg(feature = "postgres")]
                {
                    s.to_ascii_lowercase() == name
                }
                #[cfg(feature = "sqlite")]
                {
                    s.eq_ignore_ascii_case(name)
                }
            }
            Self::Quoted(s, q) => {
                let escaped = format!("{q}{q}");
                super::catalog::same_name(&s.replace(&escaped, &q.to_string()), name)
            }
            _ => false,
        }
    }
}

fn tokens(mut sql: &str) -> Vec<Token<'_>> {
    let mut result = Vec::new();
    while !sql.is_empty() {
        if sql.starts_with("--") {
            sql = sql.find('\n').map_or("", |n| &sql[n..]);
            continue;
        }
        if let Some(comment) = sql.strip_prefix("/*") {
            sql = comment.find("*/").map_or("", |n| &comment[n + 2..]);
            continue;
        }
        if cfg!(feature = "postgres")
            && let Some(rest) = sql.strip_prefix('$')
            && let Some((tag, body)) = rest.split_once('$')
            && tag.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            let delimiter = &sql[..tag.len() + 2];
            sql = body.split_once(delimiter).map_or("", |(_, tail)| tail);
            result.push(Token::Literal);
            continue;
        }
        let c = sql.chars().next().unwrap();
        if matches!(c, '\'' | '"' | '`' | '[') {
            let end = if c == '[' { ']' } else { c };
            let body = &sql[c.len_utf8()..];
            let mut rest = body;
            while let Some(n) = rest.find(end) {
                let tail = &rest[n + 1..];
                if c != '['
                    && let Some(escaped) = tail.strip_prefix(end)
                {
                    rest = escaped;
                    continue;
                }
                rest = tail;
                break;
            }
            let consumed = body.len() - rest.len();
            let value = &body[..consumed.saturating_sub(1)];
            result.push(if c == '\'' {
                Token::StringLiteral(value)
            } else {
                Token::Quoted(value, end)
            });
            sql = if consumed == 0 { "" } else { rest };
        } else if c.is_alphanumeric() || c == '_' || !c.is_ascii() && !c.is_whitespace() {
            let n = sql
                .find(|ch: char| {
                    ch.is_whitespace()
                        || ch.is_ascii() && !ch.is_alphanumeric() && !matches!(ch, '_' | '$')
                })
                .unwrap_or(sql.len());
            result.push(Token::Word(&sql[..n]));
            sql = &sql[n..];
        } else {
            if !c.is_whitespace() {
                result.push(Token::Symbol(c));
            }
            sql = &sql[c.len_utf8()..];
        }
    }
    result
}

pub(super) fn mentions_identifier(sql: &str, name: &str) -> bool {
    tokens(sql).iter().any(|t| t.named(name))
}

// Examine the last component of a possibly schema-qualified relation name.
fn names_relation(mut tail: &[Token<'_>], name: &str) -> bool {
    while let Some((head, rest)) = tail.split_first() {
        if head.keyword("only") || head.keyword("lateral") || matches!(head, Token::Symbol('(')) {
            tail = rest;
        } else if let [Token::Symbol('.'), qualified @ ..] = rest {
            tail = qualified;
        } else {
            return match head {
                Token::StringLiteral(s) => {
                    cfg!(feature = "sqlite") && s.replace("''", "'").eq_ignore_ascii_case(name)
                }
                _ => head.named(name),
            };
        }
    }
    false
}

pub(super) fn references(object: &SchemaObjectState, name: &str) -> bool {
    let tokens = tokens(&object.sql);
    let mut trigger_header = object.kind == SchemaObjectKind::Trigger;
    let mut in_from = false;
    let mut parents = Vec::new();
    for (i, &token) in tokens.iter().enumerate() {
        let tail = &tokens[i + 1..];
        let from = token.keyword("from") || token.keyword("join");
        let trigger_target = trigger_header && token.keyword("on");
        if trigger_target {
            trigger_header = false;
        }
        let update_target = token.keyword("update") && !trigger_header;
        // UPDATE OR <conflict action> is a SQLite DML target in a trigger body.
        let tail = if update_target && tail.first().is_some_and(|t| t.keyword("or")) {
            tail.get(2..).unwrap_or_default()
        } else {
            tail
        };
        if (from
            || trigger_target
            || update_target
            || token.keyword("into")
            || cfg!(feature = "sqlite")
                && token.keyword("in")
                && !matches!(tail.first(), Some(Token::Symbol('(')))
            || token.keyword("references")
            || in_from && matches!(token, Token::Symbol(',')))
            && names_relation(tail, name)
        {
            return true;
        }
        match token {
            Token::Symbol('(') => {
                parents.push(in_from);
                in_from = false;
            }
            Token::Symbol(')') => in_from = parents.pop().unwrap_or(false),
            Token::Symbol(';') => in_from = false,
            _ if from => in_from = true,
            _ if [
                "where",
                "group",
                "having",
                "order",
                "limit",
                "window",
                "union",
                "except",
                "intersect",
                "returning",
                "set",
            ]
            .iter()
            .any(|w| token.keyword(w)) =>
            {
                in_from = false
            }
            _ => {}
        }
    }
    false
}
