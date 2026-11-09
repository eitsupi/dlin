//! Column identifiers that keep their spelling and quote style.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::backend::DlinDialect;

/// Quote delimiter an identifier was written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quote {
    /// Double quotes.
    Double,
    /// Backticks.
    Backtick,
    /// SQL Server square brackets.
    Bracket,
    /// Single quotes, where a dialect permits them for identifiers.
    Single,
}

/// A column identifier: its value without the surrounding quote characters,
/// and the quote style it was written with.
///
/// Two identifiers name the same column when their keys are equal under the
/// dialect's rules (see [`Ident::column_key`]). Comparing `value` strings
/// ignores those rules and must not be used to match columns.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Ident {
    /// Spelling as written, without surrounding quotes.
    pub value: String,
    /// Quote style used in the source, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote: Option<Quote>,
}

impl Ident {
    /// An identifier written without quotes.
    pub fn unquoted(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            quote: None,
        }
    }

    /// An identifier written with the given quote style.
    pub fn quoted(value: impl Into<String>, quote: Quote) -> Self {
        Self {
            value: value.into(),
            quote: Some(quote),
        }
    }

    /// Read a column name typed by a user. A name wrapped in double quotes or
    /// backticks is a quoted identifier; anything else is unquoted.
    pub fn parse_reference(input: &str) -> Self {
        for (delimiter, quote) in [('"', Quote::Double), ('`', Quote::Backtick)] {
            if let Some(inner) = input
                .strip_prefix(delimiter)
                .and_then(|rest| rest.strip_suffix(delimiter))
            {
                let doubled = format!("{delimiter}{delimiter}");
                return Self::quoted(inner.replace(&doubled, &delimiter.to_string()), quote);
            }
        }
        Self::unquoted(input)
    }

    pub(crate) fn from_engine(ident: &sqllineage::Ident) -> Self {
        Self {
            value: ident.value.clone(),
            quote: ident.quote.map(|quote| match quote {
                sqllineage::Quote::Double => Quote::Double,
                sqllineage::Quote::Backtick => Quote::Backtick,
                sqllineage::Quote::Bracket => Quote::Bracket,
                sqllineage::Quote::Single => Quote::Single,
            }),
        }
    }

    pub(crate) fn to_engine(&self) -> sqllineage::Ident {
        sqllineage::Ident {
            value: self.value.clone(),
            quote: self.quote.map(|quote| match quote {
                Quote::Double => sqllineage::Quote::Double,
                Quote::Backtick => sqllineage::Quote::Backtick,
                Quote::Bracket => sqllineage::Quote::Bracket,
                Quote::Single => sqllineage::Quote::Single,
            }),
        }
    }

    /// The key under which this identifier is compared with other column
    /// names in `dialect`: two names are the same column exactly when their
    /// keys are equal. The rule is the engine's `column_key`.
    ///
    /// Returns `None` for a dialect the analysis engine does not support.
    pub fn column_key(&self, dialect: DlinDialect) -> Option<String> {
        let engine = dialect.to_engine_dialect().ok()?;
        Some(self.engine_key(engine))
    }

    pub(crate) fn engine_key(&self, dialect: sqllineage::Dialect) -> String {
        self.to_engine().column_key(dialect).into_owned()
    }
}

/// The identifier as it is written in SQL: quoted names keep their quotes.
impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.quote {
            None => f.write_str(&self.value),
            Some(Quote::Double) => write!(f, "\"{}\"", self.value.replace('"', "\"\"")),
            Some(Quote::Backtick) => write!(f, "`{}`", self.value.replace('`', "``")),
            Some(Quote::Bracket) => write!(f, "[{}]", self.value.replace(']', "]]")),
            Some(Quote::Single) => write!(f, "'{}'", self.value.replace('\'', "''")),
        }
    }
}

/// Test convenience: an unquoted identifier equals the text it spells.
#[cfg(test)]
impl PartialEq<&str> for Ident {
    fn eq(&self, other: &&str) -> bool {
        self.quote.is_none() && self.value == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_parsing_recognises_quoted_names() {
        assert_eq!(Ident::parse_reference("id"), Ident::unquoted("id"));
        assert_eq!(
            Ident::parse_reference("\"Id\""),
            Ident::quoted("Id", Quote::Double)
        );
        assert_eq!(
            Ident::parse_reference("`a``b`"),
            Ident::quoted("a`b", Quote::Backtick)
        );
        assert_eq!(Ident::parse_reference("\""), Ident::unquoted("\""));
    }

    #[test]
    fn display_round_trips_the_quote_style() {
        for text in ["id", "\"Id\"", "`Id`"] {
            assert_eq!(Ident::parse_reference(text).to_string(), text);
        }
    }

    #[test]
    fn keys_follow_the_dialect_rules() {
        let unquoted = Ident::unquoted("Id");
        let quoted = Ident::quoted("Id", Quote::Double);
        let key = |ident: &Ident, dialect| ident.column_key(dialect).unwrap();
        assert_eq!(key(&unquoted, DlinDialect::PostgreSQL), "id");
        assert_eq!(key(&quoted, DlinDialect::PostgreSQL), "Id");
        assert_eq!(key(&unquoted, DlinDialect::Snowflake), "ID");
        assert_eq!(key(&quoted, DlinDialect::Snowflake), "Id");
        assert_eq!(key(&unquoted, DlinDialect::BigQuery), "id");
        assert_eq!(key(&quoted, DlinDialect::BigQuery), "id");
        assert_eq!(unquoted.column_key(DlinDialect::Presto), None);
    }
}
