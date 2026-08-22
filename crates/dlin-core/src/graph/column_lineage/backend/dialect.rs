#![allow(dead_code)]

use std::fmt;
use std::str::FromStr;

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

    pub fn to_polyglot(self) -> polyglot_sql::DialectType {
        match self {
            Self::Generic => polyglot_sql::DialectType::Generic,
            Self::PostgreSQL => polyglot_sql::DialectType::PostgreSQL,
            Self::MySQL => polyglot_sql::DialectType::MySQL,
            Self::Hive => polyglot_sql::DialectType::Hive,
            Self::Databricks => polyglot_sql::DialectType::Databricks,
            Self::Snowflake => polyglot_sql::DialectType::Snowflake,
            Self::BigQuery => polyglot_sql::DialectType::BigQuery,
            Self::DuckDB => polyglot_sql::DialectType::DuckDB,
            Self::SQLite => polyglot_sql::DialectType::SQLite,
            Self::Spark => polyglot_sql::DialectType::Spark,
            Self::Trino => polyglot_sql::DialectType::Trino,
            Self::Presto => polyglot_sql::DialectType::Presto,
            Self::Redshift => polyglot_sql::DialectType::Redshift,
            Self::TSQL => polyglot_sql::DialectType::TSQL,
            Self::Oracle => polyglot_sql::DialectType::Oracle,
            Self::ClickHouse => polyglot_sql::DialectType::ClickHouse,
            Self::Athena => polyglot_sql::DialectType::Athena,
            Self::Teradata => polyglot_sql::DialectType::Teradata,
            Self::Doris => polyglot_sql::DialectType::Doris,
            Self::StarRocks => polyglot_sql::DialectType::StarRocks,
            Self::Materialize => polyglot_sql::DialectType::Materialize,
            Self::RisingWave => polyglot_sql::DialectType::RisingWave,
            Self::SingleStore => polyglot_sql::DialectType::SingleStore,
            Self::CockroachDB => polyglot_sql::DialectType::CockroachDB,
            Self::TiDB => polyglot_sql::DialectType::TiDB,
            Self::Druid => polyglot_sql::DialectType::Druid,
            Self::Solr => polyglot_sql::DialectType::Solr,
            Self::Tableau => polyglot_sql::DialectType::Tableau,
            Self::Dune => polyglot_sql::DialectType::Dune,
            Self::Fabric => polyglot_sql::DialectType::Fabric,
            Self::Drill => polyglot_sql::DialectType::Drill,
            Self::Dremio => polyglot_sql::DialectType::Dremio,
            Self::Exasol => polyglot_sql::DialectType::Exasol,
            Self::DataFusion => polyglot_sql::DialectType::DataFusion,
        }
    }

    #[cfg(feature = "column-lineage")]
    pub fn to_sqllineage(self) -> Result<sqllineage::Dialect, super::BackendError> {
        match self {
            Self::Generic => Ok(sqllineage::Dialect::Generic),
            Self::PostgreSQL => Ok(sqllineage::Dialect::PostgreSql),
            Self::MySQL => Ok(sqllineage::Dialect::MySql),
            Self::Hive => Ok(sqllineage::Dialect::Hive),
            Self::Databricks => Ok(sqllineage::Dialect::Databricks),
            Self::Snowflake => Ok(sqllineage::Dialect::Snowflake),
            Self::BigQuery => Ok(sqllineage::Dialect::BigQuery),
            Self::DuckDB
            | Self::SQLite
            | Self::Spark
            | Self::Trino
            | Self::Presto
            | Self::Redshift
            | Self::TSQL
            | Self::Oracle
            | Self::ClickHouse
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
            assert_eq!(dialect.to_polyglot(), variant.to_polyglot());
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

        let error = DlinDialect::DuckDB.to_sqllineage().unwrap_err();
        assert_eq!(
            error.kind,
            super::super::BackendErrorKind::UnsupportedDialect
        );
        assert!(error.message.contains("duckdb"));
    }
}
