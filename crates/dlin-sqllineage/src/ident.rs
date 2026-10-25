//! Identifier and relation types that preserve SQL quoting information.

use crate::types::Dialect;
use serde::Serialize;
use std::borrow::Cow;

/// An SQL identifier value, without its surrounding quote characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Ident {
    /// Identifier spelling as it appeared in SQL, without surrounding quotes.
    pub value: String,
    /// Quote style used in the source, if any.
    pub quote: Option<Quote>,
}

impl Ident {
    /// Create an unquoted identifier.
    pub fn unquoted(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            quote: None,
        }
    }

    /// Create an identifier with its quote style recorded.
    pub fn quoted(value: impl Into<String>, quote: Quote) -> Self {
        Self {
            value: value.into(),
            quote: Some(quote),
        }
    }

    /// Return the key used to compare this identifier with another.
    ///
    /// Folding applies to unquoted identifiers only, and only in dialects whose
    /// identifiers are case-insensitive by folding: Snowflake folds to upper
    /// case, PostgreSQL and Redshift fold to lower case. Every other dialect
    /// compares unquoted identifiers exactly as spelled, so `id` and `ID` have
    /// different keys there. Quoted identifiers always retain their exact
    /// spelling.
    pub fn key(&self, dialect: Dialect) -> Cow<'_, str> {
        if self.quote.is_some() {
            return Cow::Borrowed(&self.value);
        }
        match dialect {
            Dialect::Snowflake => Cow::Owned(self.value.to_uppercase()),
            Dialect::PostgreSql | Dialect::Redshift => Cow::Owned(self.value.to_lowercase()),
            _ => Cow::Borrowed(&self.value),
        }
    }
}

/// Quote delimiter used for an identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Quote {
    /// Double quotes.
    Double,
    /// Backticks.
    Backtick,
    /// SQL Server square brackets.
    Bracket,
    /// Single quotes, when a dialect permits them for identifiers.
    Single,
}

/// A possibly multi-part SQL relation name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Relation {
    /// Catalog or database component, if present.
    pub catalog: Option<Ident>,
    /// Schema component, if present.
    pub schema: Option<Ident>,
    /// Relation name.
    pub name: Ident,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_uses_dialect_folding_only_for_unquoted_names() {
        let name = Ident::unquoted("MiXeD");
        assert_eq!(name.key(Dialect::Snowflake), "MIXED");
        assert_eq!(name.key(Dialect::PostgreSql), "mixed");
        assert_eq!(name.key(Dialect::Redshift), "mixed");
        assert_eq!(name.key(Dialect::BigQuery), "MiXeD");

        let quoted = Ident::quoted("MiXeD", Quote::Double);
        for dialect in Dialect::ALL {
            assert_eq!(quoted.key(dialect), "MiXeD");
        }
    }
}
