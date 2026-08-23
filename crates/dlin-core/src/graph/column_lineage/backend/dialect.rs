#![allow(dead_code)]

use std::fmt;
use std::str::FromStr;

/// Dialects understood by dlin but no longer implemented by the active
/// column-lineage backend.  The first item is the canonical spelling and the
/// second item contains all accepted spellings (including aliases).
///
/// Keep this table next to [`DlinDialect`] so command-line parsing and
/// manifest auto-detection classify exactly the same set of names.
pub const REMOVED_DIALECTS: &[(&str, &[&str])] = &[
    ("presto", &["presto"]),
    ("oracle", &["oracle"]),
    ("athena", &["athena"]),
    ("teradata", &["teradata"]),
    ("doris", &["doris"]),
    ("starrocks", &["starrocks"]),
    ("materialize", &["materialize"]),
    ("risingwave", &["risingwave"]),
    ("singlestore", &["singlestore", "memsql"]),
    ("cockroachdb", &["cockroachdb", "cockroach"]),
    ("tidb", &["tidb"]),
    ("druid", &["druid"]),
    ("solr", &["solr"]),
    ("tableau", &["tableau"]),
    ("dune", &["dune"]),
    ("fabric", &["fabric"]),
    ("drill", &["drill"]),
    ("dremio", &["dremio"]),
    ("exasol", &["exasol"]),
    (
        "datafusion",
        &["datafusion", "arrow-datafusion", "arrow_datafusion"],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialectClassification {
    Supported(DlinDialect),
    Removed(DlinDialect),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "clap",
    derive(clap::ValueEnum),
    clap(rename_all = "lowercase")
)]
#[allow(clippy::upper_case_acronyms)]
pub enum DlinDialect {
    Generic,
    #[cfg_attr(feature = "clap", clap(alias = "postgres"))]
    PostgreSQL,
    MySQL,
    Hive,
    Databricks,
    Snowflake,
    BigQuery,
    DuckDB,
    SQLite,
    #[cfg_attr(feature = "clap", clap(alias = "spark2"))]
    Spark,
    Trino,
    Presto,
    Redshift,
    #[cfg_attr(feature = "clap", clap(alias = "mssql", alias = "sqlserver"))]
    TSQL,
    Oracle,
    ClickHouse,
    Athena,
    Teradata,
    Doris,
    StarRocks,
    Materialize,
    RisingWave,
    #[cfg_attr(feature = "clap", clap(alias = "memsql"))]
    SingleStore,
    #[cfg_attr(feature = "clap", clap(alias = "cockroach"))]
    CockroachDB,
    TiDB,
    Druid,
    Solr,
    Tableau,
    Dune,
    Fabric,
    Drill,
    Dremio,
    Exasol,
    #[cfg_attr(
        feature = "clap",
        clap(alias = "arrow-datafusion", alias = "arrow_datafusion")
    )]
    DataFusion,
}

impl DlinDialect {
    /// Classify a user-provided spelling after validating it against the full
    /// dlin dialect vocabulary.  An unknown spelling is an error; recognized
    /// dialects that the active backend does not implement are explicitly
    /// classified as removed so callers can warn and fall back safely.
    pub fn classify(input: &str) -> Result<DialectClassification, String> {
        let dialect = input.parse::<Self>()?;
        let classification = if matches!(
            dialect,
            Self::Generic
                | Self::PostgreSQL
                | Self::MySQL
                | Self::Hive
                | Self::Databricks
                | Self::Snowflake
                | Self::BigQuery
                | Self::DuckDB
                | Self::SQLite
                | Self::Spark
                | Self::Trino
                | Self::Redshift
                | Self::TSQL
                | Self::ClickHouse
        ) {
            DialectClassification::Supported(dialect)
        } else {
            DialectClassification::Removed(dialect)
        };
        Ok(classification)
    }

    pub fn is_supported_by_column_lineage(self) -> bool {
        matches!(
            self,
            Self::Generic
                | Self::PostgreSQL
                | Self::MySQL
                | Self::Hive
                | Self::Databricks
                | Self::Snowflake
                | Self::BigQuery
                | Self::DuckDB
                | Self::SQLite
                | Self::Spark
                | Self::Trino
                | Self::Redshift
                | Self::TSQL
                | Self::ClickHouse
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::PostgreSQL => "postgresql",
            Self::MySQL => "mysql",
            Self::Hive => "hive",
            Self::Databricks => "databricks",
            Self::Snowflake => "snowflake",
            Self::BigQuery => "bigquery",
            Self::DuckDB => "duckdb",
            Self::SQLite => "sqlite",
            Self::Spark => "spark",
            Self::Trino => "trino",
            Self::Presto => "presto",
            Self::Redshift => "redshift",
            Self::TSQL => "tsql",
            Self::Oracle => "oracle",
            Self::ClickHouse => "clickhouse",
            Self::Athena => "athena",
            Self::Teradata => "teradata",
            Self::Doris => "doris",
            Self::StarRocks => "starrocks",
            Self::Materialize => "materialize",
            Self::RisingWave => "risingwave",
            Self::SingleStore => "singlestore",
            Self::CockroachDB => "cockroachdb",
            Self::TiDB => "tidb",
            Self::Druid => "druid",
            Self::Solr => "solr",
            Self::Tableau => "tableau",
            Self::Dune => "dune",
            Self::Fabric => "fabric",
            Self::Drill => "drill",
            Self::Dremio => "dremio",
            Self::Exasol => "exasol",
            Self::DataFusion => "datafusion",
        }
    }

    #[cfg(feature = "column-lineage")]
    pub(crate) fn to_sqllineage(self) -> Result<sqllineage::Dialect, super::BackendError> {
        match self {
            Self::Generic => Ok(sqllineage::Dialect::Generic),
            Self::PostgreSQL => Ok(sqllineage::Dialect::PostgreSql),
            Self::MySQL => Ok(sqllineage::Dialect::MySql),
            Self::Hive => Ok(sqllineage::Dialect::Hive),
            Self::Databricks => Ok(sqllineage::Dialect::Databricks),
            Self::Snowflake => Ok(sqllineage::Dialect::Snowflake),
            Self::BigQuery => Ok(sqllineage::Dialect::BigQuery),
            Self::DuckDB => Ok(sqllineage::Dialect::DuckDb),
            Self::SQLite => Ok(sqllineage::Dialect::SQLite),
            Self::Spark => Ok(sqllineage::Dialect::Spark),
            Self::Trino => Ok(sqllineage::Dialect::Trino),
            Self::Redshift => Ok(sqllineage::Dialect::Redshift),
            Self::TSQL => Ok(sqllineage::Dialect::MsSql),
            Self::ClickHouse => Ok(sqllineage::Dialect::ClickHouse),
            Self::Presto
            | Self::Oracle
            | Self::Athena
            | Self::Teradata
            | Self::Doris
            | Self::StarRocks
            | Self::Materialize
            | Self::RisingWave
            | Self::SingleStore
            | Self::CockroachDB
            | Self::TiDB
            | Self::Druid
            | Self::Solr
            | Self::Tableau
            | Self::Dune
            | Self::Fabric
            | Self::Drill
            | Self::Dremio
            | Self::Exasol
            | Self::DataFusion => Err(super::BackendError {
                kind: super::BackendErrorKind::UnsupportedDialect,
                message: format!("sqllineage does not support dialect '{self}'"),
            }),
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
            "postgres" | "postgresql" => Ok(Self::PostgreSQL),
            "mysql" => Ok(Self::MySQL),
            "hive" => Ok(Self::Hive),
            "databricks" => Ok(Self::Databricks),
            "snowflake" => Ok(Self::Snowflake),
            "bigquery" => Ok(Self::BigQuery),
            "duckdb" => Ok(Self::DuckDB),
            "sqlite" => Ok(Self::SQLite),
            "spark" | "spark2" => Ok(Self::Spark),
            "trino" => Ok(Self::Trino),
            "presto" => Ok(Self::Presto),
            "redshift" => Ok(Self::Redshift),
            "tsql" | "mssql" | "sqlserver" => Ok(Self::TSQL),
            "oracle" => Ok(Self::Oracle),
            "clickhouse" => Ok(Self::ClickHouse),
            "athena" => Ok(Self::Athena),
            "teradata" => Ok(Self::Teradata),
            "doris" => Ok(Self::Doris),
            "starrocks" => Ok(Self::StarRocks),
            "materialize" => Ok(Self::Materialize),
            "risingwave" => Ok(Self::RisingWave),
            "singlestore" | "memsql" => Ok(Self::SingleStore),
            "cockroachdb" | "cockroach" => Ok(Self::CockroachDB),
            "tidb" => Ok(Self::TiDB),
            "druid" => Ok(Self::Druid),
            "solr" => Ok(Self::Solr),
            "tableau" => Ok(Self::Tableau),
            "dune" => Ok(Self::Dune),
            "fabric" => Ok(Self::Fabric),
            "drill" => Ok(Self::Drill),
            "dremio" => Ok(Self::Dremio),
            "exasol" => Ok(Self::Exasol),
            "datafusion" | "arrow-datafusion" | "arrow_datafusion" => Ok(Self::DataFusion),
            _ => Err(format!(
                "Unknown dialect: {s}. Expected one of: generic, postgresql, postgres, mysql, hive, databricks, snowflake, bigquery, duckdb, sqlite, spark, spark2, trino, presto, redshift, tsql, mssql, sqlserver, oracle, clickhouse, athena, teradata, doris, starrocks, materialize, risingwave, singlestore, memsql, cockroachdb, cockroach, tidb, druid, solr, tableau, dune, fabric, drill, dremio, exasol, datafusion, arrow-datafusion, arrow_datafusion"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_variants() -> [DlinDialect; 34] {
        [
            DlinDialect::Generic,
            DlinDialect::PostgreSQL,
            DlinDialect::MySQL,
            DlinDialect::Hive,
            DlinDialect::Databricks,
            DlinDialect::Snowflake,
            DlinDialect::BigQuery,
            DlinDialect::DuckDB,
            DlinDialect::SQLite,
            DlinDialect::Spark,
            DlinDialect::Trino,
            DlinDialect::Presto,
            DlinDialect::Redshift,
            DlinDialect::TSQL,
            DlinDialect::Oracle,
            DlinDialect::ClickHouse,
            DlinDialect::Athena,
            DlinDialect::Teradata,
            DlinDialect::Doris,
            DlinDialect::StarRocks,
            DlinDialect::Materialize,
            DlinDialect::RisingWave,
            DlinDialect::SingleStore,
            DlinDialect::CockroachDB,
            DlinDialect::TiDB,
            DlinDialect::Druid,
            DlinDialect::Solr,
            DlinDialect::Tableau,
            DlinDialect::Dune,
            DlinDialect::Fabric,
            DlinDialect::Drill,
            DlinDialect::Dremio,
            DlinDialect::Exasol,
            DlinDialect::DataFusion,
        ]
    }

    #[test]
    fn test_dlin_dialect_roundtrip() {
        for dialect in all_variants() {
            let variant = DlinDialect::from_str(dialect.as_str()).expect("known variant parses");
            assert_eq!(variant, dialect);
            assert_eq!(dialect.to_string(), variant.to_string());
        }
    }

    #[test]
    fn test_dlin_dialect_aliases() {
        for (alias, expected) in [
            ("postgres", DlinDialect::PostgreSQL),
            ("spark2", DlinDialect::Spark),
            ("mssql", DlinDialect::TSQL),
            ("sqlserver", DlinDialect::TSQL),
            ("memsql", DlinDialect::SingleStore),
            ("cockroach", DlinDialect::CockroachDB),
            ("arrow-datafusion", DlinDialect::DataFusion),
            ("arrow_datafusion", DlinDialect::DataFusion),
        ] {
            assert_eq!(DlinDialect::from_str(alias).unwrap(), expected);
        }
    }

    #[cfg(feature = "column-lineage")]
    #[test]
    fn test_dlin_dialect_to_sqllineage() {
        assert!(matches!(
            DlinDialect::Generic.to_sqllineage().unwrap(),
            sqllineage::Dialect::Generic
        ));
        assert!(matches!(
            DlinDialect::PostgreSQL.to_sqllineage().unwrap(),
            sqllineage::Dialect::PostgreSql
        ));
        assert!(matches!(
            DlinDialect::MySQL.to_sqllineage().unwrap(),
            sqllineage::Dialect::MySql
        ));
        assert!(matches!(
            DlinDialect::Hive.to_sqllineage().unwrap(),
            sqllineage::Dialect::Hive
        ));
        assert!(matches!(
            DlinDialect::Databricks.to_sqllineage().unwrap(),
            sqllineage::Dialect::Databricks
        ));
        assert!(matches!(
            DlinDialect::Snowflake.to_sqllineage().unwrap(),
            sqllineage::Dialect::Snowflake
        ));
        assert!(matches!(
            DlinDialect::BigQuery.to_sqllineage().unwrap(),
            sqllineage::Dialect::BigQuery
        ));
        assert!(matches!(
            DlinDialect::DuckDB.to_sqllineage().unwrap(),
            sqllineage::Dialect::DuckDb
        ));
        assert!(matches!(
            DlinDialect::SQLite.to_sqllineage().unwrap(),
            sqllineage::Dialect::SQLite
        ));
        assert!(matches!(
            DlinDialect::Spark.to_sqllineage().unwrap(),
            sqllineage::Dialect::Spark
        ));
        assert!(matches!(
            DlinDialect::Trino.to_sqllineage().unwrap(),
            sqllineage::Dialect::Trino
        ));
        assert!(matches!(
            DlinDialect::Redshift.to_sqllineage().unwrap(),
            sqllineage::Dialect::Redshift
        ));
        assert!(matches!(
            DlinDialect::TSQL.to_sqllineage().unwrap(),
            sqllineage::Dialect::MsSql
        ));
        assert!(matches!(
            DlinDialect::ClickHouse.to_sqllineage().unwrap(),
            sqllineage::Dialect::ClickHouse
        ));

        let error = DlinDialect::Presto.to_sqllineage().unwrap_err();
        assert_eq!(
            error.kind,
            super::super::BackendErrorKind::UnsupportedDialect
        );
        assert!(error.message.contains("presto"));
    }
}
