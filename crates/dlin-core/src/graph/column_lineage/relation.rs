#![allow(dead_code)]

use std::fmt;

use serde::{Deserialize, Serialize};

use super::backend::DlinDialect;

/// How an identifier was quoted at the point where it was constructed.
///
/// `Unknown` is used for manifest identifiers. A dbt manifest gives us the
/// resolved spelling, but not whether the adapter rendered that component as
/// quoted SQL. Keeping that uncertainty explicit prevents it from being
/// mistaken for an unquoted identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum QuoteStyle {
    Unquoted,
    Double,
    Backtick,
    Unknown,
}

impl QuoteStyle {
    fn is_quoted(self) -> bool {
        matches!(self, Self::Double | Self::Backtick)
    }
}

/// One component of a relation identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Identifier {
    value: String,
    quote_style: QuoteStyle,
}

impl Identifier {
    pub(crate) fn manifest(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            quote_style: QuoteStyle::Unknown,
        }
    }

    pub(crate) fn unquoted(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            quote_style: QuoteStyle::Unquoted,
        }
    }

    pub(crate) fn quoted(value: impl Into<String>, quote_style: QuoteStyle) -> Self {
        debug_assert!(quote_style.is_quoted());
        Self {
            value: value.into(),
            quote_style,
        }
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn quote_style(&self) -> QuoteStyle {
        self.quote_style
    }

    fn matches(&self, other: &Self, dialect: DlinDialect) -> bool {
        match (self.quote_style, other.quote_style) {
            (left, right) if left.is_quoted() && right.is_quoted() => self.value == other.value,
            (left, right) if left.is_quoted() || right.is_quoted() => {
                // Unknown manifest quoting can still be compared to a quoted
                // SQL identifier by exact spelling. A known unquoted side is
                // rejected because its case-folded identity is different.
                let unknown_manifest =
                    matches!(left, QuoteStyle::Unknown) || matches!(right, QuoteStyle::Unknown);
                unknown_manifest && self.value == other.value
            }
            _ => dialect
                .identifier_case_policy()
                .equivalent(&self.value, &other.value),
        }
    }

    fn render(&self) -> String {
        match self.quote_style {
            QuoteStyle::Double => format!("\"{}\"", self.value.replace('"', "\"\"")),
            QuoteStyle::Backtick => format!("`{}`", self.value.replace('`', "``")),
            QuoteStyle::Unquoted | QuoteStyle::Unknown => self.value.clone(),
        }
    }
}

/// A structurally represented relation, from bare table name to
/// catalog.schema.table.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct RelationRef {
    catalog: Option<Identifier>,
    schema: Option<Identifier>,
    name: Identifier,
}

impl RelationRef {
    pub(crate) fn from_manifest(catalog: Option<&str>, schema: Option<&str>, name: &str) -> Self {
        Self {
            catalog: catalog.map(Identifier::manifest),
            schema: schema.map(Identifier::manifest),
            name: Identifier::manifest(name),
        }
    }

    /// Construct a relation from a backend table reference. Backends expose
    /// component values but not their original quote tokens, so the spelling
    /// is represented as unquoted SQL.
    pub(crate) fn from_backend(catalog: Option<&str>, schema: Option<&str>, name: &str) -> Self {
        Self {
            catalog: catalog.map(Identifier::unquoted),
            schema: schema.map(Identifier::unquoted),
            name: Identifier::unquoted(name),
        }
    }

    pub(crate) fn from_backend_lookup(
        catalog: Option<&str>,
        schema: Option<&str>,
        name: &str,
    ) -> Self {
        Self {
            catalog: catalog.map(|value| Identifier::unquoted(value.to_lowercase())),
            schema: schema.map(|value| Identifier::unquoted(value.to_lowercase())),
            name: Identifier::unquoted(name.to_lowercase()),
        }
    }

    pub(crate) fn bare(name: impl Into<String>) -> Self {
        Self {
            catalog: None,
            schema: None,
            name: Identifier::unquoted(name),
        }
    }

    pub(crate) fn as_manifest(&self) -> Self {
        Self {
            catalog: self
                .catalog
                .as_ref()
                .map(|component| Identifier::manifest(component.value.clone())),
            schema: self
                .schema
                .as_ref()
                .map(|component| Identifier::manifest(component.value.clone())),
            name: Identifier::manifest(self.name.value.clone()),
        }
    }

    pub(crate) fn parse(input: &str) -> Result<Self, RelationParseError> {
        let parts = split_relation(input)?;
        let identifiers = parts
            .into_iter()
            .map(|part| match part.quote_style {
                QuoteStyle::Unquoted => Identifier::unquoted(part.value),
                style => Identifier::quoted(part.value, style),
            })
            .collect::<Vec<_>>();

        match identifiers.as_slice() {
            [name] => Ok(Self {
                catalog: None,
                schema: None,
                name: name.clone(),
            }),
            [schema, name] => Ok(Self {
                catalog: None,
                schema: Some(schema.clone()),
                name: name.clone(),
            }),
            [catalog, schema, name] => Ok(Self {
                catalog: Some(catalog.clone()),
                schema: Some(schema.clone()),
                name: name.clone(),
            }),
            _ => Err(RelationParseError::TooManyComponents),
        }
    }

    pub(crate) fn catalog(&self) -> Option<&Identifier> {
        self.catalog.as_ref()
    }

    pub(crate) fn schema(&self) -> Option<&Identifier> {
        self.schema.as_ref()
    }

    pub(crate) fn name(&self) -> &Identifier {
        &self.name
    }

    pub(crate) fn qualification_len(&self) -> usize {
        usize::from(self.catalog.is_some()) + usize::from(self.schema.is_some()) + 1
    }

    /// Compare two relations without allowing a qualified suffix to match a
    /// differently qualified relation. A bare relation is intentionally the
    /// only form that can match a qualified relation by name; callers that
    /// have multiple candidates must use [`resolve_unique`] instead.
    pub(crate) fn matches(&self, other: &Self, dialect: DlinDialect) -> bool {
        if self.qualification_len() > 1
            && other.qualification_len() > 1
            && self.qualification_len() != other.qualification_len()
        {
            return false;
        }

        if self.qualification_len() == 1 || other.qualification_len() == 1 {
            return self.name.matches(&other.name, dialect);
        }

        let catalog_matches = match (self.catalog.as_ref(), other.catalog.as_ref()) {
            (Some(left), Some(right)) => left.matches(right, dialect),
            (None, None) => true,
            _ => false,
        };
        let schema_matches = match (self.schema.as_ref(), other.schema.as_ref()) {
            (Some(left), Some(right)) => left.matches(right, dialect),
            (None, None) => true,
            _ => false,
        };
        catalog_matches && schema_matches && self.name.matches(&other.name, dialect)
    }

    pub(crate) fn render(&self) -> String {
        let mut parts = Vec::with_capacity(self.qualification_len());
        if let Some(catalog) = &self.catalog {
            parts.push(catalog.render());
        }
        if let Some(schema) = &self.schema {
            parts.push(schema.render());
        }
        parts.push(self.name.render());
        parts.join(".")
    }
}

impl fmt::Display for RelationRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

/// Result of resolving a query against a candidate set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelationResolution {
    Unique(usize),
    Ambiguous,
    NotFound,
}

/// Resolve a relation only when exactly one candidate matches it.
pub(crate) fn resolve_unique(
    query: &RelationRef,
    candidates: &[RelationRef],
    dialect: DlinDialect,
) -> RelationResolution {
    let mut matching = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| query.matches(candidate, dialect).then_some(index));
    let Some(first) = matching.next() else {
        return RelationResolution::NotFound;
    };
    if matching.next().is_some() {
        RelationResolution::Ambiguous
    } else {
        RelationResolution::Unique(first)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentifierCasePolicy {
    Preserve,
    Lower,
    Upper,
}

impl IdentifierCasePolicy {
    fn equivalent(self, left: &str, right: &str) -> bool {
        match self {
            Self::Preserve => left == right,
            Self::Lower => left.to_lowercase() == right.to_lowercase(),
            Self::Upper => left.to_uppercase() == right.to_uppercase(),
        }
    }
}

impl DlinDialect {
    pub(crate) fn identifier_case_policy(self) -> IdentifierCasePolicy {
        match self {
            // These are the dialects whose unquoted relation spelling has a
            // stable, well-known fold. The conservative Preserve default is
            // deliberate for dialects whose warehouse/server settings can
            // change identifier behavior.
            Self::Snowflake | Self::Oracle => IdentifierCasePolicy::Upper,
            Self::PostgreSQL | Self::Redshift => IdentifierCasePolicy::Lower,
            _ => IdentifierCasePolicy::Preserve,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelationParseError {
    EmptyInput,
    EmptyComponent,
    UnterminatedQuote,
    UnexpectedCharacterAfterQuote,
    TooManyComponents,
}

impl fmt::Display for RelationParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyInput => "relation is empty",
            Self::EmptyComponent => "relation contains an empty component",
            Self::UnterminatedQuote => "relation contains an unterminated quoted identifier",
            Self::UnexpectedCharacterAfterQuote => {
                "relation contains characters after a quoted identifier"
            }
            Self::TooManyComponents => "relation has more than three components",
        };
        f.write_str(message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedComponent {
    value: String,
    quote_style: QuoteStyle,
}

fn split_relation(input: &str) -> Result<Vec<ParsedComponent>, RelationParseError> {
    if input.is_empty() {
        return Err(RelationParseError::EmptyInput);
    }

    let mut components = Vec::new();
    let mut chars = input.chars().peekable();
    loop {
        let Some(first) = chars.peek().copied() else {
            return Err(RelationParseError::EmptyComponent);
        };
        let (value, quote_style) = match first {
            '"' | '`' => parse_quoted_component(&mut chars, first)?,
            _ => {
                let mut value = String::new();
                while let Some(character) = chars.peek().copied() {
                    if character == '.' {
                        break;
                    }
                    if character == '"' || character == '`' {
                        return Err(RelationParseError::UnexpectedCharacterAfterQuote);
                    }
                    value.push(character);
                    chars.next();
                }
                if value.is_empty() {
                    return Err(RelationParseError::EmptyComponent);
                }
                (value, QuoteStyle::Unquoted)
            }
        };

        if value.is_empty() {
            return Err(RelationParseError::EmptyComponent);
        }
        components.push(ParsedComponent { value, quote_style });

        match chars.next() {
            None => return Ok(components),
            Some('.') => {
                if chars.peek().is_none() {
                    return Err(RelationParseError::EmptyComponent);
                }
            }
            Some(_) => return Err(RelationParseError::UnexpectedCharacterAfterQuote),
        }
    }
}

fn parse_quoted_component<I>(
    chars: &mut std::iter::Peekable<I>,
    delimiter: char,
) -> Result<(String, QuoteStyle), RelationParseError>
where
    I: Iterator<Item = char>,
{
    chars.next();
    let mut value = String::new();
    loop {
        let Some(character) = chars.next() else {
            return Err(RelationParseError::UnterminatedQuote);
        };
        if character != delimiter {
            value.push(character);
            continue;
        }
        if chars.peek() == Some(&delimiter) {
            value.push(delimiter);
            chars.next();
            continue;
        }
        break;
    }

    if chars.peek().is_some_and(|character| *character != '.') {
        return Err(RelationParseError::UnexpectedCharacterAfterQuote);
    }
    let style = match delimiter {
        '"' => QuoteStyle::Double,
        '`' => QuoteStyle::Backtick,
        _ => unreachable!("parser only accepts SQL identifier quote delimiters"),
    };
    Ok((value, style))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(input: &str) -> RelationRef {
        RelationRef::parse(input).expect("relation should parse")
    }

    #[test]
    fn parse_preserves_dots_inside_quoted_components() {
        let relation = parsed("\"foo.bar\".\"orders\"");
        assert_eq!(relation.qualification_len(), 2);
        assert_eq!(relation.schema().unwrap().value(), "foo.bar");
        assert_eq!(relation.name().value(), "orders");
        assert_eq!(relation.to_string(), "\"foo.bar\".\"orders\"");

        assert!(
            !parsed("\"foo.bar\".\"orders\"")
                .matches(&parsed("\"foo\".\"bar\".\"orders\""), DlinDialect::Generic)
        );
    }

    #[test]
    fn parse_unescapes_and_renders_escaped_quotes() {
        let double = parsed("\"foo\"\"bar\".\"orders\"");
        assert_eq!(double.schema().unwrap().value(), "foo\"bar");
        assert_eq!(double.to_string(), "\"foo\"\"bar\".\"orders\"");

        let backtick = parsed("`foo``bar`.`orders`");
        assert_eq!(backtick.schema().unwrap().value(), "foo`bar");
        assert_eq!(backtick.to_string(), "`foo``bar`.`orders`");
    }

    #[test]
    fn manifest_alias_with_dot_is_one_name_component() {
        let relation = RelationRef::from_manifest(Some("warehouse"), Some("raw"), "orders.v2");
        assert_eq!(relation.qualification_len(), 3);
        assert_eq!(relation.name().value(), "orders.v2");
        assert_eq!(relation.to_string(), "warehouse.raw.orders.v2");
    }

    #[test]
    fn qualified_relations_require_equal_arity_and_bare_names_are_base_matches() {
        assert!(
            !parsed("warehouse.raw.orders").matches(&parsed("raw.orders"), DlinDialect::Generic)
        );
        assert!(parsed("raw.orders").matches(&parsed("raw.orders"), DlinDialect::Generic));
        assert!(parsed("warehouse.raw.orders").matches(&parsed("orders"), DlinDialect::Generic));
        assert!(
            !parsed("db_a.raw.orders").matches(&parsed("db_b.raw.orders"), DlinDialect::Generic)
        );
    }

    #[test]
    fn bare_name_resolution_rejects_ambiguity() {
        let query = parsed("orders");
        let candidates = vec![parsed("db_a.raw.orders"), parsed("db_b.raw.orders")];
        assert_eq!(
            resolve_unique(&query, &candidates, DlinDialect::Generic),
            RelationResolution::Ambiguous
        );
        assert_eq!(
            resolve_unique(&query, &[parsed("db_a.raw.orders")], DlinDialect::Generic),
            RelationResolution::Unique(0)
        );
    }

    #[test]
    fn case_policy_is_dialect_specific_and_quote_aware() {
        assert!(parsed("RawTable").matches(&parsed("rawtable"), DlinDialect::Snowflake));
        assert!(!parsed("RawTable").matches(&parsed("rawtable"), DlinDialect::BigQuery));
        assert!(parsed("\"RawTable\"").matches(
            &RelationRef::from_manifest(None, None, "RawTable"),
            DlinDialect::BigQuery
        ));
        assert!(!parsed("\"RawTable\"").matches(&parsed("rawtable"), DlinDialect::Snowflake));
    }

    #[test]
    fn parser_rejects_malformed_relations() {
        for input in ["", ".orders", "orders.", "\"orders", "a.b.c.d"] {
            assert!(RelationRef::parse(input).is_err(), "input: {input}");
        }
    }
}
