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
    BigQuery,
    Snowflake,
    DuckDB,
    SQLite,
    Hive,
    #[cfg_attr(feature = "clap", clap(alias = "spark2"))]
    Spark,
    Trino,
    Presto,
    Redshift,
    #[cfg_attr(feature = "clap", clap(alias = "mssql"))]
    #[cfg_attr(feature = "clap", clap(alias = "sqlserver"))]
    TSQL,
    Oracle,
    ClickHouse,
    Databricks,
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
    #[cfg_attr(feature = "clap", clap(alias = "arrow-datafusion"))]
    #[cfg_attr(feature = "clap", clap(alias = "arrow_datafusion"))]
    DataFusion,
}

impl DlinDialect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::PostgreSQL => "postgresql",
            Self::MySQL => "mysql",
            Self::BigQuery => "bigquery",
            Self::Snowflake => "snowflake",
            Self::DuckDB => "duckdb",
            Self::SQLite => "sqlite",
            Self::Hive => "hive",
            Self::Spark => "spark",
            Self::Trino => "trino",
            Self::Presto => "presto",
            Self::Redshift => "redshift",
            Self::TSQL => "tsql",
            Self::Oracle => "oracle",
            Self::ClickHouse => "clickhouse",
            Self::Databricks => "databricks",
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
            Self::BigQuery => polyglot_sql::DialectType::BigQuery,
            Self::Snowflake => polyglot_sql::DialectType::Snowflake,
            Self::DuckDB => polyglot_sql::DialectType::DuckDB,
            Self::SQLite => polyglot_sql::DialectType::SQLite,
            Self::Hive => polyglot_sql::DialectType::Hive,
            Self::Spark => polyglot_sql::DialectType::Spark,
            Self::Trino => polyglot_sql::DialectType::Trino,
            Self::Presto => polyglot_sql::DialectType::Presto,
            Self::Redshift => polyglot_sql::DialectType::Redshift,
            Self::TSQL => polyglot_sql::DialectType::TSQL,
            Self::Oracle => polyglot_sql::DialectType::Oracle,
            Self::ClickHouse => polyglot_sql::DialectType::ClickHouse,
            Self::Databricks => polyglot_sql::DialectType::Databricks,
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
            "bigquery" => Ok(Self::BigQuery),
            "snowflake" => Ok(Self::Snowflake),
            "duckdb" => Ok(Self::DuckDB),
            "sqlite" => Ok(Self::SQLite),
            "hive" => Ok(Self::Hive),
            "spark" | "spark2" => Ok(Self::Spark),
            "trino" => Ok(Self::Trino),
            "presto" => Ok(Self::Presto),
            "redshift" => Ok(Self::Redshift),
            "tsql" | "mssql" | "sqlserver" => Ok(Self::TSQL),
            "oracle" => Ok(Self::Oracle),
            "clickhouse" => Ok(Self::ClickHouse),
            "databricks" => Ok(Self::Databricks),
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
            _ => Err(format!("Unknown dialect: {}", s)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn all_variants() -> Vec<DlinDialect> {
        vec![
            DlinDialect::Generic,
            DlinDialect::PostgreSQL,
            DlinDialect::MySQL,
            DlinDialect::BigQuery,
            DlinDialect::Snowflake,
            DlinDialect::DuckDB,
            DlinDialect::SQLite,
            DlinDialect::Hive,
            DlinDialect::Spark,
            DlinDialect::Trino,
            DlinDialect::Presto,
            DlinDialect::Redshift,
            DlinDialect::TSQL,
            DlinDialect::Oracle,
            DlinDialect::ClickHouse,
            DlinDialect::Databricks,
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
        let aliases: BTreeMap<&str, DlinDialect> = BTreeMap::from([
            ("postgres", DlinDialect::PostgreSQL),
            ("spark2", DlinDialect::Spark),
            ("mssql", DlinDialect::TSQL),
            ("sqlserver", DlinDialect::TSQL),
            ("memsql", DlinDialect::SingleStore),
            ("cockroach", DlinDialect::CockroachDB),
            ("arrow-datafusion", DlinDialect::DataFusion),
            ("arrow_datafusion", DlinDialect::DataFusion),
        ]);
        for (alias, expected) in aliases {
            assert_eq!(
                DlinDialect::from_str(alias).expect("alias parses"),
                expected
            );
        }
    }
}
