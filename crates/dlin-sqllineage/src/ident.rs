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
    pub(crate) fn from_ast(ident: &sqlparser::ast::Ident) -> Self {
        let quote = ident.quote_style.map(|style| match style {
            '"' => Quote::Double,
            '`' => Quote::Backtick,
            '[' | ']' => Quote::Bracket,
            '\'' => Quote::Single,
            _ => Quote::Double,
        });
        Self {
            value: ident.value.clone(),
            quote,
        }
    }

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

    /// Return the key used to compare this identifier with another column
    /// name.
    ///
    /// The rule depends on the dialect:
    ///
    /// - Snowflake folds unquoted names to upper case; quoted names are exact.
    /// - PostgreSQL folds unquoted names to lower case; quoted names are exact.
    /// - Generic and ANSI follow the SQL standard: unquoted names fold to upper
    ///   case, so they compare case-insensitively with each other and with a
    ///   quoted name spelled in upper case; quoted names are otherwise exact.
    /// - `BigQuery`, `DuckDB`, MySQL, Hive, Databricks, Spark, Trino, SQLite,
    ///   SQL Server (default collation) and Redshift (default
    ///   `enable_case_sensitive_identifier = false`) compare column names
    ///   case-insensitively whether or not they are quoted. The key is the
    ///   lower-cased spelling.
    /// - `ClickHouse` compares column names exactly.
    ///
    /// Two column names are the same column exactly when their keys are equal.
    /// Use [`Ident::relation_key`] for relation names, which follow different
    /// rules.
    pub fn column_key(&self, dialect: Dialect) -> Cow<'_, str> {
        match dialect {
            Dialect::Snowflake | Dialect::Generic | Dialect::Ansi => self.fold_unquoted_upper(),
            Dialect::PostgreSql => self.fold_unquoted_lower(),
            Dialect::BigQuery
            | Dialect::DuckDb
            | Dialect::MySql
            | Dialect::Hive
            | Dialect::Databricks
            | Dialect::Spark
            | Dialect::Trino
            | Dialect::SQLite
            | Dialect::MsSql
            | Dialect::Redshift => Cow::Owned(self.value.to_lowercase()),
            Dialect::ClickHouse => Cow::Borrowed(&self.value),
        }
    }

    /// Return the key used to compare this identifier with another relation
    /// name part (catalog, schema or table).
    ///
    /// Folding applies to unquoted identifiers only, and only in dialects whose
    /// identifiers are case-insensitive by folding: Snowflake folds to upper
    /// case, PostgreSQL and Redshift fold to lower case. Every other dialect
    /// compares unquoted identifiers exactly as spelled, so `id` and `ID` have
    /// different keys there. Quoted identifiers always retain their exact
    /// spelling.
    ///
    /// Column names use [`Ident::column_key`] instead.
    pub fn relation_key(&self, dialect: Dialect) -> Cow<'_, str> {
        match dialect {
            Dialect::Snowflake => self.fold_unquoted_upper(),
            Dialect::PostgreSql | Dialect::Redshift => self.fold_unquoted_lower(),
            _ => Cow::Borrowed(&self.value),
        }
    }

    fn fold_unquoted_upper(&self) -> Cow<'_, str> {
        if self.quote.is_some() {
            Cow::Borrowed(&self.value)
        } else {
            Cow::Owned(self.value.to_uppercase())
        }
    }

    fn fold_unquoted_lower(&self) -> Cow<'_, str> {
        if self.quote.is_some() {
            Cow::Borrowed(&self.value)
        } else {
            Cow::Owned(self.value.to_lowercase())
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

    fn same_column(dialect: Dialect, a: &Ident, b: &Ident) -> bool {
        a.column_key(dialect) == b.column_key(dialect)
    }

    #[test]
    fn column_key_folds_unquoted_names_in_folding_dialects() {
        let name = Ident::unquoted("MiXeD");
        assert_eq!(name.column_key(Dialect::Snowflake), "MIXED");
        assert_eq!(name.column_key(Dialect::PostgreSql), "mixed");
        assert_eq!(name.column_key(Dialect::Generic), "MIXED");
        assert_eq!(name.column_key(Dialect::Ansi), "MIXED");

        let quoted = Ident::quoted("MiXeD", Quote::Double);
        for dialect in [
            Dialect::Snowflake,
            Dialect::PostgreSql,
            Dialect::Generic,
            Dialect::Ansi,
        ] {
            assert_eq!(quoted.column_key(dialect), "MiXeD");
        }
    }

    #[test]
    fn standard_dialects_match_unquoted_with_upper_case_quoted() {
        for dialect in [Dialect::Generic, Dialect::Ansi, Dialect::Snowflake] {
            assert!(same_column(
                dialect,
                &Ident::unquoted("id"),
                &Ident::quoted("ID", Quote::Double)
            ));
            assert!(same_column(
                dialect,
                &Ident::unquoted("id"),
                &Ident::unquoted("ID")
            ));
            assert!(!same_column(
                dialect,
                &Ident::unquoted("id"),
                &Ident::quoted("id", Quote::Double)
            ));
            assert!(!same_column(
                dialect,
                &Ident::quoted("id", Quote::Double),
                &Ident::quoted("ID", Quote::Double)
            ));
        }
    }

    #[test]
    fn postgres_matches_unquoted_with_lower_case_quoted() {
        let dialect = Dialect::PostgreSql;
        assert!(same_column(
            dialect,
            &Ident::unquoted("ID"),
            &Ident::quoted("id", Quote::Double)
        ));
        assert!(!same_column(
            dialect,
            &Ident::unquoted("id"),
            &Ident::quoted("ID", Quote::Double)
        ));
        assert!(!same_column(
            dialect,
            &Ident::quoted("id", Quote::Double),
            &Ident::quoted("ID", Quote::Double)
        ));
    }

    #[test]
    fn column_names_are_case_insensitive_even_when_quoted() {
        for dialect in [
            Dialect::BigQuery,
            Dialect::DuckDb,
            Dialect::MySql,
            Dialect::Hive,
            Dialect::Databricks,
            Dialect::Spark,
            Dialect::Trino,
            Dialect::SQLite,
            Dialect::MsSql,
            Dialect::Redshift,
        ] {
            let unquoted = Ident::unquoted("Foo");
            let quoted = Ident::quoted("fOO", Quote::Backtick);
            assert_eq!(unquoted.column_key(dialect), "foo", "{dialect:?}");
            assert_eq!(quoted.column_key(dialect), "foo", "{dialect:?}");
            assert!(same_column(dialect, &unquoted, &quoted), "{dialect:?}");
            assert!(
                !same_column(dialect, &unquoted, &Ident::unquoted("bar")),
                "{dialect:?}"
            );
        }
    }

    #[test]
    fn clickhouse_column_names_are_exact() {
        let dialect = Dialect::ClickHouse;
        assert_eq!(Ident::unquoted("Foo").column_key(dialect), "Foo");
        assert_eq!(
            Ident::quoted("Foo", Quote::Backtick).column_key(dialect),
            "Foo"
        );
        assert!(!same_column(
            dialect,
            &Ident::unquoted("Foo"),
            &Ident::unquoted("foo")
        ));
    }

    #[test]
    fn relation_key_uses_dialect_folding_only_for_unquoted_names() {
        let name = Ident::unquoted("MiXeD");
        assert_eq!(name.relation_key(Dialect::Snowflake), "MIXED");
        assert_eq!(name.relation_key(Dialect::PostgreSql), "mixed");
        assert_eq!(name.relation_key(Dialect::Redshift), "mixed");
        assert_eq!(name.relation_key(Dialect::BigQuery), "MiXeD");
        assert_eq!(name.relation_key(Dialect::Generic), "MiXeD");
        assert_eq!(name.relation_key(Dialect::ClickHouse), "MiXeD");

        let quoted = Ident::quoted("MiXeD", Quote::Double);
        for dialect in Dialect::ALL {
            assert_eq!(quoted.relation_key(dialect), "MiXeD");
        }
    }
}
