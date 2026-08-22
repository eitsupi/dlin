#![allow(dead_code)]

use std::fmt;
use std::str::FromStr;

const SUPPORTED_DIALECTS: &str =
    "generic, ansi, postgresql, postgres, mysql, hive, databricks, snowflake, bigquery";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "clap",
    derive(clap::ValueEnum),
    clap(rename_all = "lowercase")
)]
#[allow(clippy::upper_case_acronyms)]
pub enum DlinDialect {
    Generic,
    Ansi,
    #[cfg_attr(feature = "clap", clap(alias = "postgres"))]
    PostgreSQL,
    MySQL,
    Hive,
    Databricks,
    Snowflake,
    BigQuery,
}

impl DlinDialect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Ansi => "ansi",
            Self::PostgreSQL => "postgresql",
            Self::MySQL => "mysql",
            Self::Hive => "hive",
            Self::Databricks => "databricks",
            Self::Snowflake => "snowflake",
            Self::BigQuery => "bigquery",
        }
    }

    pub fn to_polyglot(self) -> polyglot_sql::DialectType {
        match self {
            Self::Generic => polyglot_sql::DialectType::Generic,
            // polyglot-sql has no separate ANSI dialect; its generic dialect is
            // the closest equivalent and is deliberately permissive.
            Self::Ansi => polyglot_sql::DialectType::Generic,
            Self::PostgreSQL => polyglot_sql::DialectType::PostgreSQL,
            Self::MySQL => polyglot_sql::DialectType::MySQL,
            Self::Hive => polyglot_sql::DialectType::Hive,
            Self::Databricks => polyglot_sql::DialectType::Databricks,
            Self::Snowflake => polyglot_sql::DialectType::Snowflake,
            Self::BigQuery => polyglot_sql::DialectType::BigQuery,
        }
    }

    #[cfg(feature = "column-lineage")]
    pub fn to_sqllineage(self) -> sqllineage::Dialect {
        match self {
            Self::Generic => sqllineage::Dialect::Generic,
            Self::Ansi => sqllineage::Dialect::Ansi,
            Self::PostgreSQL => sqllineage::Dialect::PostgreSql,
            Self::MySQL => sqllineage::Dialect::MySql,
            Self::Hive => sqllineage::Dialect::Hive,
            Self::Databricks => sqllineage::Dialect::Databricks,
            Self::Snowflake => sqllineage::Dialect::Snowflake,
            Self::BigQuery => sqllineage::Dialect::BigQuery,
        }
    }
}

impl fmt::Display for DlinDialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for DlinDialect {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "generic" | "" => Ok(Self::Generic),
            "ansi" => Ok(Self::Ansi),
            "postgres" | "postgresql" => Ok(Self::PostgreSQL),
            "mysql" => Ok(Self::MySQL),
            "hive" => Ok(Self::Hive),
            "databricks" => Ok(Self::Databricks),
            "snowflake" => Ok(Self::Snowflake),
            "bigquery" => Ok(Self::BigQuery),
            _ => Err(format!(
                "Unknown dialect: {s}. Expected one of: {SUPPORTED_DIALECTS}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_variants() -> [DlinDialect; 8] {
        [
            DlinDialect::Generic,
            DlinDialect::Ansi,
            DlinDialect::PostgreSQL,
            DlinDialect::MySQL,
            DlinDialect::Hive,
            DlinDialect::Databricks,
            DlinDialect::Snowflake,
            DlinDialect::BigQuery,
        ]
    }

    #[test]
    fn test_dlin_dialect_roundtrip() {
        for dialect in all_variants() {
            let variant = DlinDialect::from_str(dialect.as_str()).expect("known variant parses");
            assert_eq!(variant, dialect);
            assert_eq!(dialect.to_polyglot(), variant.to_polyglot());
            assert_eq!(dialect.to_string(), variant.to_string());
        }
    }

    #[test]
    fn test_dlin_dialect_aliases() {
        assert_eq!(
            DlinDialect::from_str("postgres").expect("alias parses"),
            DlinDialect::PostgreSQL
        );
    }

    #[test]
    fn test_removed_dialect_is_rejected() {
        let error = DlinDialect::from_str("duckdb").expect_err("removed dialect must fail");
        assert!(error.contains(SUPPORTED_DIALECTS));
    }

    #[cfg(feature = "column-lineage")]
    #[test]
    fn test_dlin_dialect_to_sqllineage() {
        assert!(matches!(
            DlinDialect::Generic.to_sqllineage(),
            sqllineage::Dialect::Generic
        ));
        assert!(matches!(
            DlinDialect::Ansi.to_sqllineage(),
            sqllineage::Dialect::Ansi
        ));
        assert!(matches!(
            DlinDialect::PostgreSQL.to_sqllineage(),
            sqllineage::Dialect::PostgreSql
        ));
        assert!(matches!(
            DlinDialect::MySQL.to_sqllineage(),
            sqllineage::Dialect::MySql
        ));
        assert!(matches!(
            DlinDialect::Hive.to_sqllineage(),
            sqllineage::Dialect::Hive
        ));
        assert!(matches!(
            DlinDialect::Databricks.to_sqllineage(),
            sqllineage::Dialect::Databricks
        ));
        assert!(matches!(
            DlinDialect::Snowflake.to_sqllineage(),
            sqllineage::Dialect::Snowflake
        ));
        assert!(matches!(
            DlinDialect::BigQuery.to_sqllineage(),
            sqllineage::Dialect::BigQuery
        ));
    }
}
