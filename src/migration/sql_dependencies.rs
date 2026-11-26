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

    fn same_identifier(self, other: Self) -> bool {
        match other {
            Self::Word(s) if cfg!(feature = "postgres") => self.named(&s.to_ascii_lowercase()),
            Self::Word(s) => self.named(s),
            Self::Quoted(s, q) => self.named(&s.replace(&format!("{q}{q}"), &q.to_string())),
            _ => false,
        }
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
        // PostgreSQL E'...' uses backslash escapes as well as doubled quotes.
        // Consume the prefix and literal together, so whitespace after an
        // ordinary identifier named e cannot enable escape processing.
        let escaped_string =
            cfg!(feature = "postgres") && (sql.starts_with("e'") || sql.starts_with("E'"));
        if escaped_string {
            sql = &sql[1..];
        }
        let c = sql.chars().next().unwrap();
        if matches!(c, '\'' | '"' | '`' | '[') {
            let end = if c == '[' { ']' } else { c };
            let body = &sql[c.len_utf8()..];
            let mut rest = body;
            let mut value = body;
            sql = "";
            while let Some(n) = rest.find(|ch| ch == end || escaped_string && ch == '\\') {
                if escaped_string && rest.as_bytes()[n] == b'\\' {
                    let escaped = &rest[n + 1..];
                    rest = escaped
                        .chars()
                        .next()
                        .map_or("", |ch| &escaped[ch.len_utf8()..]);
                    continue;
                }
                let tail = &rest[n + 1..];
                if c != '['
                    && let Some(escaped) = tail.strip_prefix(end)
                {
                    rest = escaped;
                    continue;
                }
                value = &body[..body.len() - rest.len() + n];
                sql = tail;
                break;
            }
            result.push(if c == '\'' {
                Token::StringLiteral(value)
            } else {
                Token::Quoted(value, end)
            });
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

// A qualified reference to a different registered source (other.id) can
// coexist with the renamed table in the same query. Unknown qualifiers may
// be aliases of the affected source, so keep those subject to review.
pub(super) fn may_read_column(
    object: &SchemaObjectState,
    source: &str,
    column: &str,
    state: &super::ProjectState,
) -> bool {
    let tokens = tokens(&object.sql);
    let names: Vec<_> = state
        .tables()
        .iter()
        .map(|t| t.name.as_str())
        .chain(state.objects().iter().map(|o| o.name.as_str()))
        .collect();
    tokens.iter().enumerate().any(|(i, token)| {
        if !token.named(column) || i > 0 && tokens[i - 1].keyword("as") {
            return false;
        }
        if i < 2 || !matches!(tokens[i - 1], Token::Symbol('.')) {
            return true;
        }
        let qualifier = tokens[i - 2];
        let mut other = false;
        for (j, t) in tokens.iter().enumerate() {
            if !t.keyword("from") && !t.keyword("join") && !matches!(t, Token::Symbol(',')) {
                continue;
            }
            let mut tail = &tokens[j + 1..];
            while matches!(tail, [_, Token::Symbol('.'), ..]) {
                tail = &tail[2..];
            }
            let Some((relation, rest)) = tail.split_first() else {
                continue;
            };
            let Some(&bound) = names.iter().find(|n| relation.named(n)) else {
                continue;
            };
            let alias = if rest.first().is_some_and(|t| t.keyword("as")) {
                rest.get(1)
            } else {
                rest.first()
            };
            if qualifier.named(bound) || alias.is_some_and(|a| a.same_identifier(qualifier)) {
                if super::catalog::same_name(bound, source) {
                    return true;
                }
                other = true;
            }
        }
        !other
    })
}

// In an ordinary SELECT, an explicit projection alias preserves the public
// name for consumers of the view, even if its input column changed.
#[cfg(feature = "postgres")]
pub(super) fn exports_alias(sql: &str, column: &str) -> bool {
    let tokens = tokens(sql);
    let mut i = 0;
    let mut projection = false;
    while let Some(token) = tokens.get(i) {
        if matches!(token, Token::Symbol('(')) {
            i = group_end(&tokens, i);
            continue;
        }
        if token.keyword("select") {
            projection = true;
        }
        if projection
            && ["from", "union", "except", "intersect"]
                .iter()
                .any(|w| token.keyword(w))
        {
            return false;
        }
        if projection && token.keyword("as") && tokens.get(i + 1).is_some_and(|t| t.named(column)) {
            return true;
        }
        i += 1;
    }
    false
}

// Return the index after a balanced parenthesized group, or the end of
// the input for an unterminated group.
fn group_end(tokens: &[Token<'_>], start: usize) -> usize {
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate().skip(start) {
        match token {
            Token::Symbol('(') => depth += 1,
            Token::Symbol(')') if depth <= 1 => return i + 1,
            Token::Symbol(')') => depth -= 1,
            _ => {}
        }
    }
    tokens.len()
}

// WITH bindings apply to unqualified source names within this query only.
// PostgreSQL non-recursive bindings start after their own defining query.
fn cte_scopes(tokens: &[Token<'_>], name: &str) -> Vec<std::ops::Range<usize>> {
    let mut scopes = Vec::new();
    for (start, _) in tokens.iter().enumerate().filter(|(_, t)| t.keyword("with")) {
        let recursive = tokens
            .get(start + 1)
            .is_some_and(|t| t.keyword("recursive"));
        let mut i = start + 1 + usize::from(recursive);
        while let Some(binding) = tokens.get(i) {
            let shadows = binding.named(name);
            i += 1;
            if matches!(tokens.get(i), Some(Token::Symbol('('))) {
                i = group_end(tokens, i);
            }
            if !tokens.get(i).is_some_and(|t| t.keyword("as")) {
                break;
            }
            i += 1;
            if tokens.get(i).is_some_and(|t| t.keyword("not")) {
                i += 1;
            }
            if tokens.get(i).is_some_and(|t| t.keyword("materialized")) {
                i += 1;
            }
            if !matches!(tokens.get(i), Some(Token::Symbol('('))) {
                break;
            }
            i = group_end(tokens, i);
            if shadows {
                let mut end = i;
                while end < tokens.len() {
                    match tokens[end] {
                        Token::Symbol('(') => end = group_end(tokens, end),
                        Token::Symbol(')' | ';') => break,
                        _ => end += 1,
                    }
                }
                let visible = if cfg!(feature = "sqlite") || recursive {
                    start
                } else {
                    i
                };
                scopes.push(visible..end);
            }
            if !matches!(tokens.get(i), Some(Token::Symbol(','))) {
                break;
            }
            i += 1;
        }
    }
    scopes
}

// Examine the last component of a possibly schema-qualified relation name.
fn names_relation(mut tail: &[Token<'_>], name: &str, source: bool, mut shadowed: bool) -> bool {
    while let Some((head, rest)) = tail.split_first() {
        if (cfg!(feature = "postgres")
            && source
            && (head.keyword("only") || head.keyword("lateral")))
            || matches!(head, Token::Symbol('('))
        {
            tail = rest;
        } else if let [Token::Symbol('.'), qualified @ ..] = rest {
            shadowed = false;
            tail = qualified;
        } else {
            return !shadowed
                && match head {
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
    let scopes = cte_scopes(&tokens, name);
    let mut group_start = false;
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
        let source = from || in_from && matches!(token, Token::Symbol(','));
        if (source
            || trigger_target
            || update_target
            || token.keyword("into")
            || cfg!(feature = "sqlite")
                && token.keyword("in")
                && !matches!(tail.first(), Some(Token::Symbol('(')))
            || token.keyword("references"))
            && names_relation(
                tail,
                name,
                source,
                source && scopes.iter().any(|scope| scope.contains(&i)),
            )
        {
            return true;
        }
        match token {
            Token::Symbol('(') => {
                parents.push(in_from);
                in_from = group_start
                    && !tail.first().is_some_and(|t| {
                        t.keyword("select") || t.keyword("with") || t.keyword("values")
                    });
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
        group_start = source || group_start && matches!(token, Token::Symbol('('));
    }
    false
}
