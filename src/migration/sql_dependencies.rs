//! Relation names in registered CREATE statements. Keep quoted identifiers
//! distinct from keywords, literals and comments when ordering schema changes.
use super::{SchemaObjectKind, SchemaObjectState};

#[derive(Clone, Copy, PartialEq, Eq)]
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

    fn relation_named(self, name: &str) -> bool {
        match self {
            Self::StringLiteral(s) if cfg!(feature = "sqlite") => {
                s.replace("''", "'").eq_ignore_ascii_case(name)
            }
            _ => self.named(name),
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

fn after_block_comment(mut sql: &str) -> &str {
    let mut depth = 1;
    while let Some(index) = sql.find(['/', '*']) {
        sql = &sql[index..];
        if let Some(tail) = sql.strip_prefix("*/") {
            depth -= 1;
            if depth == 0 {
                return tail;
            }
            sql = tail;
        } else if cfg!(feature = "postgres")
            && let Some(tail) = sql.strip_prefix("/*")
        {
            depth += 1;
            sql = tail;
        } else {
            sql = &sql[1..];
        }
    }
    ""
}

fn tokens(mut sql: &str) -> Vec<Token<'_>> {
    let mut result = Vec::new();
    while !sql.is_empty() {
        if sql.starts_with("--") {
            sql = sql.find('\n').map_or("", |n| &sql[n..]);
            continue;
        }
        if let Some(comment) = sql.strip_prefix("/*") {
            sql = after_block_comment(comment);
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
        if matches!(c, '\'' | '"') || cfg!(feature = "sqlite") && matches!(c, '`' | '[') {
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

// SQLite's own completeness scanner understands the semicolons inside
// CREATE TRIGGER ... BEGIN ... END. The remaining tokens must be empty.
#[cfg(feature = "sqlite")]
pub(super) fn is_single_statement(sql: &str) -> bool {
    if sql.contains('\0') {
        return false;
    }
    sqlite_statement_end(sql).is_none_or(|end| {
        tokens(&sql[end + 1..])
            .iter()
            .all(|token| matches!(token, Token::Symbol(';')))
    })
}

#[cfg(feature = "sqlite")]
pub(super) fn sqlite_statement_end(sql: &str) -> Option<usize> {
    sql.match_indices(';').map(|(i, _)| i).find(|&end| {
        let Ok(prefix) = std::ffi::CString::new(&sql[..=end]) else {
            return false;
        };
        // SAFETY: prefix is a valid, NUL-terminated string for this call.
        unsafe { crate::rusqlite::ffi::sqlite3_complete(prefix.as_ptr()) != 0 }
    })
}

#[cfg(feature = "postgres")]
pub(super) fn is_single_statement(sql: &str) -> bool {
    let tokens = tokens(sql);
    tokens
        .iter()
        .skip_while(|token| !matches!(token, Token::Symbol(';')))
        .all(|token| matches!(token, Token::Symbol(';')))
        && !sql.contains('\0')
}

pub(super) fn mentions_identifier(sql: &str, name: &str) -> bool {
    tokens(sql).iter().any(|t| t.named(name))
}

// Recognize direct projections over one relation. Keeping this proof narrow
// excludes distinctness, positional expressions, set operations and any
// change to the source or meaning of existing columns.
struct DirectView<'a> {
    table: Token<'a>,
    fields: Vec<(&'a str, &'a [Token<'a>])>,
}

fn direct_view<'a>(sql: &'a [Token<'a>]) -> Option<DirectView<'a>> {
    let [create, view, Token::Word(_), as_kw, select, query @ ..] = sql else {
        return None;
    };
    if !create.keyword("create")
        || !view.keyword("view")
        || !as_kw.keyword("as")
        || !select.keyword("select")
    {
        return None;
    }
    let query = query.strip_suffix(&[Token::Symbol(';')]).unwrap_or(query);
    let [fields @ .., from, table @ Token::Word(_)] = query else {
        return None;
    };
    if !from.keyword("from") {
        return None;
    }
    let mut names = std::collections::BTreeSet::new();
    let mut result = Vec::new();
    for field in fields.split(|token| *token == Token::Symbol(',')) {
        let name = match field {
            [Token::Word(column)] if !column.starts_with(|c: char| c.is_ascii_digit()) => column,
            [
                Token::Word(_) | Token::StringLiteral(_),
                alias,
                Token::Word(name),
            ] if alias.keyword("as") => name,
            _ => return None,
        };
        if !names.insert(name.to_ascii_lowercase()) {
            return None;
        }
        result.push((*name, field));
    }
    Some(DirectView {
        table: *table,
        fields: result,
    })
}

pub(super) fn direct_view_columns(object: &SchemaObjectState) -> Vec<String> {
    direct_view(&tokens(&object.sql)).map_or_else(Vec::new, |view| {
        view.fields
            .into_iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect()
    })
}

pub(super) fn appended_view_columns(
    before: &SchemaObjectState,
    after: &SchemaObjectState,
) -> Option<Vec<String>> {
    if before.kind != SchemaObjectKind::View
        || after.kind != SchemaObjectKind::View
        || before.name != after.name
        || before.app != after.app
        || ![before, after].iter().all(|o| {
            o.sql
                .trim_start()
                .get(..6)
                .is_some_and(|s| s.eq_ignore_ascii_case("create"))
        })
    {
        return None;
    }
    let old_tokens = tokens(&before.sql);
    let new_tokens = tokens(&after.sql);
    let old = direct_view(&old_tokens)?;
    let new = direct_view(&new_tokens)?;
    if old.table != new.table
        || new.fields.len() <= old.fields.len()
        || !new.fields.starts_with(&old.fields)
    {
        return None;
    }
    Some(
        new.fields[old.fields.len()..]
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect(),
    )
}

// Bind sources within each SELECT. Columns read by a derived table or CTE
// are accounted for by its defining query, even when its consumer selects d.*.
enum Source<'a> {
    Relation(Token<'a>),
    Group(Vec<Binding<'a>>),
    Query(std::ops::Range<usize>),
    Unknown,
}

struct Binding<'a> {
    alias: Option<Token<'a>>,
    declaration: std::ops::Range<usize>,
    source: Source<'a>,
}

struct Select<'a> {
    range: std::ops::Range<usize>,
    bindings: Vec<Binding<'a>>,
}

pub(super) struct SqlReads<'a> {
    tokens: Vec<Token<'a>>,
    selects: Vec<Select<'a>>,
    ctes: Vec<Cte<'a>>,
    kind: SchemaObjectKind,
    named_projections: std::collections::BTreeSet<usize>,
}

impl<'a> SqlReads<'a> {
    pub(super) fn new(object: &'a SchemaObjectState) -> Self {
        let tokens = tokens(&object.sql);
        let ctes = cte_bindings(&tokens);
        let selects = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| t.keyword("select"))
            .map(|(start, _)| select_sources(&tokens, start, &ctes))
            .collect();
        let mut reads = Self {
            tokens,
            selects,
            ctes,
            kind: object.kind,
            named_projections: Default::default(),
        };
        // Propagate projection demand from consumers through successive derived
        // tables and CTEs. Unresolved or recursive bindings retain their shape.
        loop {
            let narrowed: Vec<_> = reads
                .selects
                .iter()
                .filter(|s| {
                    !reads.named_projections.contains(&s.range.start)
                        && reads.only_named_consumers(s)
                })
                .map(|s| s.range.start)
                .collect();
            if narrowed.is_empty() {
                break;
            }
            reads.named_projections.extend(narrowed);
        }
        reads
    }

    pub(super) fn copies_rows(&self) -> bool {
        self.kind == SchemaObjectKind::Table
            && (!self.selects.is_empty()
                || (0..self.tokens.len()).any(|i| table_query(&self.tokens, i)))
    }

    pub(super) fn references(&self, name: &str) -> bool {
        references_tokens(&self.tokens, self.kind, name, &self.ctes)
    }

    fn scopes(&self, position: usize) -> impl Iterator<Item = &Select<'a>> {
        self.selects
            .iter()
            .rev()
            .filter(move |s| s.range.contains(&position))
    }

    fn source_reads(
        &self,
        binding: &Binding<'_>,
        table: &str,
        path: Option<&mut Vec<usize>>,
    ) -> Option<bool> {
        match &binding.source {
            Source::Relation(t) => Some(t.relation_named(table)),
            // The nested query accounts for its own stars and named inputs. An
            // explicit reference to d.col can also read col through a nested star.
            Source::Query(body) => {
                let Some(path) = path else { return Some(false) };
                if path.contains(&body.start) {
                    return None;
                }
                path.push(body.start);
                let scope = self.selects.iter().find(|s| s.range == *body);
                let mut result = Some(false);
                for i in body.clone() {
                    if scope.is_some_and(|s| !self.exports_star(s, i)) {
                        continue;
                    }
                    match self.star_reads(i, table, Some(path)) {
                        Some(true) => {
                            result = Some(true);
                            break;
                        }
                        None => result = None,
                        Some(false) => {}
                    }
                }
                path.pop();
                result
            }
            Source::Group(members) => self.any_source_reads(members, table, path),
            Source::Unknown => None,
        }
    }

    fn qualified(&self, i: usize, table: &str, path: Option<&mut Vec<usize>>) -> Option<bool> {
        let qualifier = self.tokens[i];
        for scope in self.scopes(i) {
            if let Some(binding) = scope
                .bindings
                .iter()
                .find(|b| b.alias.is_some_and(|a| a.same_identifier(qualifier)))
            {
                return self.source_reads(binding, table, path);
            }
        }
        if self.kind == SchemaObjectKind::Trigger
            && (qualifier.named("new") || qualifier.named("old"))
            && let Some(on) = self.tokens.iter().position(|t| t.keyword("on"))
        {
            return Some(names_relation(&self.tokens[on + 1..], table, false, false));
        }
        None
    }

    fn star_reads(&self, i: usize, table: &str, path: Option<&mut Vec<usize>>) -> Option<bool> {
        let t = self.tokens[i];
        if table_query(&self.tokens, i) {
            return Some(names_relation(
                &self.tokens[i + 1..],
                table,
                true,
                self.ctes
                    .iter()
                    .any(|c| c.name.named(table) && c.scope.contains(&i)),
            ));
        }
        if i == 0 || !matches!(t, Token::Symbol('*')) {
            return Some(false);
        }
        if i >= 2 && matches!(self.tokens[i - 1], Token::Symbol('.')) {
            return self.qualified(i - 2, table, path);
        }
        if !(matches!(self.tokens[i - 1], Token::Symbol(','))
            || ["select", "distinct", "all", "returning"]
                .iter()
                .any(|w| self.tokens[i - 1].keyword(w)))
        {
            return Some(false);
        }
        let scope = self.scopes(i).next()?;
        self.any_source_reads(&scope.bindings, table, path)
    }

    fn any_source_reads(
        &self,
        bindings: &[Binding<'_>],
        table: &str,
        mut path: Option<&mut Vec<usize>>,
    ) -> Option<bool> {
        let mut result = Some(false);
        for binding in bindings {
            match self.source_reads(binding, table, path.as_deref_mut()) {
                Some(true) => return Some(true),
                None => result = None,
                Some(false) => {}
            }
        }
        result
    }

    // A trigger header's UPDATE describes its event, not a write.
    pub(super) fn checks_write(&self, table: &str, column: &str) -> bool {
        let mut header = self.kind == SchemaObjectKind::Trigger;
        self.tokens.iter().enumerate().any(|(i, t)| {
            if header {
                header = !t.keyword("on");
                return false;
            }
            if !(t.keyword("into") || t.keyword("update")) {
                return false;
            }
            let mut tail = &self.tokens[i + 1..];
            if t.keyword("update") && matches!(tail, [kw, _, ..] if kw.keyword("or")) {
                tail = &tail[2..];
            }
            if !names_relation(tail, table, false, false) {
                return false;
            }
            if t.keyword("into") {
                return true;
            }
            let Some(set) = tail.iter().position(|kw| kw.keyword("set")) else {
                return true;
            };
            let mut assignments = &tail[set + 1..];
            while let Some(&target) = assignments.first() {
                // Tuple assignments may name several columns.
                if matches!(target, Token::Symbol('(')) || target.named(column) {
                    return true;
                }
                let mut j = 1;
                while j < assignments.len() {
                    let t = assignments[j];
                    if matches!(t, Token::Symbol(',')) {
                        break;
                    }
                    if matches!(t, Token::Symbol(';'))
                        || ["where", "from", "returning", "order", "limit"]
                            .iter()
                            .any(|w| t.keyword(w))
                    {
                        return false;
                    }
                    j = if matches!(t, Token::Symbol('(')) {
                        group_end(assignments, j)
                    } else {
                        j + 1
                    };
                }
                assignments = assignments.get(j + 1..).unwrap_or_default();
            }
            false
        })
    }

    fn insert_tail(&self, i: usize, table: &str) -> Option<&[Token<'a>]> {
        let mut tail = &self.tokens[i + 1..];
        if !self.tokens[i].keyword("into") || !names_relation(tail, table, false, false) {
            return None;
        }
        while matches!(tail, [_, Token::Symbol('.'), ..]) {
            tail = &tail[2..];
        }
        tail = &tail[1..];
        if matches!(tail,[kw,_,..] if kw.keyword("as")) {
            tail = &tail[2..];
        }
        Some(tail)
    }

    pub(super) fn uses_default(&self, table: &str, column: &str, ordinal: usize) -> bool {
        (0..self.tokens.len()).any(|i| {
            let Some(mut tail) = self.insert_tail(i, table) else {
                return false;
            };
            let mut field = ordinal;
            let explicit = matches!(tail.first(), Some(Token::Symbol('(')));
            if explicit {
                let end = group_end(tail, 0);
                let Some(pos) = tail[1..end]
                    .iter()
                    .filter(|t| !matches!(t, Token::Symbol(',')))
                    .position(|t| t.named(column))
                else {
                    return true;
                };
                field = pos;
                tail = &tail[end..];
            }
            if !tail.first().is_some_and(|t| t.keyword("values")) {
                return !explicit
                    || tail
                        .iter()
                        .take_while(|t| !matches!(t, Token::Symbol(';')))
                        .any(|t| t.keyword("default"));
            }
            tail = &tail[1..];
            while matches!(tail.first(), Some(Token::Symbol('('))) {
                let end = group_end(tail, 0);
                let mut n = 0;
                let mut j = 1;
                while j < end.saturating_sub(1) {
                    let t = tail[j];
                    if n == field && t.keyword("default") {
                        return true;
                    }
                    if matches!(t, Token::Symbol(',')) {
                        n += 1;
                    }
                    j = if matches!(t, Token::Symbol('(')) {
                        group_end(tail, j)
                    } else {
                        j + 1
                    };
                }
                if n < field {
                    return true;
                }
                tail = &tail[end..];
                if !matches!(tail.first(), Some(Token::Symbol(','))) {
                    break;
                }
                tail = &tail[1..];
            }
            false
        })
    }

    pub(super) fn projected_inputs(
        &self,
        relation: &str,
        fields: &std::collections::BTreeSet<&str>,
    ) -> Option<std::collections::BTreeSet<String>> {
        let view = direct_view(&self.tokens)?;
        if !view.table.relation_named(relation) {
            return None;
        }
        Some(
            view.fields
                .iter()
                .filter(|(output, _)| fields.iter().any(|f| output.eq_ignore_ascii_case(f)))
                .filter_map(|(_, expression)| match expression[0] {
                    Token::Word(name) if !name.starts_with(|c: char| c.is_ascii_digit()) => {
                        Some(name.to_ascii_lowercase())
                    }
                    _ => None,
                })
                .collect(),
        )
    }

    pub(super) fn reads_whole_row(&self, relation: &str) -> bool {
        self.selects.iter().any(|select| {
            select.bindings.iter().any(|binding| {
                let Some(alias) = binding.alias else {
                    return false;
                };
                self.source_reads(binding, relation, Some(&mut Vec::new())) == Some(true)
                    && select.range.clone().any(|i| {
                        !select
                            .bindings
                            .iter()
                            .any(|source| source.declaration.contains(&i))
                            && alias.same_identifier(self.tokens[i])
                            && !matches!(self.tokens.get(i + 1), Some(Token::Symbol('.')))
                            && !matches!(
                                self.tokens.get(i.wrapping_sub(1)),
                                Some(Token::Symbol('.'))
                            )
                    })
            })
        })
    }

    pub(super) fn column_list(&self, table: &str) -> Option<bool> {
        self.reads_column_list(table, false)
    }

    pub(super) fn physical_column_list(&self, table: &str) -> Option<bool> {
        self.reads_column_list(table, true)
    }

    // Only a standalone select-list star exports columns by name. A star
    // inside a row-valued expression contributes to the value of one column.
    fn exports_star(&self, select: &Select<'_>, position: usize) -> bool {
        if !matches!(self.tokens[position], Token::Symbol('*')) {
            return false;
        }
        let mut i = select.range.start + 1;
        while i < position {
            if self.tokens[i].keyword("from") {
                return false;
            }
            i = if matches!(self.tokens[i], Token::Symbol('(')) {
                group_end(&self.tokens, i)
            } else {
                i + 1
            };
        }
        i == position
            && (i + 1 == select.range.end
                || self
                    .tokens
                    .get(i + 1)
                    .is_some_and(|t| t.keyword("from") || matches!(t, Token::Symbol(','))))
    }

    fn uses_ordinal(&self, select: &Select<'_>) -> bool {
        let mut has_by = false;
        let mut i = select.range.start + 1;
        while i < select.range.end {
            let token = self.tokens[i];
            if token.keyword("by") {
                has_by = true;
            } else if has_by
                && (self.tokens[i - 1].keyword("by")
                    || matches!(self.tokens[i - 1], Token::Symbol(',')))
            {
                let term = self.tokens[i..select.range.end]
                    .iter()
                    .find(|t| !matches!(t, Token::Symbol('(')));
                if matches!(term, Some(Token::Word(n)) if n.bytes().all(|b| b.is_ascii_digit())) {
                    return true;
                }
            }
            i = if matches!(token, Token::Symbol('(')) {
                group_end(&self.tokens, i)
            } else {
                i + 1
            };
        }
        false
    }

    // A star in a derived table supplies its shape to its consumers. Named
    // references are already resolved by column(). Keep the whole shape for
    // positional consumers, whole-row expressions, and set comparisons.
    fn only_named_consumers(&self, select: &Select<'_>) -> bool {
        let body = &select.range;
        if self
            .tokens
            .get(body.start + 1)
            .is_some_and(|t| t.keyword("distinct"))
            || self.uses_ordinal(select)
            || self
                .ctes
                .iter()
                .any(|cte| cte.body == *body && cte.positional)
        {
            return false;
        }
        let mut consumed = false;
        for parent in &self.selects {
            for binding in &parent.bindings {
                if !matches!(&binding.source, Source::Query(query) if query == body) {
                    continue;
                }
                consumed = true;
                if self.uses_ordinal(parent)
                    || matches!(
                        self.tokens.get(binding.declaration.end),
                        Some(Token::Symbol('('))
                    )
                {
                    return false;
                }
                for i in parent
                    .range
                    .clone()
                    .filter(|i| !binding.declaration.contains(i))
                {
                    let token = self.tokens[i];
                    if token.keyword("natural") {
                        return false;
                    }
                    let alias = |t: Token<'_>| binding.alias.is_some_and(|a| a.same_identifier(t));
                    if alias(token) && !matches!(self.tokens.get(i + 1), Some(Token::Symbol('.'))) {
                        return false;
                    }
                    if matches!(token, Token::Symbol('*')) {
                        let qualified = i >= 2 && matches!(self.tokens[i - 1], Token::Symbol('.'));
                        let exported = self.exports_star(parent, i);
                        let uses_binding = if qualified {
                            alias(self.tokens[i - 2])
                        } else {
                            exported
                        };
                        if uses_binding
                            && !(exported && self.named_projections.contains(&parent.range.start))
                        {
                            return false;
                        }
                    }
                }
            }
        }
        consumed
    }

    fn reads_column_list(&self, table: &str, physical: bool) -> Option<bool> {
        let mut result = Some(false);
        for (i, t) in self.tokens.iter().enumerate() {
            if !physical
                && matches!(t, Token::Symbol('*'))
                && self.scopes(i).next().is_some_and(|scope| {
                    let start = scope.range.start;
                    self.named_projections.contains(&start) && self.exports_star(scope, i)
                        || start >= 2
                            && self.tokens[start - 2].keyword("exists")
                            && matches!(self.tokens[start - 1], Token::Symbol('('))
                            && !self.tokens.get(scope.range.end).is_some_and(|kw| {
                                ["union", "except", "intersect"]
                                    .iter()
                                    .any(|w| kw.keyword(w))
                            })
                })
            {
                continue;
            }
            if self
                .insert_tail(i, table)
                .is_some_and(|tail| !matches!(tail.first(), Some(Token::Symbol('('))))
            {
                return Some(true);
            }
            let reads = if t.keyword("natural") {
                self.scopes(i)
                    .next()
                    .and_then(|s| self.any_source_reads(&s.bindings, table, None))
            } else {
                self.star_reads(i, table, None)
            };
            match reads {
                Some(true) => return Some(true),
                None => result = None,
                Some(false) => {}
            }
        }
        result
    }

    fn is_ordered_output(&self, i: usize, name: &str) -> bool {
        let Some(scope) = self.scopes(i).next() else {
            return false;
        };
        let mut j = scope.range.start + 1;
        let mut projection = true;
        let mut alias = false;
        let mut order = false;
        while j < i {
            let token = self.tokens[j];
            if token.keyword("from") {
                projection = false;
            }
            if projection
                && token.keyword("as")
                && self.tokens.get(j + 1).is_some_and(|t| t.named(name))
            {
                alias = true;
            }
            if token.keyword("order") && self.tokens.get(j + 1).is_some_and(|t| t.keyword("by")) {
                order = true;
            }
            if ["limit", "offset", "fetch"]
                .iter()
                .any(|w| token.keyword(w))
            {
                order = false;
            }
            j = if matches!(token, Token::Symbol('(')) {
                group_end(&self.tokens, j)
            } else {
                j + 1
            };
        }
        alias
            && order
            && j == i
            && (self.tokens[i - 1].keyword("by")
                || matches!(self.tokens[i - 1], Token::Symbol(',')))
            && self.tokens.get(i + 1).is_none_or(|t| {
                matches!(t, Token::Symbol(',' | ')' | ';'))
                    || ["asc", "desc", "nulls", "limit", "offset", "fetch"]
                        .iter()
                        .any(|w| t.keyword(w))
            })
    }

    fn unqualified_column(
        &self,
        position: usize,
        table: &str,
        name: &str,
        state: &super::ProjectState,
    ) -> Option<bool> {
        let mut in_query = false;
        for scope in self.scopes(position) {
            in_query = true;
            let mut local = false;
            for binding in &scope.bindings {
                if self.source_reads(binding, table, Some(&mut Vec::new())) == Some(true) {
                    return Some(true);
                }
                if let Source::Relation(token) = binding.source {
                    local |= state.tables().iter().any(|t| {
                        token.relation_named(&t.name)
                            && t.columns
                                .iter()
                                .any(|c| super::catalog::same_name(&c.name, name))
                    });
                }
            }
            // Correlated references fall through to the enclosing SELECT only
            // when the current query has no column with this name.
            if local {
                return Some(false);
            }
        }
        if in_query { None } else { Some(true) }
    }

    pub(super) fn column(
        &self,
        table: &str,
        name: &str,
        state: &super::ProjectState,
    ) -> Option<bool> {
        let mut result = Some(false);
        for (i, token) in self.tokens.iter().enumerate() {
            if !token.named(name) || i > 0 && self.tokens[i - 1].keyword("as") {
                continue;
            }
            if matches!(self.tokens.get(i + 1), Some(Token::Symbol('.'))) {
                continue;
            }
            let binding = if i >= 2 && matches!(self.tokens[i - 1], Token::Symbol('.')) {
                self.qualified(i - 2, table, Some(&mut Vec::new()))
            } else {
                if self.is_ordered_output(i, name) {
                    continue;
                }
                self.unqualified_column(i, table, name, state)
            };
            match binding {
                Some(true) => return Some(true),
                None => result = None,
                Some(false) => {}
            }
        }
        result
    }
}

// PostgreSQL's TABLE relation query is equivalent to SELECT * FROM relation.
// CREATE TABLE and a quoted identifier named "table" are not query expressions.
fn table_query(tokens: &[Token<'_>], i: usize) -> bool {
    cfg!(feature = "postgres")
        && tokens[i].keyword("table")
        && i > 0
        && (matches!(tokens[i - 1], Token::Symbol('(' | ')'))
            || ["as", "union", "all", "distinct", "except", "intersect"]
                .iter()
                .any(|w| tokens[i - 1].keyword(w)))
}

fn select_sources<'a>(tokens: &[Token<'a>], start: usize, ctes: &[Cte<'a>]) -> Select<'a> {
    let mut bindings = Vec::new();
    let grouped = matches!(tokens[start], Token::Symbol('('));
    let mut in_from = grouped;
    let mut i = if grouped { start } else { start + 1 };
    while i < tokens.len() {
        let token = tokens[i];
        if matches!(token, Token::Symbol(')' | ';'))
            || ["union", "intersect", "except"]
                .iter()
                .any(|w| token.keyword(w))
        {
            break;
        }
        if i != start && matches!(token, Token::Symbol('(')) {
            i = group_end(tokens, i);
            continue;
        }
        i += 1;
        if [
            "where",
            "group",
            "having",
            "order",
            "limit",
            "window",
            "returning",
            "set",
        ]
        .iter()
        .any(|w| token.keyword(w))
        {
            in_from = false;
        }
        if !(grouped && i == start + 1
            || token.keyword("from")
            || token.keyword("join")
            || in_from && matches!(token, Token::Symbol(',')))
        {
            continue;
        }
        in_from = true;
        if cfg!(feature = "postgres")
            && tokens
                .get(i)
                .is_some_and(|t| t.keyword("lateral") || t.keyword("only"))
        {
            i += 1;
        }
        let start_relation = i;
        let Some(&relation) = tokens.get(i) else {
            break;
        };
        if matches!(relation, Token::Symbol('(')) {
            i = group_end(tokens, i);
            let body = start_relation + 1..i.saturating_sub(1);
            let query = tokens.get(body.start).is_some_and(|t| {
                ["select", "with", "values", "table"]
                    .iter()
                    .any(|w| t.keyword(w))
            });
            let alias = relation_alias(&tokens[i..]);
            let declaration = start_relation
                ..i + usize::from(alias.is_some())
                    + usize::from(tokens.get(i).is_some_and(|t| t.keyword("as")));
            if query {
                bindings.push(Binding {
                    alias,
                    declaration,
                    source: Source::Query(body),
                });
            } else {
                let members = select_sources(tokens, start_relation, ctes).bindings;
                if alias.is_some() {
                    bindings.push(Binding {
                        alias,
                        declaration,
                        source: Source::Group(members),
                    });
                } else {
                    bindings.extend(members);
                }
            }
        } else {
            while matches!(tokens.get(i + 1), Some(Token::Symbol('.'))) && i + 2 < tokens.len() {
                i += 2;
            }
            let relation = tokens[i];
            let cte = ctes.iter().rev().find(|c| {
                start_relation == i && c.name.same_identifier(relation) && c.scope.contains(&i)
            });
            i += 1;
            let source = if matches!(tokens.get(i), Some(Token::Symbol('('))) {
                i = group_end(tokens, i);
                Source::Unknown
            } else if let Some(cte) = cte {
                Source::Query(cte.body.clone())
            } else {
                Source::Relation(relation)
            };
            let alias = relation_alias(&tokens[i..]);
            bindings.push(Binding {
                alias: alias.or(Some(relation)),
                declaration: start_relation
                    ..i + usize::from(alias.is_some())
                        + usize::from(tokens.get(i).is_some_and(|t| t.keyword("as"))),
                source,
            });
        }
    }
    Select {
        range: start..i,
        bindings,
    }
}

pub(super) fn may_read_column(
    object: &SchemaObjectState,
    table: &str,
    name: &str,
    state: &super::ProjectState,
) -> bool {
    SqlReads::new(object)
        .column(table, name, state)
        .unwrap_or(true)
}

fn relation_alias<'a>(rest: &[Token<'a>]) -> Option<Token<'a>> {
    if rest.first().is_some_and(|t| t.keyword("as")) {
        return rest.get(1).copied();
    }
    rest.first().copied().filter(|t| {
        matches!(t, Token::Word(_) | Token::Quoted(_, _))
            && ![
                "on",
                "using",
                "join",
                "left",
                "right",
                "full",
                "inner",
                "outer",
                "cross",
                "natural",
                "where",
                "group",
                "having",
                "order",
                "limit",
                "window",
                "union",
                "except",
                "intersect",
                "indexed",
                "not",
                "offset",
                "fetch",
                "for",
                "tablesample",
            ]
            .iter()
            .any(|w| t.keyword(w))
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
struct Cte<'a> {
    positional: bool,
    name: Token<'a>,
    scope: std::ops::Range<usize>,
    body: std::ops::Range<usize>,
}

fn cte_bindings<'a>(tokens: &[Token<'a>]) -> Vec<Cte<'a>> {
    let mut scopes = Vec::new();
    for (start, _) in tokens.iter().enumerate().filter(|(_, t)| t.keyword("with")) {
        let recursive = tokens
            .get(start + 1)
            .is_some_and(|t| t.keyword("recursive"));
        let mut i = start + 1 + usize::from(recursive);
        while let Some(binding) = tokens.get(i) {
            i += 1;
            let renamed_columns = matches!(tokens.get(i), Some(Token::Symbol('(')));
            if renamed_columns {
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
            let body_start = i + 1;
            i = group_end(tokens, i);
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
            scopes.push(Cte {
                positional: renamed_columns || recursive,
                name: *binding,
                scope: visible..end,
                body: body_start..i.saturating_sub(1),
            });
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
            return !shadowed && head.relation_named(name);
        }
    }
    false
}

pub(super) fn references_foreign_key(sql: &str, name: &str) -> bool {
    let tokens = tokens(sql);
    tokens.iter().enumerate().any(|(index, token)| {
        token.keyword("references") && names_relation(&tokens[index + 1..], name, false, false)
    })
}

pub(super) fn references(object: &SchemaObjectState, name: &str) -> bool {
    let tokens = tokens(&object.sql);
    references_tokens(&tokens, object.kind, name, &cte_bindings(&tokens))
}

fn references_tokens(
    tokens: &[Token<'_>],
    kind: SchemaObjectKind,
    name: &str,
    ctes: &[Cte<'_>],
) -> bool {
    let mut group_start = false;
    let mut trigger_header = kind == SchemaObjectKind::Trigger;
    let mut in_from = false;
    let mut parents = Vec::new();
    for (i, &token) in tokens.iter().enumerate() {
        let tail = &tokens[i + 1..];
        let from = token.keyword("from") || token.keyword("join") || table_query(tokens, i);
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
                source
                    && ctes
                        .iter()
                        .any(|c| c.name.named(name) && c.scope.contains(&i)),
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
